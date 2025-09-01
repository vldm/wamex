use std::{collections::HashSet, fmt::Debug, iter::Peekable};

use anyhow::Result;
use wasm_encoder::Encode;
use wasmparser::{Data, DataKind, SymbolFlags};

use crate::{
    analysis,
    emit::globals::DataSymbol,
    helpers::{encoding_size, RangeComp, RangeExt},
    index::{AnySymbolId, DataSymbolId, Indexed},
};

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
pub struct NamedData<'a> {
    name: &'a str,
    index: DataSymbolId,
    flags: SymbolFlags,
    relation: SymbolRelation<'a>,

    // offset related to this symbol
    relocations: Vec<wasmparser::RelocationEntry>,
}

impl NamedData<'_> {
    pub fn name(&self) -> &str {
        self.name
    }
    pub fn relocations(&self) -> &[wasmparser::RelocationEntry] {
        &self.relocations
    }
    //TODO: Don't expose in public API
    pub fn symbol_relation(&self) -> &SymbolRelation<'_> {
        &self.relation
    }
    pub fn index(&self) -> DataSymbolId {
        self.index
    }
}

#[derive(Clone)]
pub struct DataSegment<'a> {
    data_parts: Vec<NamedData<'a>>,

    alignment: usize,
    kind: DataKind<'a>,
    mem_offset: usize,
}

impl Debug for DataSegment<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match &self.kind {
            DataKind::Passive => format!("Passive"),
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

impl<'src> DataSegment<'src> {
    pub fn new_inner(
        data: Data<'src>,
        segment_info: wasmparser::Segment<'_>,
        symbols: &[analysis::DataSymbol<'_, 'src>],
        // Save relocations related to each symbol
        relocations: &[wasmparser::RelocationEntry],
    ) -> Result<DataSegment<'src>> {
        let alignment = (2usize).pow(segment_info.alignment);
        // skip header of data segment
        let data_start = data.range.end - data.data.len();

