use std::{collections::HashSet, fmt::Debug, iter::Peekable};

use anyhow::Result;
use wasm_encoder::Encode;
use wasmparser::{Data, DataKind, SymbolFlags};

use crate::{
    analysis,
    emit::globals::DataSymbol,
    helpers::{encoding_size, RangeComp, RangeExt},
    index::{DataSymbolId, Indexed},
};

#[derive(Clone, Debug)]
pub struct NamedData<'a> {
    chunk: &'a [u8],

    name: &'a str,
    index: DataSymbolId,
    flags: SymbolFlags,
    aligned: bool, // true if data is aligned to segment alignment
    // offset related to this symbol
    relocations: Vec<wasmparser::RelocationEntry>,
}

#[derive(Clone)]
pub struct DataSegment<'a> {
    data_parts: Vec<NamedData<'a>>,

    pub alignment: usize,
    pub kind: DataKind<'a>,
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

impl<'a> DataSegment<'a> {
    pub fn new_inner(
        data: Data<'a>,
        segment_info: wasmparser::Segment<'a>,
        symbols: &[analysis::DataSymbol<'a>],
        // Save relocations related to each symbol
        relocations: &[wasmparser::RelocationEntry],
    ) -> Result<Self> {
        let alignment = (2usize).pow(segment_info.alignment);
        // skip header of data segment
        let data_start = data.range.end - data.data.len();

        let mut data_parts = vec![];
        let mut relocation_iter = relocations.iter().peekable();
        let mut prev_end = 0;
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
                    let range = entry.relocation_range();
                    match sym.range.cmp_range(range) {
                        // In case relocations is not ordered, or related to more than one symbol.
                        RangeComp::Right | RangeComp::NonComparable => {
                            panic!(
                                "BUG: Relocation entry is not related to symbols: {:?} and {:?}",
                                sym, entry
                            );
                        }
                        RangeComp::OverlapOrEqual => true,
                        RangeComp::Left => false,
                    }
                },
            );

            let original_range = sym.range.clone().shift_left(data_start);

            let field_alignment = std::cmp::min(alignment, original_range.len());
            let aligned = original_range.start % field_alignment == 0;
            let chunk = &data.data[original_range.clone()];

            if prev_end < original_range.start {
                let gap_range = prev_end..original_range.start;

                if gap_range.len() >= alignment {
                    log::debug!(
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
            } else if prev_end > original_range.start {
                log::error!(
                    "Skipping DataSymbol that intersects with previous: {:?} range:{:?} prev_end: {}",
                    sym,
                    original_range,
                    prev_end
                );
                //TODO: SKip?
                continue;
            }

            let name = sym.data_in_segment.name;

            prev_end = original_range.end;

            let part = NamedData {
                chunk,
                name,
                index: sym.symbol_index,
                flags: sym.data_in_segment.flags,
                relocations: entries,
                aligned,
            };
            log::trace!("Data part: {part:?}");
            data_parts.push(part)
        }

        let kind = data.kind.clone();

        Ok(DataSegment {
            alignment,
            data_parts,
            kind,
        })
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
    pub fn retain_symbols(&mut self, indexes: &HashSet<DataSymbolId>) {
        let mut result = vec![];

        for item in self.data_parts.drain(..) {
            if !indexes.contains(&item.index) {
                continue;
            }
            result.push(item);
        }

        self.data_parts = result;
    }

    pub fn data_len(&self, segment_misalignment: usize) -> usize {
        let mut len = 0;

        for data_part in &self.data_parts {
            let current_offset = segment_misalignment + len;
            let field_alignment = std::cmp::min(self.alignment, data_part.chunk.len());
            // Determine how far off we are from the required alignment
            let misalignment = current_offset % field_alignment;

            // If we're not aligned, add padding
            if data_part.aligned && misalignment != 0 {
                let padding = field_alignment - misalignment;
                len += padding;
            }

            len += data_part.chunk.len();
        }

        len
    }

    pub fn header_len(
        &self,
        memory_index: u32,
        lib_base_global_id: Option<u32>,
        mem_start: i32,
        segment_offset: i32,
    ) -> usize {
        let (data_init, segment_misalignment) =
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

        len + encoding_size(self.data_len(segment_misalignment) as u32)
    }

    /// Compute data init offset and segment misalignment.
    /// Returns (offset_expr, segment_misalignment)
    fn segment_header(
        &self,
        mem_start: i32,
        segment_offset: i32,
        lib_base_global_id: Option<u32>,
    ) -> (Option<wasm_encoder::ConstExpr>, usize) {
        match self.kind {
            DataKind::Passive => (None, 0),
            DataKind::Active { .. } => {
                let (offset_expr, segment_misalignment) = match lib_base_global_id {
                    None => (
                        wasm_encoder::ConstExpr::i32_const(mem_start + segment_offset),
                        (mem_start + segment_offset) as usize % self.alignment,
                    ),
                    Some(lib_base_global_id) => {
                        // submodules use lib_base_id
                        (
                            wasm_encoder::ConstExpr::global_get(lib_base_global_id)
                                .with_i32_const(segment_offset)
                                .with_i32_add(),
                            segment_offset as usize % self.alignment,
                        )
                    }
                };
                (Some(offset_expr), segment_misalignment)
            }
        }
    }

    pub fn to_lib_output(
        &self,
        lib_base_global_id: Option<u32>,
        mem_start: i32,
        segment_offset: i32,
    ) -> DataSegmentOutput {
        const BYTE_FILLER: u8 = 0;
        let mut all_relocations = Vec::new();

        let mut data = Vec::new();

        let (data_init, segment_misalignment) =
            self.segment_header(mem_start, segment_offset, lib_base_global_id);
        let mut globals = Vec::new();
        for symbol in &self.data_parts {
            let total_offset = data.len() + segment_misalignment;
            let field_alignment = std::cmp::min(self.alignment, symbol.chunk.len());
            let misalignment = total_offset % field_alignment;

            // add padding to align data
            if symbol.aligned && misalignment != 0 {
                let padding = field_alignment - misalignment;
                log::debug!(
                    "Add padding before data symbol {}: {padding} bytes",
                    symbol.name
                );
                data.resize(data.len() + padding, BYTE_FILLER);
            }

            globals.push(DataSymbol {
                data_offset: data.len() as i32,
                symbol_index: symbol.index,
                type_info: super::globals::GlobalConstructor::POINTER_TYPE,
            });
            all_relocations.extend(symbol.relocations.iter().map(|entry| {
                wasmparser::RelocationEntry {
                    offset: entry.offset + data.len() as u32,
                    ..entry.clone()
                }
            }));
            data.extend_from_slice(symbol.chunk);
        }
        DataSegmentOutput {
            memory_offset: mem_start + segment_offset,
            data_init,
            data,
            globals,
            relocations: all_relocations,
        }
    }
}

// generate data segment and global initializers
pub struct DataSegmentOutput {
    data_init: Option<wasm_encoder::ConstExpr>,
    // only for active segments
    memory_offset: i32,
    data: Vec<u8>,
    globals: Vec<super::globals::DataSymbol>,
    relocations: Vec<wasmparser::RelocationEntry>,
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
    pub fn globals(&self) -> &[super::globals::DataSymbol] {
        &self.globals
    }
    pub fn relocations(&self) -> &[wasmparser::RelocationEntry] {
        &self.relocations
    }
    pub fn is_active(&self) -> bool {
        self.data_init.is_some()
    }
    pub fn memory_offset(&self) -> i32 {
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
