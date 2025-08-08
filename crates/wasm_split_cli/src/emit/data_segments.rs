use std::{cmp::Ordering, collections::HashSet, fmt::Debug, ops::Range};

use anyhow::Result;
use wasm_encoder::Encode;
use wasmparser::{Data, DataKind, SymbolFlags};

use crate::{
    analysis::{self, ModuleInfo},
    emit::globals::DataSymbol,
    helpers::{encoding_size, RangeComp, RangeExt},
    index::{DataSymbolId, Indexed},
};

#[derive(Clone)]
pub struct NamedData<'a> {
    chunk: &'a [u8],
    // Related to data segment start
    original_range: Range<usize>,

    symbol: GapOrSymbol<'a>,
}

#[derive(Clone)]
enum GapOrSymbol<'a> {
    Gap,
    Symbol {
        name: &'a str,
        index: DataSymbolId,
        flags: SymbolFlags,
    },
}

impl Debug for NamedData<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Print in short form:
        // ...<name>:symbol_index - flags
        let name = match &self.symbol {
            GapOrSymbol::Gap => "GAP",
            GapOrSymbol::Symbol { name, .. } => name,
        };
        let flags = match &self.symbol {
            GapOrSymbol::Gap => SymbolFlags::empty(),
            GapOrSymbol::Symbol { flags, .. } => *flags,
        };
        write!(f, "{} <{}>:{:?}", hex::encode(&self.chunk), name, flags)
    }
}

