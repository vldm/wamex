//!
//! Imports for wasm entities.
//!
//! as bonus it contains export entries representation =)
//!

use std::borrow::Cow;

use cranelift_entity::EntityRef;
use wasmparser::TypeRef;

use crate::{
    index::IdVec,
    read::{
        FuncTypeId,
        typed::entities::{FunctionRef, GlobalRef, MemoryRef, TableRef, TagRef},
    },
};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct ExportEntry<'a, IDX: EntityRef> {
    pub name: Cow<'a, str>,
    pub entity_index: IDX,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct ImportedFunction<'a> {
    pub module_name: Cow<'a, str>,
    pub func_name: Cow<'a, str>,
    pub func_type: wasmparser::FuncType,
}

// No Ord in wasmparser::TableType
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ImportedTable<'a> {
    pub module_name: Cow<'a, str>,
    pub table_name: Cow<'a, str>,
    pub table_type: wasmparser::TableType,
}

// No Ord in wasmparser::GlobalType
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ImportedMemory<'a> {
    pub module_name: Cow<'a, str>,
    pub memory_name: Cow<'a, str>,
    pub memory_type: wasmparser::MemoryType,
}

// No Ord in wasmparser::GlobalType
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ImportedGlobal<'a> {
    pub module_name: Cow<'a, str>,
    pub global_name: Cow<'a, str>,
    pub global_type: wasmparser::GlobalType,
}

// No Ord and Hash in wasmparser::TagType
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedTag<'a> {
    pub module_name: Cow<'a, str>,
    pub tag_name: Cow<'a, str>,
    pub tag_type: wasmparser::TagType,
}

impl_entity_index! {
    pub struct ImportedFuncId(for<'a> ImportedFunction<'a>);
    pub struct ImportedTableId(for<'a> ImportedTable<'a>);
    pub struct ImportedMemoryId(for<'a> ImportedMemory<'a>);
    pub struct ImportedGlobalId(for<'a> ImportedGlobal<'a>);
    pub struct ImportedTagId(for<'a> ImportedTag<'a>);
}

pub fn read_imports<'a>(
    reader: &crate::read::raw::ObjectReader<'a>,
) -> crate::Result<(
    IdVec<ImportedFunction<'a>>,
    IdVec<ImportedTable<'a>>,
    IdVec<ImportedMemory<'a>>,
    IdVec<ImportedGlobal<'a>>,
    IdVec<ImportedTag<'a>>,
)> {
    let mut imported_funcs: IdVec<ImportedFunction<'a>> = IdVec::new();
    let mut imported_tables: IdVec<ImportedTable<'a>> = IdVec::new();
    let mut imported_memories: IdVec<ImportedMemory<'a>> = IdVec::new();
    let mut imported_globals: IdVec<ImportedGlobal<'a>> = IdVec::new();
    let mut imported_tags: IdVec<ImportedTag<'a>> = IdVec::new();
    for (_import_id, import) in reader.imports.iter() {
        match import.ty {
            TypeRef::Func(num) => {
                imported_funcs.push(ImportedFunction {
                    module_name: import.module.into(),
                    func_name: import.name.into(),
                    func_type: reader
                        .types
                        .get(FuncTypeId::from_u32(num))
                        .expect("Function type must exist")
                        .clone(),
                });
            }
            TypeRef::Table(ref table_type) => {
                imported_tables.push(ImportedTable {
                    module_name: import.module.into(),
                    table_name: import.name.into(),
                    table_type: table_type.clone(),
                });
            }
            TypeRef::Memory(ref memory_type) => {
                imported_memories.push(ImportedMemory {
                    module_name: import.module.into(),
                    memory_name: import.name.into(),
                    memory_type: memory_type.clone(),
                });
            }
            TypeRef::Global(ref global_type) => {
                imported_globals.push(ImportedGlobal {
                    module_name: import.module.into(),
                    global_name: import.name.into(),
                    global_type: global_type.clone(),
                });
            }
            TypeRef::Tag(ref tag_type) => {
                imported_tags.push(ImportedTag {
                    module_name: import.module.into(),
                    tag_name: import.name.into(),
                    tag_type: tag_type.clone(),
                });
            }
        }
    }
    Ok((
        imported_funcs,
        imported_tables,
        imported_memories,
        imported_globals,
        imported_tags,
    ))
}

pub fn read_exports<'a>(
    reader: &crate::read::raw::ObjectReader<'a>,
) -> crate::Result<(
    Vec<ExportEntry<'a, FunctionRef>>,
    Vec<ExportEntry<'a, TableRef>>,
    Vec<ExportEntry<'a, MemoryRef>>,
    Vec<ExportEntry<'a, GlobalRef>>,
    Vec<ExportEntry<'a, TagRef>>,
)> {
    let mut func_exports: Vec<ExportEntry<'a, FunctionRef>> = Vec::new();
    let mut table_exports: Vec<ExportEntry<'a, TableRef>> = Vec::new();
    let mut memory_exports: Vec<ExportEntry<'a, MemoryRef>> = Vec::new();
    let mut global_exports: Vec<ExportEntry<'a, GlobalRef>> = Vec::new();
    let mut tag_exports: Vec<ExportEntry<'a, TagRef>> = Vec::new();
    for (_export_id, export) in reader.exports.iter() {
        match export.kind {
            wasmparser::ExternalKind::Func => {
                func_exports.push(ExportEntry {
                    name: export.name.into(),
                    entity_index: FunctionRef::from_u32(export.index),
                });
            }
            wasmparser::ExternalKind::Table => {
                table_exports.push(ExportEntry {
                    name: export.name.into(),
                    entity_index: TableRef::from_u32(export.index),
                });
            }
            wasmparser::ExternalKind::Memory => {
                memory_exports.push(ExportEntry {
                    name: export.name.into(),
                    entity_index: MemoryRef::from_u32(export.index),
                });
            }
            wasmparser::ExternalKind::Global => {
                global_exports.push(ExportEntry {
                    name: export.name.into(),
                    entity_index: GlobalRef::from_u32(export.index),
                });
            }
            wasmparser::ExternalKind::Tag => {
                tag_exports.push(ExportEntry {
                    name: export.name.into(),
                    entity_index: TagRef::from_u32(export.index),
                });
            }
        }
    }
    Ok((
        func_exports,
        table_exports,
        memory_exports,
        global_exports,
        tag_exports,
    ))
}
