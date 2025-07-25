//!
//! Module with external info usefull to build dep graph, and request information about function and data entries.
//!

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::Debug;
use std::ops::Range;

use anyhow::{anyhow, bail, Context, Result};
use vec_map::VecMap;
use wasmparser::{Data, TypeRef};

use crate::read::{self, linking::section::DataInSegment};

use crate::index::{DataSegmentId, ImportId, InputFuncId, SymbolIndex};

mod debug;
pub mod dep_graph;
pub mod split_point;
#[cfg(test)]
mod testing;

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct ImportFuncsInfo {
    // List of imported functions
    pub imported_funcs: Vec<ImportId>,
    pub imported_func_map: VecMap<InputFuncId>,
}

#[derive(Debug, Clone)]
pub struct DataSymbol<'a> {
    pub segment_index: DataSegmentId,
    pub symbol_index: SymbolIndex,
    pub data_in_segment: &'a DataInSegment<'a>,
    // Range relative to the start of the WebAssembly file.
    pub range: Range<usize>,
}

/// Provides a additional info about module.
/// Like data_symbols - ordered by offsets where symbol is defined (relative to module start)
/// and info about imported functions
#[derive(Clone)]
pub struct ModuleInfo<'a> {
    pub import_funcs_info: ImportFuncsInfo,
    // Symbol table with data entries sorted by offsets
    pub data_symbols: Vec<DataSymbol<'a>>,

    pub source: &'a read::InputModule<'a>,

    pub export_map: HashMap<(isize, usize), (usize, &'a str)>,
}

impl<'a> ModuleInfo<'a> {
    pub fn new(module: &'a read::InputModule<'a>) -> Result<ModuleInfo<'a>> {
        let data_symbols = get_data_symbols(
            module.data.section_payload.data_segments.as_slice(),
            &module.linking.linking_symbols.data_in_segments,
        )?;
        let imported_funcs: Vec<ImportId> = module
            .imports
            .iter()
            .enumerate()
            .filter_map(|(import_id, import)| match import.ty {
                TypeRef::Func(_) => Some(import_id as ImportId),
                _ => None,
            })
            .collect();
        let imported_func_map = imported_funcs
            .iter()
            .enumerate()
            .map(|(func_id, &import_id)| (import_id, func_id))
            .collect();
        let import_funcs_info = ImportFuncsInfo {
            imported_funcs,
            imported_func_map,
        };
        let export_map = module
            .exports
            .iter()
            .enumerate()
            .map(|(i, export)| {
                (
                    (export.kind as isize, export.index as usize),
                    (i, export.name),
                )
            })
            .collect();
        Ok(ModuleInfo {
            import_funcs_info,
            data_symbols,
            source: module,
            export_map,
        })
    }

    pub fn find_data_symbol_by_name(&self, name: &str) -> Option<&DataSymbol<'_>> {
        self.data_symbols
            .iter()
            .find(|data_symbol| data_symbol.data_in_segment.name == name)
    }

    pub fn find_function_id_by_name(&self, name: &str) -> Option<usize> {
        let func = self.source.names.functions.iter().find(|f| *f.1 == name)?;
        Some(func.0 + self.import_funcs_info.imported_funcs.len())
    }

    pub fn find_data_symbol_containing_range(
        &self,
        range: Range<usize>,
    ) -> anyhow::Result<&DataSymbol<'_>> {
        let index = Self::find_by_range(&self.data_symbols, &range, |data_symbol| {
            data_symbol.range.clone()
        })
        .with_context(|| format!("No match for data relocation range {range:?}"))?;
        let sym = &self.data_symbols[index];
        Ok(sym)
    }

    pub fn find_function_id_containing_range(&self, range: Range<usize>) -> Result<usize> {
        let func_index = Self::find_by_range(
            &self.source.code.section_payload.defined_funcs,
            &range,
            |defined_func| defined_func.body.range(),
        )
        .with_context(|| format!("No match for function relocation range {range:?}"))?;
        Ok(func_index + self.import_funcs_info.imported_funcs.len())
    }

    fn find_by_range<T: Debug, U: Debug + Ord, F: Fn(&T) -> Range<U>>(
        items: &[T],
        range: &Range<U>,
        get_range: F,
    ) -> anyhow::Result<usize> {
        let index = items
            .binary_search_by(|item| {
                let item_range = get_range(item);
                if item_range.end <= range.start {
                    Ordering::Less
                } else if item_range.start <= range.start {
                    Ordering::Equal
                } else {
                    Ordering::Greater
                }
            })
            .or_else(|index| {
                bail!(
                    "Prev range is: {:?}, next range is: {:?}",
                    index
                        .checked_sub(1)
                        .and_then(|i| items.get(i).map(|item| (item, get_range(item)))),
                    items.get(index).map(|item| (item, get_range(item)))
                )
            })?;
        if range.end > get_range(&items[index]).end {
            bail!(
                "Item {:?} has incompatible range {:?}",
                items[index],
                get_range(&items[index])
            )
        }
        Ok(index)
    }
}

fn get_data_symbols<'a>(
    data_segments: &[Data],
    symbols: &'a VecMap<Vec<DataInSegment<'a>>>,
) -> Result<Vec<DataSymbol<'a>>> {
    let mut data_symbols = Vec::new();
    for (segment_id, symbols) in symbols.iter() {
        for (symbol_index, symbol) in symbols.iter().enumerate() {
            if symbol.size == 0 {
                println!("Data segment has zero-size symbol: {:?}", symbol);
                // Ignore zero-size symbols since they cannot be the target of a relocation.
                continue;
            }
            let data_segment = data_segments
                .get(segment_id)
                .ok_or_else(|| anyhow!("Invalid data segment index in symbol: {:?}", symbol))?;
            if symbol
                .offset
                .checked_add(symbol.size)
                .ok_or_else(|| anyhow!("Invalid symbol: {symbol:?}"))? as usize
                > data_segment.data.len()
            {
                bail!(
                    "Invalid symbol {symbol:?} for data segment of size {:?}",
                    data_segment.data.len()
                );
            }
            let offset =
                data_segment.range.end - data_segment.data.len() + (symbol.offset as usize);
            let range = offset..(offset + symbol.size as usize);
            data_symbols.push(DataSymbol {
                segment_index: segment_id,
                symbol_index,
                data_in_segment: symbol,
                range,
            });
        }
    }
    data_symbols.sort_by_key(|symbol| symbol.range.start);

    // assert that segment is also sorted
    let mut last_symbol = 0;

    if cfg!(debug_assertions) {
        for symbol in &data_symbols {
            assert!(symbol.segment_index >= last_symbol);
            last_symbol = symbol.segment_index;
        }
    }

    Ok(data_symbols)
}

impl<'a> Debug for ModuleInfo<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModuleInfo")
            .field("import_funcs_info", &self.import_funcs_info)
            .field("data_symbols", &self.data_symbols)
            .finish()
    }
}
