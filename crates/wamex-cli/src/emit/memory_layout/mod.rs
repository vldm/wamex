use std::{borrow::Cow, collections::BTreeMap, fmt::Debug, io::IsTerminal, iter::Peekable};

use anyhow::{bail, Result};
use gxhash::HashSet;
use wasm_encoder::Encode;
use wasmparser::{Data, DataKind, SymbolFlags};

use crate::{
    analysis::{
        self,
        symbols::{self, SymbolKind},
    },
    emit::index_safety::OutputGlobalId,
    helpers::{encoding_size, RangeComp, RangeExt},
    index::{DataSegmentId, Id, IdMap, IdVec, Indexed, SymbolId},
};
mod hexdump;

/// Describes how a data symbol relates to its neighboring symbols within a segment.
#[derive(Clone, Debug)]
pub enum SymbolRelation<'a> {
    /// A standalone symbol with no binding constraints.
    Regular {
        chunk: &'a [u8],
        // true if data was properly aligned.
        // If data was not aligned, it will not be aligned in output.
        // It can report false-positive. But it is okay to align data on bigger alignment.
        //
        // Linking table does not contain information about symbol alignment.
        // We use size + segment alignment to calculate if data was aligned properly.
        aligned: bool,
    },

    /// A symbol that must stay adjacent to the previous symbol
    /// and cannot be moved or removed independently.
    BoundToPrevious {
        /// minus offset from end of previous symbol to start of this symbol.
        offset: usize,
        len: usize,
    },
}

#[derive(Clone, Debug)]
pub struct DataChunk<'a> {
    name: Cow<'a, str>,
    #[allow(dead_code)]
    flags: SymbolFlags,
    relation: SymbolRelation<'a>,
    symbol_index: SymbolId,

    // offset related to this symbol
    relocations: Vec<wasmparser::RelocationEntry>,
}

impl DataChunk<'_> {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn relocations(&self) -> &[wasmparser::RelocationEntry] {
        &self.relocations
    }
    //TODO: Don't expose in public API
    pub fn symbol_relation(&self) -> &SymbolRelation<'_> {
        &self.relation
    }
}

#[derive(Clone)]
pub struct SegmentLayout<'a> {
    data_parts: Vec<DataChunk<'a>>,
    alignment: usize,
    kind: DataKind<'a>,
    mem_offset: usize,
}

impl Debug for SegmentLayout<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match &self.kind {
            DataKind::Passive => "Passive".to_string(),
            DataKind::Active {
                offset_expr,
                memory_index,
            } => {
                let offset_expr = offset_expr.get_operators_reader().into_iter().fold(
                    String::new(),
                    |mut val, op| {
                        let operator = op.expect("Expected operator in offset expression");
                        if !val.is_empty() {
                            val.push(' ');
                        }
                        val.push_str(&format!("{:?}", operator));
                        val
                    },
                );
                format!("Active(memory:{memory_index}, offset:{})", offset_expr)
            }
        };
        write!(
            f,
            "DataSegment {{ data_parts: {:?}, kind: {} }}",
            self.data_parts, kind
        )
    }
}

