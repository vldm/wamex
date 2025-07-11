use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

use anyhow::Result;
use vec_map::VecMap;
use wasm_encoder::GlobalType;
use wasmparser::{Data, DataKind, SymbolFlags};

use crate::{
    analysis,
    index::{DataSegmentId, SymbolIndex},
    wasm_parse::{linking, Relocation, RelocationSection},
};

#[derive(Clone, Debug)]
pub struct DataPartSymbolized<'a> {
    chunk: &'a [u8],
    // if no data symbol - emit anonymous symbol
    name: &'a str,
    flags: SymbolFlags,
}

#[derive(Clone, Debug)]
pub struct DataSegment<'a> {
    data_parts: Vec<DataPartSymbolized<'a>>,
    pub kind: DataKind<'a>,
    // Map from original linking symbol index to new symbol index
    linking_symbols_map: VecMap<SymbolIndex>,
}

impl<'a> DataSegment<'a> {
    fn new_inner(data: Data<'a>, symbols: &[analysis::DataSymbol<'a>]) -> Result<Self> {
        let mut data_parts = vec![];
        let mut linking_symbols = VecMap::new();
        for sym in symbols {
            if sym.range.len() == 0 {
                // Ignore zero-size symbols since they cannot be the target of a relocation.
                continue;
            }
            let chunk = &data.data[sym.range.clone()];
            let name = sym.data_in_segment.name;

            linking_symbols.insert(sym.symbol_index, data_parts.len());
            data_parts.push(DataPartSymbolized {
                chunk,
                name,
                flags: sym.data_in_segment.flags,
            })
        }
        let kind = data.kind.clone();

        Ok(DataSegment {
            data_parts,
            kind,
            linking_symbols_map: linking_symbols,
        })
    }

    // Keeps only symbols with id is in `indexes`.
    // Returns map from old index to new index.
    pub fn retain_indexes(&mut self, indexes: HashSet<SymbolIndex>) {
        let mut result = vec![];
        let mut sym_map = VecMap::new();

        for linking_index in indexes {
            let Some(idx) = self.linking_symbols_map.get(linking_index) else {
                log::error!("Symbol index {linking_index} is not found");
                continue;
            };

            sym_map.insert(linking_index, result.len());
            result.push(self.data_parts[*idx].clone());
        }

        self.data_parts = result;
        self.linking_symbols_map = sym_map
    }

    fn to_linking_symbols(&self) -> Vec<linking::DataInSegment<'a>> {
        let mut result = Vec::new();
        let mut start = 0;
        for symbol in &self.data_parts {
            result.push(linking::DataInSegment {
                name: symbol.name,
                flags: symbol.flags,
                offset: start,
                size: symbol.chunk.len() as u32,
            });
            start += symbol.chunk.len() as u32;
        }
        result
    }

    pub fn to_lib_output(&self, lib_base_global_id: u32, offset: i32) -> DataSegmentOutput {
        let mut data = Vec::new();
        let data_init = match self.kind {
            DataKind::Passive => None,
            DataKind::Active { .. } => {
                let offset_expr = wasm_encoder::ConstExpr::global_get(lib_base_global_id)
                    .with_i32_const(offset)
                    .with_i32_add();
                Some(offset_expr)
            }
        };
        let mut globals = vec![];
        for symbol in &self.data_parts {
            globals.push(super::globals::GlobalConstructor {
                data_offset: data.len() as i32,
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
struct DataSegmentOutput {
    data_init: Option<wasm_encoder::ConstExpr>,
    data: Vec<u8>,
    globals: Vec<super::globals::GlobalConstructor>,
}

impl DataSegmentOutput {
    pub fn data_segment<'a>(
        &'a self,
        memory_index: u32,
    ) -> wasm_encoder::DataSegment<'a, &'a [u8]> {
        wasm_encoder::DataSegment {
            mode: match self.data_init.as_ref() {
                None => wasm_encoder::DataSegmentMode::Passive,
                Some(data_init) => wasm_encoder::DataSegmentMode::Active {
                    memory_index,
                    offset: &data_init,
                },
            },
            data: &self.data,
        }
    }
}