#[derive(Clone)]
pub struct DataSegment<'a> {
    data_parts: Vec<NamedData<'a>>,
    // Full range of original data segment, including header
    pub original_range: Range<usize>,
    pub segment_offset: usize,
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
    pub fn new_inner(data: Data<'a>, symbols: &[analysis::DataSymbol<'a>]) -> Result<Self> {
        let mut data_parts = vec![];

        // skip header of data segment
        let data_start = data.range.end - data.data.len();
        dbg!(data_start - data.range.start);
        dbg!(&data.kind);

        let mut prev_end = 0;
        for sym in symbols {
            if sym.range.len() == 0 {
                log::error!("Data segment has zero-size symbol: {:?}", sym);
                // Ignore zero-size symbols since they cannot be the target of a relocation.
                continue;
            }
            let original_range = sym.range.clone().shift_left(data_start);
            let chunk = &data.data[original_range.clone()];

            if prev_end < original_range.start {
                let gap_range = prev_end..original_range.start;
                // There is a gap before this symbol, add it
                let gap = &data.data[gap_range.clone()];
                data_parts.push(NamedData {
                    chunk: gap,
                    original_range: gap_range,
                    symbol: GapOrSymbol::Gap,
                });
            } else if prev_end > original_range.start {
                log::error!(
                    "Data segment has intersecting parts: {:?} and {:?}",
                    prev_end,
                    original_range
                );
                //TODO: SKip?
                continue;
            }

            let name = sym.data_in_segment.name;

            prev_end = original_range.end;

            data_parts.push(NamedData {
                chunk,
                original_range,
                symbol: GapOrSymbol::Symbol {
                    name,
                    index: sym.symbol_index,
                    flags: sym.data_in_segment.flags,
                },
            })
        }

        let kind = data.kind.clone();
        let original_range = data.range;
        // debug_assert!(data_parts.is_sorted_by(|a, b| matches!(
        //     a.original_range.end.cmp(&b.original_range.start),
        //     Ordering::Less | Ordering::Equal
        // )),);
        Ok(DataSegment {
            data_parts,
            kind,
            original_range,
            segment_offset: data_start,
        })
    }

    // Keeps only symbols with id is in `indexes`.
    pub fn retain_symbols(&mut self, indexes: &HashSet<DataSymbolId>, retain_gaps: bool) {
        let mut result = vec![];

        for item in self.data_parts.drain(..) {
            match item.symbol {
                GapOrSymbol::Gap if !retain_gaps => continue,
                GapOrSymbol::Symbol { index, .. } if !indexes.contains(&index) => continue,
                _ => {}
            }
            result.push(item);
        }

        self.data_parts = result;
    }

    /// Shifts relocation entries to account for removed or reordered data parts in the segment.
    ///
    /// # Parameters
    /// - `entries`: A slice of `wasmparser::RelocationEntry` representing the original relocation entries.
    ///
    /// # Returns
    /// A `Vec<wasmparser::RelocationEntry>` containing the relocation entries with updated offsets
    /// relative to the current data segment layout.
    pub fn shift_relocation_entries(
        &self,
        entries: &[wasmparser::RelocationEntry],
    ) -> Vec<wasmparser::RelocationEntry> {
        let mut kept_data_len = 0;
        let segment_offset = self.segment_offset as usize;

        let mut result = Vec::with_capacity(entries.len());

        let mut parts_iter = self.data_parts.iter().peekable();
        let Some(mut part) = parts_iter.next() else {
            return Vec::new();
        };
        'outer: for entry in entries.iter() {
            // offset related to start
            let entry_range = entry.relocation_range().shift_left(segment_offset);

            log::trace!("Entry range: {:?}", entry_range);
            log::trace!("Part range: {:?}", part.original_range);

            'no_reloc: loop {
                match part.original_range.cmp_range(&entry_range) {
                    RangeComp::Left => {
                        log::trace!("Skipping entry {:?} after part {:?}", entry, part);
                        // This means that entry is after part, so we can skip it

                        kept_data_len += part.chunk.len();
                        let Some(new_part) = parts_iter.next() else {
                            log::debug!("No more data parts for entry {:?}", entry);
                            break 'outer;
                        };
                        part = new_part;
                        continue 'no_reloc;
                    }
                    RangeComp::OverlapOrEqual => {
                        break 'no_reloc;
                    }
                    RangeComp::Right => {
                        log::trace!("Skipping entry {:?} before part {:?}", entry, part);
                        continue 'outer;
                    }
                    RangeComp::NonComparable => {
                        panic!(
                            "Data segment has intersecting parts: {:?} and {:?}",
                            part, entry
                        );
                    }
                }
            }
            // Compute new offset: sum of all kept bytes before this part + offset within this part
            let rel_in_part = (entry_range.start as usize)
                .checked_sub(part.original_range.start)
                .unwrap();
            let new_offset = kept_data_len + rel_in_part;

            let mut shifted_entry = entry.clone();
            shifted_entry.offset = u32::try_from(segment_offset + new_offset).unwrap();

            result.push(shifted_entry);
        }

        result
    }
    pub fn data_len(&self, retain_gaps: bool) -> usize {
        let mut len = 0;
        for symbol in &self.data_parts {
            match symbol.symbol {
                GapOrSymbol::Gap if !retain_gaps => continue,
                _ => {}
            }
            len += symbol.chunk.len();
        }
        len
    }

    pub fn header_len(
        &self,
        memory_index: u32,
        lib_base_global_id: Option<u32>,
        mem_start: i32,
        segment_offset: i32,
        retain_gaps: bool,
    ) -> usize {
        let data_init = match self.kind {
            DataKind::Passive => None,
            DataKind::Active { .. } => {
                let offset_expr = match lib_base_global_id {
                    None => wasm_encoder::ConstExpr::i32_const(mem_start + segment_offset),
                    Some(lib_base_global_id) => {
                        // submodules use lib_base_id
                        wasm_encoder::ConstExpr::global_get(lib_base_global_id)
                            .with_i32_const(segment_offset)
                            .with_i32_add()
                    }
                };
                Some(offset_expr)
            }
        };
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

        len + encoding_size(self.data_len(retain_gaps) as u32)
    }

    pub fn to_lib_output(
        &self,
        lib_base_global_id: Option<u32>,
        mem_start: i32,
        segment_offset: i32,
        retain_gaps: bool,
    ) -> DataSegmentOutput {
        let mut data = Vec::new();
        let data_init = match self.kind {
            DataKind::Passive => None,
            DataKind::Active { .. } => {
                let offset_expr = match lib_base_global_id {
                    None => wasm_encoder::ConstExpr::i32_const(mem_start + segment_offset),
                    Some(lib_base_global_id) => {
                        // submodules use lib_base_id
                        wasm_encoder::ConstExpr::global_get(lib_base_global_id)
                            .with_i32_const(segment_offset)
                            .with_i32_add()
                    }
                };
                Some(offset_expr)
            }
        };
        let mut globals = Vec::new();
        for symbol in &self.data_parts {
            let GapOrSymbol::Symbol { index, .. } = symbol.symbol else {
                // Gaps are not represented as globals

                if retain_gaps {
                    data.extend_from_slice(symbol.chunk);
                }
                continue;
            };
            globals.push(DataSymbol {
                data_offset: data.len() as i32,
                symbol_index: index,
                type_info: super::globals::GlobalConstructor::POINTER_TYPE,
            });
            data.extend_from_slice(symbol.chunk);
        }
        DataSegmentOutput {
            memory_offset: mem_start + segment_offset,
            data_init,
            data,
            globals,
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
