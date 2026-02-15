use std::borrow::Cow;

use cranelift_entity::EntityRef;
use wasmparser::TypeRef;

use super::{FunctionRef, GlobalRef, MemoryRef, TableRef, TagRef};
use crate::{
    SVec,
    linkage::reloc::RelocationEntry,
    typed::{FnTypeRef, common_index::ErasedEntityRef},
};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct ExportEntry<'a, IDX: EntityRef> {
    pub name: Cow<'a, str>,
    pub entity_index: IDX,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct ImportedEntity<'a, Type> {
    pub module: Cow<'a, str>,
    pub name: Cow<'a, str>,
    pub entity_type: Type,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DefinedEntity<'src, Type> {
    pub entity_type: Type,
    pub body: EntityDefinition<'src>,
}

pub type ImportedFunction<'a> = ImportedEntity<'a, FnTypeRef>;
pub type ImportedTable<'a> = ImportedEntity<'a, wasmparser::TableType>;
pub type ImportedMemory<'a> = ImportedEntity<'a, wasmparser::MemoryType>;
pub type ImportedGlobal<'a> = ImportedEntity<'a, wasmparser::GlobalType>;
pub type ImportedTag<'a> = ImportedEntity<'a, wasmparser::TagType>;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Rewrite {
    pub old_range: std::ops::Range<usize>,
    // TODO: Maybe put in common pool?
    pub new_relocs: SVec<RelocationEntry<ErasedEntityRef>, 2>,
    pub new_bytes: SVec<u8, 16>,
}

/// Body of a defined entity
/// For functions it's locals + instructions.
/// For tables/memories/globals/tags it's the initializers.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EntityDefinition<'src> {
    // Copy of original entity with optilan applied patches.
    Copied {
        body: &'src [u8],
        patches: Vec<Rewrite>,
        // TODO: Consider using &[..] with bitmask to indicate which relocations should be ignored.
        original_relocs: Vec<RelocationEntry<ErasedEntityRef>>,
    },
    // IndirectTrampoline to import | or regular Trampoline
    New {
        // just one big patch that generates the entire function body.
        body: Rewrite,
    },
}

// impl DefinedFunction {
//     pub fn generate_function(
//         &self,
//         function_type_id: FuncTypeId,
//         section: &mut wasm_encoder::CodeSection,
//     ) -> crate::Result<()> {
//         todo!()
//     }
// }

pub fn read_imports<'a>(
    reader: &crate::raw::ObjectReader<'a>,
) -> crate::Result<(
    Vec<ImportedFunction<'a>>,
    Vec<ImportedTable<'a>>,
    Vec<ImportedMemory<'a>>,
    Vec<ImportedGlobal<'a>>,
    Vec<ImportedTag<'a>>,
)> {
    let mut imported_funcs: Vec<ImportedFunction<'a>> = Vec::new();
    let mut imported_tables: Vec<ImportedTable<'a>> = Vec::new();
    let mut imported_memories: Vec<ImportedMemory<'a>> = Vec::new();
    let mut imported_globals: Vec<ImportedGlobal<'a>> = Vec::new();
    let mut imported_tags: Vec<ImportedTag<'a>> = Vec::new();
    for (_import_id, import) in reader.imports.iter() {
        match import.ty {
            TypeRef::Func(num) => {
                imported_funcs.push(ImportedFunction {
                    module: import.module.into(),
                    name: import.name.into(),
                    entity_type: FnTypeRef::from_u32(num),
                });
            }
            TypeRef::Table(ref table_type) => {
                imported_tables.push(ImportedTable {
                    module: import.module.into(),
                    name: import.name.into(),
                    entity_type: table_type.clone(),
                });
            }
            TypeRef::Memory(ref memory_type) => {
                imported_memories.push(ImportedMemory {
                    module: import.module.into(),
                    name: import.name.into(),
                    entity_type: memory_type.clone(),
                });
            }
            TypeRef::Global(ref global_type) => {
                imported_globals.push(ImportedGlobal {
                    module: import.module.into(),
                    name: import.name.into(),
                    entity_type: global_type.clone(),
                });
            }
            TypeRef::Tag(ref tag_type) => {
                imported_tags.push(ImportedTag {
                    module: import.module.into(),
                    name: import.name.into(),
                    entity_type: tag_type.clone(),
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
    reader: &crate::raw::ObjectReader<'a>,
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

#[cfg(test)]
mod test {
    #[test]
    fn size_of_defined_entity_body() {
        use super::{EntityDefinition, Rewrite};
        // assert_eq!(std::mem::size_of::<Rewrite>(), 48);
        assert_eq!(std::mem::size_of::<EntityDefinition>(), 48);
    }
}
