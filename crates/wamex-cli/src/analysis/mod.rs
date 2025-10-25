//!
//! Module with external info usefull to build dep graph, and request information about function and data entries.
//!

use std::{cmp::Ordering, collections::HashMap, fmt::Debug, ops::Range};

use anyhow::{anyhow, bail, ensure, Context, Result};
use wasmparser::{Data, ElementItems, ElementKind, TypeRef};

use crate::{
    helpers::RangeExt,
    index::{
        AnySymbolId, DataSegmentId, DataSymbolId, DefinedFuncId, ElementId, ExportId, FuncTypeId,
        IdMap, IdVec, ImportId, InputFuncId, InputGlobalId, TableId,
    },
    read::{self, linking::section::DataInSegment},
};
mod debug;
pub mod dep_graph;
pub mod split_point;
#[cfg(test)]
mod testing;

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct ImportInfo {
    // List of imported functions
    pub imported_funcs: Vec<ImportId>,
    pub imported_func_map: IdMap<ImportId, InputFuncId>,

    pub imported_globals: Vec<ImportId>,
    pub imported_global_map: IdMap<ImportId, InputGlobalId>,
}

#[derive(Debug, Clone)]
pub struct DataSymbol<'a, 'src> {
    pub segment_index: DataSegmentId,
    pub symbol_index: DataSymbolId,
    pub data_in_segment: &'a DataInSegment<'src>,
    // Range relative to the start of the WebAssembly file.
    pub range: Range<usize>,
}

/// Provides a additional info about module.
/// Like data_symbols - ordered by offsets where symbol is defined (relative to module start)
/// and info about imported functions
#[derive(Clone)]
pub struct ModuleInfo<'a, 'src> {
    pub import_info: ImportInfo,
    // Symbol table with data entries sorted by offsets
    // TODO: also re-build symbol table?
    pub data_symbols: Vec<DataSymbol<'a, 'src>>,

    pub wasm: &'a read::InputModule<'src>,
    pub export_map: HashMap<(isize, AnySymbolId), (ExportId, &'src str)>,

    pub indirect_function_table_id: (TableId, ElementId),
    pub indirect_function_list: Vec<InputFuncId>,
}

impl<'a, 'src> ModuleInfo<'a, 'src> {
    pub fn new(module: &'a read::InputModule<'src>) -> Result<ModuleInfo<'a, 'src>> {
        let data_symbols = get_data_symbols(
            module.data.section_payload.data_segments.as_slice(),
            &module.linking.linking_symbols.data_in_segments,
        )?;
        //TODO: Maybe we should use `IdMap` here?
        let mut imported_funcs: Vec<ImportId> = Vec::new();
        let mut imported_globals: Vec<ImportId> = Vec::new();

        for (import_id, import) in module.imports.iter() {
            match import.ty {
                TypeRef::Global(_) => {
                    imported_globals.push(import_id);
                    continue;
                }
                TypeRef::Func(_) => {
                    imported_funcs.push(import_id);
                }
                _ => {}
            }
        }
        let imported_func_map = imported_funcs
            .iter()
            .enumerate()
            .map(|(func_id, &import_id)| (import_id, InputFuncId::from_index(func_id)))
            .collect();
        let imported_global_map = imported_globals
            .iter()
            .enumerate()
            .map(|(global_id, &import_id)| (import_id, InputGlobalId::from_index(global_id)))
            .collect();

        let import_funcs_info = ImportInfo {
            imported_funcs,
            imported_func_map,
            imported_globals,
            imported_global_map,
        };
        let export_map = module
            .exports
            .iter()
            .map(|(i, export)| {
                (
                    (export.kind as isize, export.index as AnySymbolId),
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
                    .with_context(|| format!("Failed to read function ID from element {id:?}"))?;
                function_list.push(InputFuncId::from_index(raw_function_id));
            }
            indirect_element = Some((id, function_list));
            break;
        }
        let (indirect_element_id, indirect_function_list) = indirect_element
            .ok_or_else(|| anyhow!("No element segment with __indirect_function_table found"))?;

        Ok(ModuleInfo {
            import_info: import_funcs_info,
            data_symbols,
            wasm: module,
            export_map,
            indirect_function_list,
            indirect_function_table_id: (table_id, indirect_element_id),
        })
    }

    pub(crate) fn read_const_expr(offset_expr: &wasmparser::ConstExpr<'a>) -> Result<i32> {
        let mut reader = offset_expr.get_operators_reader();

        let val = match reader.read()? {
            wasmparser::Operator::I32Const { value } => Ok(value),
            op => bail!("Expected only I32.const operator, found: {:?}", op),
        };
        match reader.read()? {
            wasmparser::Operator::End => {}
            op => bail!("Expected End after I32.const: {:?}", op),
        }
        val
    }
    pub fn function_id_iter<'any>(
        &'any self,
    ) -> impl Iterator<Item = InputFuncId> + use<'any, 'src> {
        (0..self.import_info.imported_funcs.len())
            .map(InputFuncId::from_index)
            .chain(
                self.wasm
                    .code
                    .section_payload
                    .defined_funcs
                    .iter()
                    .enumerate()
                    .map(|(index, _)| {
                        InputFuncId::from_index(index + self.import_info.imported_funcs.len())
                    }),
            )
    }

