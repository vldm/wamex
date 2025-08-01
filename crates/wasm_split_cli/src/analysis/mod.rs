//!
//! Module with external info usefull to build dep graph, and request information about function and data entries.
//!

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::Debug;
use std::ops::Range;

use anyhow::{anyhow, bail, ensure, Context, Result};
use wasmparser::{Data, ElementItems, ElementKind, TypeRef};

use crate::read::{self, linking::section::DataInSegment};

use crate::index::{
    DataSegmentId, DataSymbolId, DefinedFuncId, ElementId, ExportId, IdMap, IdVec, ImportId,
    InputFuncId, SymbolId, TableId,
};

mod debug;
pub mod dep_graph;
pub mod split_point;
#[cfg(test)]
mod testing;

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct ImportFuncsInfo {
    // List of imported functions
    pub imported_funcs: Vec<ImportId>,
    pub imported_func_map: IdMap<ImportId, InputFuncId>,
}

#[derive(Debug, Clone)]
pub struct DataSymbol<'a> {
    pub segment_index: DataSegmentId,
    pub symbol_index: DataSymbolId,
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
    pub export_map: HashMap<(isize, SymbolId), (ExportId, &'a str)>,

    pub indirect_function_table_id: (TableId, ElementId),
    pub indirect_function_list: Vec<InputFuncId>,
}

impl<'a> ModuleInfo<'a> {
    pub fn new(module: &'a read::InputModule<'a>) -> Result<ModuleInfo<'a>> {
        let data_symbols = get_data_symbols(
            module.data.section_payload.data_segments.as_slice(),
            &module.linking.linking_symbols.data_in_segments,
        )?;
        //TODO: Maybe we should use `IdMap` here?
        let imported_funcs: Vec<ImportId> = module
            .imports
            .iter()
            .filter_map(|(import_id, import)| match import.ty {
                TypeRef::Func(_) => Some(import_id),
                _ => None,
            })
            .collect();
        let imported_func_map = imported_funcs
            .iter()
            .enumerate()
            .map(|(func_id, &import_id)| (import_id, InputFuncId::from_index(func_id)))
            .collect();
        let import_funcs_info = ImportFuncsInfo {
            imported_funcs,
            imported_func_map,
        };
        let export_map = module
            .exports
            .iter()
            .map(|(i, export)| {
                (
                    (export.kind as isize, export.index as SymbolId),
                    (i, export.name),
                )
            })
            .collect();

        let (_table_name, table_id) = module
            .tables
            .iter()
            .filter_map(|(id, _)| module.names.tables.get(id).map(|name| (*name, id)))
            .find(|(name, _)| *name == "__indirect_function_table")
            .unwrap_or_else(|| {
                assert!(
                    module.tables.len() == 1,
                    "No named __indirect_function_table was found, and there is not one table in the module."
                );
                (
                    "__indirect_function_table",
                    module.tables.iter().next().unwrap().0,
                )
            });

        let mut indirect_element = None;
        for (id, element) in module.elements.iter() {
            let ElementKind::Active {
                table_index,
                offset_expr,
            } = &element.kind
            else {
                continue;
            };

            if !table_index.is_none()  // None for first index.
               && table_index.unwrap() == table_id.as_raw_index() as u32
            {
                continue;
            }

            let offset = Self::read_const_expr(offset_expr)
                .with_context(|| format!("Failed to read offset expression for element {id:?}"))?;

            ensure!(
                offset == 1,
                "Element segment {id:?} should be inited with 1 offset, but got {offset}, which is not supported"
            );

            let ElementItems::Functions(functions) = &element.items else {
                bail!("Only function elements are supported, but got constant instead");
            };

            let mut function_list = Vec::with_capacity(functions.count() as usize);
            for function_id in functions.clone().into_iter() {
                let raw_function_id = function_id
                    .with_context(|| format!("Failed to read function ID from element {id:?}"))?
                    as u32;
                function_list.push(InputFuncId::from_index(raw_function_id));
            }
            indirect_element = Some((id, function_list));
            break;
        }
        let (indirect_element_id, indirect_function_list) = indirect_element
            .ok_or_else(|| anyhow!("No element segment with __indirect_function_table found"))?;

        Ok(ModuleInfo {
            import_funcs_info,
            data_symbols,
            source: module,
            export_map,
            indirect_function_list,
            indirect_function_table_id: (table_id, indirect_element_id),
        })
    }

    fn read_const_expr(offset_expr: &wasmparser::ConstExpr<'a>) -> Result<i32> {
        let mut reader = offset_expr.get_operators_reader();

        let val = match reader.read()? {
            wasmparser::Operator::I32Const { value } => Ok(value),
            op => bail!("Expected only I32.const operator, found: {:?}", op),
        };
        match reader.read()? {
            wasmparser::Operator::End => {}
            op => bail!("Expected End after I32.const: {:?}", op),
        }
        return val;
    }

    pub fn is_imported_function(&self, func_id: InputFuncId) -> bool {
        func_id.as_raw_index() < self.import_funcs_info.imported_funcs.len()
    }

    pub fn as_defined_function_id(&self, func_id: InputFuncId) -> Option<DefinedFuncId> {
        if self.is_imported_function(func_id) {
            None
        } else {
            Some(DefinedFuncId::from_index(
                func_id
                    .as_raw_index()
                    .checked_sub(self.import_funcs_info.imported_funcs.len())
                    .expect("Function ID is out of bounds") as u32,
            ))
        }
    }

    pub fn get_function_import_id(&self, func_id: InputFuncId) -> Option<ImportId> {
        self.import_funcs_info
            .imported_funcs
            .get(func_id.as_raw_index())
            .copied()
    }

    pub fn find_data_symbol_by_name(&self, name: &str) -> Option<&DataSymbol<'_>> {
        self.data_symbols
            .iter()
            .find(|data_symbol| data_symbol.data_in_segment.name == name)
    }

    pub fn find_function_id_by_name(&self, name: &str) -> Option<InputFuncId> {
        let func = self.source.names.functions.iter().find(|f| *f.1 == name)?;
        Some(func.0)
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

    pub fn find_function_id_containing_range(&self, range: Range<usize>) -> Result<InputFuncId> {
        let func_index = Self::find_by_range(
            &self.source.code.section_payload.defined_funcs.as_slice(),
            &range,
            |defined_func| defined_func.body.range(),
        )
        .with_context(|| format!("No match for function relocation range {range:?}"))?;
        Ok(InputFuncId::from_index(
            func_index + self.import_funcs_info.imported_funcs.len(),
        ))
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
    data: &[Data],
    symbols: &'a IdMap<DataSegmentId, IdVec<DataInSegment<'a>>>,
) -> Result<Vec<DataSymbol<'a>>> {
    let mut data_symbols = Vec::new();
    for (segment_id, symbols) in symbols.iter() {
        for (symbol_index, symbol) in symbols.iter() {
            if symbol.size == 0 {
                log::warn!("Data segment has zero-size symbol: {:?}", symbol);
                // Ignore zero-size symbols since they cannot be the target of a relocation.
                continue;
            }
            let data_segment = data
                .get(segment_id.as_raw_index())
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
    if cfg!(debug_assertions) {
        let mut last_symbol = 0;
        for symbol in &data_symbols {
            assert!(symbol.segment_index.as_raw_index() >= last_symbol);
            last_symbol = symbol.segment_index.as_raw_index();
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