impl<'src> SegmentLayout<'src> {
    pub fn new_inner<'a>(
        data: &Data<'src>,
        segment_info: &wasmparser::Segment<'src>,
        data_symbols: impl Iterator<Item = (SymbolId, &'a analysis::symbols::SymbolRecord<'src>)>,
    ) -> Result<SegmentLayout<'src>>
    where
        'src: 'a,
    {
        let alignment = (2usize).pow(segment_info.alignment);

        let mem_offset = match &data.kind {
            DataKind::Passive => panic!("Passive data is not currently supported"),
            DataKind::Active { offset_expr, .. } => {
                crate::analysis::ModuleInfo::read_const_expr(offset_expr)?
            }
        };

        log::debug!("Memory offset is {}", mem_offset);
        log::debug!(
            "Segment alignment is {}, aligned = {}",
            alignment,
            mem_offset % alignment as i32 == 0
        );

        let mut data_parts = vec![];
        let mut last_regular = 0..0;
        for (sym_id, sym) in data_symbols {
            let SymbolKind::DataDefined { offset, length, .. } = &sym.kind else {
                unreachable!("Expected DataDefined symbol kind for data segment symbol");
            };

            let symbol_in_data = offset.clone()..(offset + length);
            let field_alignment = Self::data_symbol_alignment(alignment, symbol_in_data.len());

            let relation = if last_regular.end > symbol_in_data.start {
                log::warn!(
                    "DataSymbol intersects with previous, this is currently in testing: {:?} range:{:?} prev_range: {:?}",
                    sym,
                    symbol_in_data,
                    last_regular
                );
                assert!(matches!(
                    last_regular.cmp_range(&symbol_in_data),
                    RangeComp::Overlap | RangeComp::Equal
                ));
                let offset = last_regular.end - symbol_in_data.start;
                SymbolRelation::BoundToPrevious {
                    offset,
                    len: symbol_in_data.len(),
                }
            } else {
                if last_regular.end < symbol_in_data.start {
                    let gap_range = last_regular.end..symbol_in_data.start;

                    if gap_range.len() >= alignment {
                        log::error!(
                            "Data segment has gap larger than alignment: {:?} > {} before {}",
                            gap_range,
                            alignment,
                            sym.name
                        );
                    } else {
                        log::debug!(
                            "Data segment has gap: {:?} ({} bytes) before {}",
                            gap_range,
                            gap_range.len(),
                            sym.name
                        );
                    }
                }
                let aligned =
                    (symbol_in_data.start + mem_offset as usize).is_multiple_of(field_alignment);

                log::trace!(
                    "Data symbol {}: offset: {}, size: {}, aligned: {}, alignment: {}",
                    sym.name,
                    symbol_in_data.start,
                    symbol_in_data.len(),
                    aligned,
                    field_alignment
                );
                last_regular = symbol_in_data.clone();
                SymbolRelation::Regular {
                    chunk: &data.data[symbol_in_data.clone()],
                    aligned,
                }
            };

            let part = DataChunk {
                name: sym.name.clone(),
                flags: sym.flags,
                relocations: sym.relocs.clone(),
                symbol_index: sym_id,
                relation,
            };
            log::trace!("Data part: {part:?}");
            data_parts.push(part)
        }

        let kind = data.kind.clone();

        Ok(SegmentLayout {
            alignment,
            data_parts,
            kind,
            mem_offset: mem_offset as usize,
        })
    }

    pub fn debug_layout(
        symbol_table: &symbols::SymbolMap,
        module_name: String,
        data_segments: &IdVec<SegmentLayout<'_>>,
    ) {
        use std::fmt::Write;
        let mut print_data_format = String::new();
        writeln!(print_data_format, "<Module {module_name}>").unwrap();

        let mut base = 0;
        for (i, segment) in data_segments.iter() {
            for symbol in segment.data_parts.iter() {
                writeln!(
                    print_data_format,
                    "Data symbol [{i}.{index}]: {name}",
                    i = i,
                    index = symbol.symbol_index,
                    name = symbol.name(),
                )
                .unwrap();
                match symbol.symbol_relation() {
                    SymbolRelation::Regular { chunk, .. } => {
                        let input_symbol = symbol_table.get(symbol.symbol_index).unwrap();
                        let refs = input_symbol
                            .relocs
                            .iter()
                            .map(|reloc| {
                                let reloc_symbol =
                                    symbol_table.get(Id::from_index(reloc.index)).unwrap();
                                hexdump::Ref {
                                    range: reloc.relocation_range(),
                                    name: &reloc_symbol.name,
                                }
                            })
                            .collect();
                        let part = hexdump::DataPart {
                            name: symbol.name(),
                            bytes: chunk,
                            refs,
                        };
                        hexdump::render_part(
                            &mut print_data_format,
                            base,
                            &part,
                            std::io::stderr().is_terminal(),
                        );
                        // TODO: add padding
                        base += chunk.len();
                    }
                    SymbolRelation::BoundToPrevious { .. } => {
                        writeln!(print_data_format, "<bound to previous>").unwrap()
                    }
                };
            }
        }
        log::warn!("Data segments {print_data_format}");
    }
    pub fn memory_offset(&self) -> usize {
        self.mem_offset
    }

    fn collect_and_map_while<'any, I, U>(
        iterator: &mut Peekable<I>,
        map: impl Fn(&'any wasmparser::RelocationEntry) -> U,
        condition: impl Fn(&&'any wasmparser::RelocationEntry) -> bool + Copy,
    ) -> Vec<U>
    where
        I: Iterator<Item = &'any wasmparser::RelocationEntry>,
    {
        let mut result = vec![];
        while let Some(entry) = iterator.next_if(condition) {
            result.push(map(entry));
        }
        result
    }

    // Keeps only symbols with id is in `indexes`.
    pub fn new_with_whitelist(mut self, indexes: &HashSet<SymbolId>) -> Self {
        let mut result = vec![];

        {
            let mut parts_iter = self.data_parts.drain(..).peekable();
            let mut last_regular_removed = false;
            for item in &mut parts_iter {
                let remove = !indexes.contains(&item.symbol_index);

                match item.relation {
                    SymbolRelation::BoundToPrevious { .. } => {
                        if remove != last_regular_removed {
                            // TODO: Add dep in DepGraph for BoundToPrevious symbol
                            log::error!(
                                "BUG: Data segment symbol {} has bound to symbol that was removed, but previous symbol removed: {}",
                                item.symbol_index,
                                last_regular_removed
                            );
                        }
                    }
                    SymbolRelation::Regular { .. } => {
                        last_regular_removed = remove;
                    }
                }
                if remove {
                    continue;
                }
                result.push(item);
            }
        }

        self.data_parts = result;
        self
    }

    pub fn data_len(&self, segment_offset: usize) -> usize {
        let mut len = 0;

        for data_part in &self.data_parts {
            let SymbolRelation::Regular { chunk, aligned } = data_part.relation else {
                // BoundToPrevious symbols are not counted in data length
                continue;
            };
            let current_offset = segment_offset + len;

            // If we're not aligned, add padding
            if aligned {
                let field_alignment = Self::data_symbol_alignment(self.alignment, chunk.len());
                let padding = Self::calculate_padding(current_offset, field_alignment);
                len += padding;
            }

            len += chunk.len();
        }

        len
    }

    /// Compute data init offset.
    /// Returns (offset_expr, segment_offset)
    fn segment_header(
        &self,
        mem_start: usize,
        mut segment_offset: usize,
        lib_base_global_id: Option<u32>,
    ) -> (Option<wasm_encoder::ConstExpr>, usize) {
        match self.kind {
            DataKind::Passive => (None, 0),
            DataKind::Active { .. } => {
                let offset_expr = match lib_base_global_id {
                    None => {
                        segment_offset +=
                            Self::calculate_padding(mem_start + segment_offset, self.alignment);
                        wasm_encoder::ConstExpr::i32_const(
                            (mem_start + segment_offset).try_into().unwrap(),
                        )
                    }
                    Some(lib_base_global_id) => {
                        // submodules use lib_base_id
                        {
                            segment_offset +=
                                Self::calculate_padding(segment_offset, self.alignment);
                            wasm_encoder::ConstExpr::global_get(lib_base_global_id)
                                .with_i32_const(segment_offset.try_into().unwrap())
                                .with_i32_add()
                        }
                    }
                };
                (Some(offset_expr), segment_offset)
            }
        }
    }

    fn data_symbol_alignment(segment_alignment: usize, chunk_size: usize) -> usize {
        if chunk_size == 0 {
            return 1;
        }
        let alignment = 1usize << chunk_size.trailing_zeros();
        std::cmp::min(segment_alignment, alignment)
    }

    fn calculate_padding(starting_point: usize, alignment: usize) -> usize {
        let misalignment = starting_point % alignment;
        if misalignment == 0 {
            0
        } else {
            alignment - misalignment
        }
    }

    /// Compute data init offset and alligned segment_offset.
    pub fn to_segment_output(
        &self,
        lib_base_global_id: Option<u32>,
        mem_start: usize,
        segment_offset: usize,
        //TODO: move segment_offset padding outside
    ) -> (usize, DataSegmentOutput) {
        const BYTE_FILLER: u8 = 0;

        let mut data = Vec::new();

        log::debug!("Segment offset before is {}", mem_start + segment_offset);
        let (data_init, segment_offset) =
            self.segment_header(mem_start, segment_offset, lib_base_global_id);

        log::debug!("Segment offset is {}", mem_start + segment_offset);
        let mut globals = BTreeMap::new();
        let segment_in_mem_start = mem_start + segment_offset;
        for symbol in self.data_parts.iter() {
            match symbol.relation {
                SymbolRelation::BoundToPrevious { offset, .. } => {
                    // BoundToPrevious symbols are not counted in data length
                    log::debug!(
                        "BoundToPrevious symbol {}: {offset} is not counted in data length",
                        symbol.name
                    );

                    globals.insert(
                        symbol.symbol_index,
                        DataSymbolRefs {
                            data_mem_offset: data.len() - offset,
                        },
                    );
                }
                SymbolRelation::Regular { chunk, aligned } => {
                    let total_offset = data.len() + segment_in_mem_start;

                    // add padding to align data
                    if aligned {
                        let field_alignment =
                            Self::data_symbol_alignment(self.alignment, chunk.len());

                        let padding = Self::calculate_padding(total_offset, field_alignment);
                        if padding > 0 {
                            log::debug!(
                                "Add padding before data symbol {}: {padding} bytes",
                                symbol.name
                            );

                            data.resize(data.len() + padding, BYTE_FILLER);
                        }
                    }
                    log::trace!(
                        "Data symbol {}: offset: {}, size: {}, aligned: {}",
                        symbol.name,
                        data.len(),
                        chunk.len(),
                        aligned
                    );

                    globals.insert(
                        symbol.symbol_index,
                        DataSymbolRefs {
                            data_mem_offset: data.len(),
                        },
                    );
                    data.extend_from_slice(chunk);
                }
            }
        }
        (
            segment_offset,
            DataSegmentOutput {
                data_init: data_init.expect("Active data segment should have offset"),
                data,
                data_symbols: globals,
                memory_offset: mem_start + segment_offset,
            },
        )
    }
}

