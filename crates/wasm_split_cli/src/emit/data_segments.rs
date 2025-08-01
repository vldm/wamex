use std::{cmp::Ordering, collections::HashSet, fmt::Debug, ops::Range};

use anyhow::Result;
use wasmparser::{Data, DataKind, SymbolFlags};

use crate::{
    analysis,
    emit::globals::DataSymbol,
    helpers::ShiftRange,
    index::{DataSymbolId, Indexed},
};

#[derive(Clone)]
pub struct NamedData<'a> {
    chunk: &'a [u8],
    // if no data symbol - emit anonymous symbol
    name: &'a str,
    // Related to data segment start
    original_range: Range<usize>,
    symbol_index: DataSymbolId,
    flags: SymbolFlags,
}

impl Debug for NamedData<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Print in short form:
        // ...<name>:symbol_index - flags
        write!(
            f,
            "{} <{}>:{:?}",
            hex::encode(&self.chunk),
            self.name,
            self.flags
        )
    }
}

#[derive(Clone)]
pub struct DataSegment<'a> {
    data_parts: Vec<NamedData<'a>>,
    // Full range of original data segment, including header
    pub original_range: Range<usize>,
    pub data_offset: usize,
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

        for sym in symbols {
            if sym.range.len() == 0 {
                println!("Data segment has zero-size symbol: {:?}", sym);
                // Ignore zero-size symbols since they cannot be the target of a relocation.
                continue;
            }
            let original_range = sym.range.clone().shift_left(data_start);
            let chunk = &data.data[original_range.clone()];
            let name = sym.data_in_segment.name;

            data_parts.push(NamedData {
                chunk,
                name,
                symbol_index: sym.symbol_index,
                flags: sym.data_in_segment.flags,
                original_range,
            })
        }
        let kind = data.kind.clone();

        let original_range = data.range;
        debug_assert!(data_parts.is_sorted_by(|a, b| matches!(
            a.original_range.start.cmp(&b.original_range.start),
            Ordering::Less
        )),);
        Ok(DataSegment {
            data_parts,
            kind,
            original_range,
            data_offset: data_start,
        })
    }

    // Keeps only symbols with id is in `indexes`.
    pub fn retain_symbols(&mut self, indexes: &HashSet<DataSymbolId>) {
        let mut result = vec![];

        for item in self.data_parts.drain(..) {
            if !indexes.contains(&item.symbol_index) {
                continue;
            }
            result.push(item);
        }

        self.data_parts = result;
    }

    // Get relocations related to module start
    // Return relocations with shifted offsets (if some data parts are removed)
    pub fn shift_relocation_entries(
        &self,
        entries: &[wasmparser::RelocationEntry],
    ) -> Vec<wasmparser::RelocationEntry> {
        let mut data_len = 0;
        let segment_offset = self.data_offset as usize;

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
            // skip parts that is after current entry
            // or not exist
            while &part.original_range.end < &entry_range.end {
                log::trace!("Skipping part {:?} before entry {:?}", part, entry);
                data_len += part.chunk.len();
                let Some(new_part) = parts_iter.next() else {
                    log::debug!("No more data parts for entry {:?}", entry);
                    break 'outer;
                };
                part = new_part;
            }
            if part.original_range.start > entry_range.start as usize {
                log::trace!("Skipping relocation entry {:?}", entry);
                continue;
            }
            debug_assert!(
                part.original_range.start <= entry_range.start as usize,
                "Data segment should start before relocation entry start"
            );
            log::debug!("Processing part {:?} for entry {:?}", part, entry);

            log::trace!("Part range after skip: {:?}", part.original_range);
            // calculate shift using current part and
            let shift = part.original_range.start.saturating_sub(data_len + 1); // +1 because it is inclusive range
            let mut shifted_entry = entry.clone(); // Shift relocation entry
            shifted_entry.offset =
                (entry_range.start.checked_sub(shift).unwrap() + segment_offset) as u32;

            result.push(shifted_entry);
        }

        result
    }

    pub fn to_lib_output(
        &self,
        lib_base_global_id: Option<u32>,
        segment_offset: i32,
    ) -> DataSegmentOutput {
        let mut data = Vec::new();
        let data_init = match self.kind {
            DataKind::Passive => None,
            DataKind::Active { .. } => {
                let offset_expr = match lib_base_global_id {
                    None => wasm_encoder::ConstExpr::i32_const(segment_offset),
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
            globals.push(DataSymbol {
                data_offset: data.len() as i32,
                symbol_index: symbol.symbol_index,
                type_info: super::globals::GlobalConstructor::POINTER_TYPE,
            });
            data.extend_from_slice(symbol.chunk);
        }
        DataSegmentOutput {
            data_init,
            data,
            globals,
        }
    }
}

// generate data segment and global initializers
pub struct DataSegmentOutput {
    data_init: Option<wasm_encoder::ConstExpr>,
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
}

impl<'a> Indexed for crate::emit::DataSegment<'a> {
    type StaticIndexType = Data<'static>;
}

impl Indexed for crate::emit::DataSegmentOutput {
    type StaticIndexType = Data<'static>;
}
