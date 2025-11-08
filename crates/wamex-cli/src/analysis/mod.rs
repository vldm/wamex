//!
//! Module with external info usefull to build dep graph, and request information about function and data entries.
//!

use std::{cmp::Ordering, collections::HashMap, fmt::Debug, ops::Range};

use anyhow::{Context, Result, anyhow, bail, ensure};
pub use symbols::SymbolMap;
use wasmparser::{ElementItems, ElementKind, TypeRef};

use crate::{
    index::{
        AnySymbolId, DefinedFuncId, ElementId, ExportId, FuncTypeId, IdMap, ImportId, InputFuncId,
        InputGlobalId, TableId,
    },
    read,
};
mod debug;
pub mod dep_graph;
pub mod split_point;
pub mod symbols;
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

/// Provides a additional info about module.
/// Like ordered_data_symbols - ordered by offsets where symbol is defined (relative to module start)
/// and info about imported functions
pub struct ModuleInfo<'src> {
    pub import_info: ImportInfo,

    pub wasm: read::InputModule<'src>,
    pub export_map: HashMap<(isize, AnySymbolId), (ExportId, &'src str)>,
    pub symbols: SymbolMap<'src>,

    pub indirect_function_table_id: (TableId, ElementId),
    pub indirect_function_list: Vec<InputFuncId>,
}

impl<'src> ModuleInfo<'src> {
    pub fn from_wasm_bytes(wasm_bytes: &'src [u8]) -> Result<Self> {
        let module = read::InputModule::parse(&wasm_bytes)?;
        Self::from_raw_module(module)
    }
    pub fn from_raw_module(module: read::InputModule<'src>) -> Result<Self> {
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
        dbg!(&indirect_element);
        let (indirect_element_id, indirect_function_list) = indirect_element
            .ok_or_else(|| anyhow!("No element segment with __indirect_function_table found"))?;

        let symbols_map = symbols::SymbolMap::new(&module, import_funcs_info.imported_funcs.len())?;

        Ok(ModuleInfo {
            import_info: import_funcs_info,
            symbols: symbols_map,
            wasm: module,
            export_map,
            indirect_function_list,
            indirect_function_table_id: (table_id, indirect_element_id),
        })
    }

    pub(crate) fn read_const_expr(offset_expr: &wasmparser::ConstExpr<'_>) -> Result<i32> {
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

    pub fn find_function_id_by_name(&self, name: &str) -> Option<InputFuncId> {
        let func = self.wasm.names.functions.iter().find(|f| *f.1 == name)?;
        Some(func.0)
    }

    pub fn find_global_id_by_name(&self, name: &str) -> Option<InputGlobalId> {
        let global = self.wasm.names.globals.iter().find(|f| *f.1 == name)?;
        Some(global.0)
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

impl<'src> Debug for ModuleInfo<'src> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModuleInfo")
            .field("import_funcs_info", &self.import_info)
            .finish()
    }
}