#[derive(Debug)]
pub struct DataSymbolRefs {
    // Relative to lib_base for submodules
    pub data_mem_offset: usize,
}

// generate data segment and global initializers
/// Representation of calculated data segment for output module.
/// Contain data chunk
#[derive(Debug)]
pub struct DataSegmentOutput {
    // only for active segments
    data_init: wasm_encoder::ConstExpr,
    memory_offset: usize,

    data: Vec<u8>,
    data_symbols: BTreeMap<SymbolId, DataSymbolRefs>,
}

impl DataSegmentOutput {
    pub fn data_segment<'a>(&'a self, memory_index: u32) -> wasm_encoder::DataSegment<'a, Vec<u8>> {
        wasm_encoder::DataSegment {
            mode: wasm_encoder::DataSegmentMode::Active {
                memory_index,
                offset: &self.data_init,
            },

            data: self.data.clone(),
        }
    }
    pub fn as_raw(&self) -> &[u8] {
        &self.data
    }
    pub fn memory_offset(&self) -> usize {
        self.memory_offset
    }
    pub fn symbols(&self) -> &BTreeMap<SymbolId, DataSymbolRefs> {
        &self.data_symbols
    }
}

impl<'a> Indexed for crate::emit::SegmentLayout<'a> {
    type StaticTypeTagForIndex = Data<'static>;
    type IndexType = crate::index::Id<Self::StaticTypeTagForIndex>;
}

impl Indexed for crate::emit::DataSegmentOutput {
    type StaticTypeTagForIndex = Data<'static>;
    type IndexType = crate::index::Id<Self::StaticTypeTagForIndex>;
}
