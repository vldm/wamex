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
    emit::globals::DataSymbol,
    index::{DataSegmentId, SymbolIndex},
    read::linking::section::DataInSegment,
};

#[derive(Clone, Debug)]
pub struct NamedData<'a> {
    chunk: &'a [u8],
    // if no data symbol - emit anonymous symbol
    name: &'a str,
    symbol_index: SymbolIndex,
    flags: SymbolFlags,
}

#[derive(Clone, Debug)]
pub struct DataSegment<'a> {
    data_parts: Vec<NamedData<'a>>,
    pub kind: DataKind<'a>,
    // Map from original linking symbol id to index in `data_parts`.
    linking_symbols_map: VecMap<SymbolIndex>,
}

impl<'a> DataSegment<'a> {
    fn shift_range_left(range: Range<usize>, start_offset: usize) -> Range<usize> {
        (range.start - start_offset)..(range.end - start_offset)
    }
    pub fn new_inner(data: Data<'a>, symbols: &[analysis::DataSymbol<'a>]) -> Result<Self> {
        let mut data_parts = vec![];
        let mut linking_symbols = VecMap::new();
        for sym in symbols {
            if sym.range.len() == 0 {
                println!("Data segment has zero-size symbol: {:?}", sym);
                // Ignore zero-size symbols since they cannot be the target of a relocation.
                continue;
            }
            let chunk = &data.data
                [Self::shift_range_left(sym.range.clone(), data.range.end - data.data.len())];
            let name = sym.data_in_segment.name;

            linking_symbols.insert(sym.symbol_index, data_parts.len());
            data_parts.push(NamedData {
                chunk,
                name,
                symbol_index: sym.symbol_index,
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
    pub fn retain_symbols(&mut self, indexes: &[SymbolIndex]) {
        let indexes: HashSet<_> = indexes.iter().collect();
        let mut result = vec![];

        for item in self.data_parts.drain(..) {
            if !indexes.contains(&item.symbol_index) {
                continue;
            }
            result.push(item);
        }

        self.data_parts = result;
    }

    // fn to_linking_symbols(&self) -> Vec<DataInSegment<'a>> {
    //     let mut result = Vec::new();
    //     let mut start = 0;
    //     for symbol in &self.data_parts {
    //         result.push(DataInSegment {
    //             name: symbol.name,
    //             flags: symbol.flags,
    //             offset: start,
    //             size: symbol.chunk.len() as u32,
    //         });
    //         start += symbol.chunk.len() as u32;
    //     }
    //     result
    // }

    pub fn to_lib_output(&self, lib_base_global_id: u32, segment_offset: i32) -> DataSegmentOutput {
        let mut data = Vec::new();
        let data_init = match self.kind {
            DataKind::Passive => None,
            DataKind::Active { .. } => {
                let offset_expr = wasm_encoder::ConstExpr::global_get(lib_base_global_id)
                    .with_i32_const(segment_offset)
                    .with_i32_add();
                Some(offset_expr)
            }
        };
        let mut globals = vec![];
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