        let mem_offset = match &data.kind {
            DataKind::Passive => 0,
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
        let mut relocation_iter = relocations.iter().peekable();
        let mut prev = 0..0;
        for sym in symbols {
            if sym.range.len() == 0 {
                log::error!("Data segment has zero-size symbol: {:?}", sym);
                // Ignore zero-size symbols since they cannot be the target of a relocation.
                continue;
            }
            let entries = Self::collect_and_map_while(
                &mut relocation_iter,
                // Save relocation entries related to this symbol
                |entry| wasmparser::RelocationEntry {
                    offset: entry.offset - sym.range.start as u32,
                    ..entry.clone()
                },
                |entry| {
                    match sym.range.cmp_range(entry.relocation_range()) {
                        // In case relocations is not ordered, or related to more than one symbol.
                        RangeComp::Right | RangeComp::NonComparable | RangeComp::Within => {
                            panic!(
                                "BUG: Relocation entry is not related to symbols: {:?} and {:?}",
                                sym, entry
                            );
                        }
                        RangeComp::Overlap | RangeComp::Equal => true,
                        RangeComp::Left => false,
                    }
                },
            );

            let symbol_in_data = sym.range.clone().shift_left(data_start);

            let field_alignment = std::cmp::min(alignment, symbol_in_data.len());

            let relation = if prev.end > symbol_in_data.start {
                log::warn!(
                    "DataSymbol intersects with previous, this is currently in testing: {:?} range:{:?} prev_range: {:?}",
                    sym,
                    symbol_in_data,
                    prev
                );
                assert!(matches!(
                    prev.cmp_range(&symbol_in_data),
                    RangeComp::Overlap | RangeComp::Equal
                ));
                let offset = prev.end - symbol_in_data.start;
                log::warn!(
                    "DataSymbol intersects with previous, offset: {}, entries: {:?}, index {}",
                    offset,
                    entries,
                    sym.symbol_index
                );
                // range.intersect(other)
                SymbolRelation::BoundToPrevious {
                    offset,
                    len: symbol_in_data.len(),
                }
            } else {
                if prev.end < symbol_in_data.start {
                    let gap_range = prev.end..symbol_in_data.start;

                    if gap_range.len() >= alignment {
                        log::error!(
                            "Data segment has gap larger than alignment: {:?} > {} before {}",
                            gap_range,
                            alignment,
                            sym.data_in_segment.name
                        );
                    } else {
                        log::debug!(
                            "Data segment has gap: {:?} ({} bytes) before {}",
                            gap_range,
                            gap_range.len(),
                            sym.data_in_segment.name
                        );
                    }
                }
                let aligned = (symbol_in_data.start + mem_offset as usize) % field_alignment == 0;

                log::trace!(
                    "Data symbol {}: offset: {}, size: {}, aligned: {}, alignment: {}",
                    sym.data_in_segment.name,
                    symbol_in_data.start,
                    symbol_in_data.len(),
                    aligned,
                    field_alignment
                );
                prev = symbol_in_data.clone();
                SymbolRelation::Regular {
                    chunk: &data.data[symbol_in_data.clone()],
                    aligned: aligned,
                }
            };

            let part = NamedData {
                name: sym.data_in_segment.name,
                index: sym.symbol_index,
                flags: sym.data_in_segment.flags,
                relocations: entries,
                relation,
            };
            log::trace!("Data part: {part:?}");
            data_parts.push(part)
        }

        let kind = data.kind.clone();

        Ok(DataSegment {
            alignment,
            data_parts,
            kind,
            mem_offset: mem_offset as usize,
        })
    }
    pub fn _data_symbols_iter(&self) -> impl Iterator<Item = &NamedData<'src>> {
        self.data_parts.iter()
    }
    pub fn _data_symbols_rev_iter(
        &self,
        idx: DataSymbolId,
    ) -> impl Iterator<Item = &NamedData<'src>> {
        let idx = self
            .data_parts
            .iter()
            .enumerate()
            .find(|(_, part)| part.index == idx);
        let end = idx.map(|(end, _)| end).unwrap();
        self.data_parts[..end].iter().rev()
    }

    pub fn get_data_symbol(&self, idx: DataSymbolId) -> Option<&NamedData<'src>> {
        self.data_parts.iter().find(|part| part.index == idx)
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
    pub fn new_with_whitelist(mut self, indexes: &HashSet<DataSymbolId>) -> Self {
        let mut result = vec![];

        {
            let mut parts_iter = self.data_parts.drain(..).peekable();
            let mut last_regular_removed = false;
            for item in &mut parts_iter {
                let remove = !indexes.contains(&item.index);

                match item.relation {
                    SymbolRelation::BoundToPrevious { .. } => {
                        if remove != last_regular_removed {
                            // TODO: Add dep in DepGraph for BoundToPrevious symbol
                            log::error!(
                                "BUG: Data segment symbol {} has bound to symbol that was removed, but previous symbol removed: {}",
                                item.index,
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
                let field_alignment = self.field_alignment(chunk.len());
                let padding = Self::calculate_padding(current_offset, field_alignment);
                len += padding;
            }

            len += chunk.len();
        }

        len
    }

    pub fn header_len(
        &self,
        memory_index: u32,
        lib_base_global_id: Option<u32>,
        mem_start: usize,
        segment_offset: usize,
    ) -> usize {
        let (data_init, segment_offset) =
            self.segment_header(mem_start, segment_offset, lib_base_global_id);
        let len = match data_init {
            None => 1,
            Some(data_init) => {
                let mut len = 1;
                if memory_index != 0 {
                    len += encoding_size(memory_index);
                }
                let mut buf = Vec::new();
                data_init.encode(&mut buf);
                len + buf.len()
            }
        };

        len + encoding_size(self.data_len(segment_offset) as u32)
    }

    /// Compute data init offset and alligned segment_offset.
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

    fn field_alignment(&self, chunk_size: usize) -> usize {
        let alignment = 1usize << chunk_size.trailing_zeros();
        std::cmp::min(self.alignment, alignment)
    }

    fn calculate_padding(starting_point: usize, alignment: usize) -> usize {
        let misalignment = starting_point % alignment;
        if misalignment == 0 {
            0
        } else {
            alignment - misalignment
        }
    }

    pub fn to_lib_output(
        &self,
        lib_base_global_id: Option<u32>,
        mem_start: usize,
        segment_offset: usize,
        //TODO: move segment_offset padding outside
    ) -> (usize, DataSegmentOutput) {
        const BYTE_FILLER: u8 = 0;
        let mut all_relocations = Vec::new();

        let mut data = Vec::new();

        log::debug!("Segment offset before is {}", mem_start + segment_offset);
        let (data_init, segment_offset) =
            self.segment_header(mem_start, segment_offset, lib_base_global_id);

        log::debug!("Segment offset is {}", mem_start + segment_offset);
        let mut globals = Vec::new();
        for (new_index, symbol) in self.data_parts.iter().enumerate() {
            match symbol.relation {
                SymbolRelation::BoundToPrevious { offset, len } => {
                    // BoundToPrevious symbols are not counted in data length
                    log::debug!(
                        "BoundToPrevious symbol {}: {offset} is not counted in data length",
                        symbol.name
                    );

                    globals.push(DataSymbol {
                        data_offset: data.len() - offset,
                        symbol_index: symbol.index,
                        type_info: super::globals::GlobalConstructor::POINTER_TYPE,
                    });
                }
                SymbolRelation::Regular { chunk, aligned } => {
                    let total_offset = data.len() + segment_offset as usize;

                    // add padding to align data
                    if aligned {
                        let field_alignment = self.field_alignment(chunk.len()); //std::cmp::min(self.alignment, chunk.len());

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

                    globals.push(DataSymbol {
                        data_offset: data.len(),
                        symbol_index: symbol.index,
                        type_info: super::globals::GlobalConstructor::POINTER_TYPE,
                    });
                    all_relocations.extend(symbol.relocations.iter().map(|entry| SymbolReloc {
                        reloc_in_symbol_index: DataSymbolId::from_index(new_index),
                        offset: entry.offset as i64,
                        entry: wasmparser::RelocationEntry {
                            offset: entry.offset + data.len() as u32,
                            ..entry.clone()
                        },
                    }));
                    data.extend_from_slice(chunk);
                }
            }
        }
        (
            segment_offset,
            DataSegmentOutput {
                memory_offset: mem_start + segment_offset,
                data_init,
                data,
                symbols: globals,
                relocations: all_relocations,
            },
        )
    }
}

pub struct SymbolReloc {
    pub reloc_in_symbol_index: DataSymbolId,
    pub offset: i64,
    pub entry: wasmparser::RelocationEntry,
}
// generate data segment and global initializers
pub struct DataSegmentOutput {
    data_init: Option<wasm_encoder::ConstExpr>,
    // only for active segments
    memory_offset: usize,
    data: Vec<u8>,
    symbols: Vec<super::globals::DataSymbol>,
    relocations: Vec<SymbolReloc>,
}

impl DataSegmentOutput {
    pub fn data_segment<'a>(&'a self, memory_index: u32) -> wasm_encoder::DataSegment<'a, Vec<u8>> {
        wasm_encoder::DataSegment {
            mode: match self.data_init.as_ref() {
                None => wasm_encoder::DataSegmentMode::Passive,
                Some(data_init) => wasm_encoder::DataSegmentMode::Active {
                    memory_index,
                    offset: &data_init,
                },
            },
            data: self.data.clone(),
        }
    }
    pub fn as_raw(&self) -> &[u8] {
        &self.data
    }
    pub fn symbols(&self) -> &[super::globals::DataSymbol] {
        &self.symbols
    }
    pub fn relocations(&self) -> &[SymbolReloc] {
        &self.relocations
    }
    pub fn is_active(&self) -> bool {
        self.data_init.is_some()
    }
    pub fn memory_offset(&self) -> usize {
        self.memory_offset
    }
}

impl<'a> Indexed for crate::emit::DataSegment<'a> {
    type StaticTypeTagForIndex = Data<'static>;
    type IndexType = crate::index::Id<Self::StaticTypeTagForIndex>;
}

impl Indexed for crate::emit::DataSegmentOutput {
    type StaticTypeTagForIndex = Data<'static>;
    type IndexType = crate::index::Id<Self::StaticTypeTagForIndex>;
}