    pub fn is_imported_function(&self, func_id: InputFuncId) -> bool {
        func_id.as_raw_index() < self.import_info.imported_funcs.len()
    }

    pub fn as_defined_function_id(&self, func_id: InputFuncId) -> Option<DefinedFuncId> {
        if self.is_imported_function(func_id) {
            None
        } else {
            Some(DefinedFuncId::from_index(
                func_id
                    .as_raw_index()
                    .checked_sub(self.import_info.imported_funcs.len())
                    .expect("Function ID is out of bounds") as u32,
            ))
        }
    }

    pub fn get_function_type_id(&self, func_id: InputFuncId) -> FuncTypeId {
        let Some(defined_index) = self.as_defined_function_id(func_id) else {
            // It's import function - recover from import id.
            let import_id = self.import_info.imported_funcs[func_id.as_raw_index()];
            let TypeRef::Func(ty) = self.wasm.imports[import_id].ty else {
                panic!("Expected function type")
            };
            return FuncTypeId::from_index(ty);
        };
        // It's a defined function.
        self.wasm.defined_func_type_id(defined_index)
    }

    pub fn get_function_import_id(&self, func_id: InputFuncId) -> Option<ImportId> {
        self.import_info
            .imported_funcs
            .get(func_id.as_raw_index())
            .copied()
    }
    pub fn get_global_import_id(&self, global_id: InputGlobalId) -> Option<ImportId> {
        self.import_info
            .imported_globals
            .get(global_id.as_raw_index())
            .copied()
    }

    pub fn find_data_symbol_by_name(&self, name: &str) -> Option<&DataSymbol<'_, '_>> {
        self.data_symbols
            .iter()
            .find(|data_symbol| data_symbol.data_in_segment.name == name)
    }

    pub fn find_function_id_by_name(&self, name: &str) -> Option<InputFuncId> {
        let func = self.wasm.names.functions.iter().find(|f| *f.1 == name)?;
        Some(func.0)
    }

    pub fn find_global_id_by_name(&self, name: &str) -> Option<InputGlobalId> {
        let global = self.wasm.names.globals.iter().find(|f| *f.1 == name)?;
        Some(global.0)
    }

    pub fn find_data_symbol_containing_range(
        &self,
        range: Range<usize>,
    ) -> anyhow::Result<&DataSymbol<'_, '_>> {
        let index = Self::find_by_range(&self.data_symbols, &range, |data_symbol| {
            data_symbol.range.clone()
        })
        .with_context(|| format!("No match for data relocation range {range:?}"))?;
        let sym = &self.data_symbols[index];
        Ok(sym)
    }

    pub fn find_function_id_containing_range(&self, range: Range<usize>) -> Result<InputFuncId> {
        let func_index = Self::find_by_range(
            self.wasm.code.section_payload.defined_funcs.as_slice(),
            &range,
            |defined_func| defined_func.body.range(),
        )
        .with_context(|| format!("No match for function relocation range {range:?}"))?;
        Ok(InputFuncId::from_index(
            func_index + self.import_info.imported_funcs.len(),
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

fn get_data_symbols<'a, 'src>(
    data: &[Data],
    symbols: &'a IdMap<DataSegmentId, IdVec<DataInSegment<'src>>>,
) -> Result<Vec<DataSymbol<'a, 'src>>> {
    let mut data_symbols = Vec::new();
    for (segment_id, symbols) in symbols.iter() {
        let data_segment = data
            .get(segment_id.as_raw_index())
            .ok_or_else(|| anyhow!("No data found for data segment: {:?}", segment_id))?;

        let data_segment_start = data_segment.range.end - data_segment.data.len();
        for (symbol_index, symbol) in symbols.iter() {
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

            let offset = data_segment_start + (symbol.offset as usize);
            let range = offset..(offset + symbol.size as usize);
            data_symbols.push(DataSymbol {
                segment_index: segment_id,
                symbol_index,
                data_in_segment: symbol,
                range,
            });
        }
    }
    data_symbols.sort_by(|left, right|
        left.range.cmp_range(&right.range)
        .as_partial_ordering()
        .expect("Failed to compare symbol ranges, this means that some symbol partially intesects with another symbol, which is not allowed")
    );

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

impl<'a, 'src> Debug for ModuleInfo<'a, 'src> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModuleInfo")
            .field("import_funcs_info", &self.import_info)
            .field("data_symbols", &self.data_symbols)
            .finish()
    }
}
