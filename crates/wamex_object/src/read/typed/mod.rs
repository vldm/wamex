//!
//! Wasm high-level API for simplification of structured reading.
//! The root is `InputObject` struct which gives access to wasm entities in structured way.
//!
//! 1. `ElementTable` provides a way to access wasm table with elements corresponding to this table.
//! 2.
//!

use std::{cmp::Ordering, fmt::Debug, ops::Range};

use anyhow::{Context, Result, bail};
use cranelift_entity::EntityRef;
pub use entities::*;
pub use imports::*;
use wasmparser::{ElementItems, TypeRef};

use crate::{
    index::NonDefault,
    read::{
        self,
        raw::{DefinedFuncId, ElementId, FuncTypeId, ImportId},
    },
    symbols::Symbols,
};

mod data;
pub mod elements;
mod entities;
mod imports;
/// Partially parsed wasm object.
/// It expects that module has valid structure and contains additional custom sections:
/// - name section with function and global names
/// - linking section with symbol information
///
/// Unlike `read::ObjectReader` which is low-level representation of wasm module sections structure,
/// `InputObject` provides higher-level API to access wasm entities like functions and globals, in a way that concatenates imported and defined entities.
/// So user can use type-safe indexes from original module.
/// Additionally, `InputObject` expects that module has linking information and symbol names for entities.
#[derive(Debug)]
pub struct InputObject<'src> {
    pub wasm_reader: read::ObjectReader<'src>,
    pub symbols: Symbols<'src>,

    // entities
    pub functions: entities::Functions<'src>,
    pub tables: entities::Tables<'src>,
    pub memories: entities::Memories<'src>,
    pub globals: entities::Globals<'src>,
    pub tags: entities::Tags<'src>,
    // extra information
    pub indirect_function_table: elements::IndirectFunctionTable,
}

impl<'src> InputObject<'src> {
    pub fn from_wasm_bytes(wasm_bytes: &'src [u8]) -> Result<Self> {
        let reader = read::ObjectReader::parse(&wasm_bytes)?;
        Self::from_raw_module(reader)
    }
    pub fn from_raw_module(module: read::ObjectReader<'src>) -> Result<Self> {
        //TODO: Maybe we should use `IdMap` here?
        let mut imported_funcs: Vec<ImportId> = Vec::new();
        let mut imported_globals: Vec<ImportId> = Vec::new();

        let imports = imports::read_imports(&module)?;
        let exports = imports::read_exports(&module)?;

        let functions = entities::Functions::new(
            CompoundList::new(imports.0, module.code.section_payload.defined_funcs.clone()),
            module
                .names
                .functions
                .iter()
                // TODO: remove
                .map(|(id, name)| (FunctionRef::from_u32(id.as_u32()), NonDefault::from(*name)))
                .collect(),
            exports.0,
        );
        let tables = entities::Tables::new(
            CompoundList::new(imports.1, module.tables.clone()),
            module
                .names
                .tables
                .iter()
                // TODO: remove
                .map(|(id, name)| (TableRef::from_u32(id.as_u32()), NonDefault::from(*name)))
                .collect(),
            exports.1,
        );
        let memories = entities::Memories::new(
            CompoundList::new(imports.2, module.memories.clone()),
            module
                .names
                .memories
                .iter()
                // TODO: remove
                .map(|(id, name)| (MemoryRef::from_u32(id.as_u32()), NonDefault::from(*name)))
                .collect(),
            exports.2,
        );
        let globals = entities::Globals::new(
            CompoundList::new(imports.3, module.globals.clone()),
            module
                .names
                .globals
                .iter()
                // TODO: remove
                .map(|(id, name)| (GlobalRef::from_u32(id.as_u32()), NonDefault::from(*name)))
                .collect(),
            exports.3,
        );

        let tags = entities::Tags::new(
            CompoundList::new(imports.4, module.tags.clone()),
            module
                .names
                .tags
                .iter()
                // TODO: remove
                .map(|(id, name)| (TagRef::from_u32(id.as_u32()), NonDefault::from(*name)))
                .collect(),
            exports.4,
        );

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

        let (_table_name, table_id) = tables
            .iter()
            .filter_map(|(id, _)| module.names.tables.get(id).map(|name| (name.into_inner(), id)))
            .find(|(name, _)| *name == "__indirect_function_table")
            .unwrap_or_else(|| {
                assert!(
                    tables.items.defined.len() == 1,
                    "No named __indirect_function_table was found, and there is not one table in the module."
                );
                (
                    "__indirect_function_table",
                    tables.defined_iter().next().unwrap().0,
                )
            });

        let indirect_function_table =
            elements::IndirectFunctionTable::from_reader(&module, table_id, true)?;

        let symbols_map = Symbols::new(&module, functions.items.imports.len())?;

        Ok(InputObject {
            wasm_reader: module,
            symbols: symbols_map,

            indirect_function_table,

            functions,
            tables,
            memories,
            globals,
            tags,
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
    ) -> impl Iterator<Item = FunctionRef> + use<'any, 'src> {
        self.functions.iter_all_ids()
    }

    pub fn is_imported_function(&self, func_id: FunctionRef) -> bool {
        func_id.index() < self.functions.items.imports.len()
    }

    pub fn as_defined_function_id(&self, func_id: FunctionRef) -> Option<DefinedFuncId> {
        if self.is_imported_function(func_id) {
            None
        } else {
            Some(DefinedFuncId::from_u32(
                func_id.index() as u32 - self.functions.items.imports.len() as u32,
            ))
        }
    }

    pub fn get_function_type_id(&self, func_id: FunctionRef) -> FuncTypeId {
        let func = self.functions.items.get_entity(func_id);
        match func {
            ImportOrDefined::Defined(defined) => defined.type_id,
            ImportOrDefined::Import(import) => import.type_id,
        }
    }

    pub fn find_function_id_by_name(&self, name: &str) -> Option<FunctionRef> {
        let func = self
            .wasm_reader
            .names
            .functions
            .iter()
            .find(|f| **f.1 == name)?;
        Some(func.0)
    }

    pub fn find_global_id_by_name(&self, name: &str) -> Option<GlobalRef> {
        let global = self
            .wasm_reader
            .names
            .globals
            .iter()
            .find(|f| **f.1 == name)?;
        Some(global.0)
    }

    pub fn find_function_id_containing_range(&self, range: Range<usize>) -> Result<FunctionRef> {
        let func_index = Self::find_by_range(
            self.wasm_reader
                .code
                .section_payload
                .defined_funcs
                .as_values_slice(),
            &range,
            |defined_func| defined_func.body.range(),
        )
        .with_context(|| format!("No match for function relocation range {range:?}"))?;
        Ok(FunctionRef::new(
            func_index + self.functions.items.imports.len(),
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
