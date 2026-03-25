use std::collections::BTreeMap;

use cranelift_entity::{PrimaryMap, SecondaryMap};
use wasmparser::{FuncType, GlobalType, MemoryType, TableType, TagType};

use crate::{
    raw::FuncTypeId,
    typed::{FileId, FileLoader, ImportedEntity, snapshot::FlatEntityRef},
};

impl_entity_index! {
    #[display = "new_import"]
    pub struct NewImportRef;
}
/// Content plan for a single emitted module.
#[derive(Clone, Debug)]
pub struct OutputModuleCopyPlan {
    /// Entities that copied as is from original module (can be imported or defined).
    /// Can have additional export names.
    pub entities: BTreeMap<FlatEntityRef, CopyEntity>,
    /// New created imports.
    pub imports: PrimaryMap<NewImportRef, ImportSpec>,
    /// Addressing mode for the module.
    pub addressing: AddressingMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportSpec {
    pub module: String,
    pub name: String,
    pub ty: EntityType,
}
impl ImportSpec {
    pub const WAMEX_DEFAULT_MODULE: &'static str = "__wamex";
    pub fn memory_base(extern_prefix: &str) -> Self {
        Self {
            module: extern_prefix.to_string(),
            name: format!("_{}_memory_base", extern_prefix),
            ty: EntityType::Global(GlobalType {
                content_type: wasmparser::ValType::I32,
                mutable: false,
                shared: false,
            }),
        }
    }
    pub fn table_base(extern_prefix: &str) -> Self {
        Self {
            module: extern_prefix.to_string(),
            name: format!("_{}_table_base", extern_prefix),
            ty: EntityType::Global(GlobalType {
                content_type: wasmparser::ValType::I32,
                mutable: false,
                shared: false,
            }),
        }
    }
}

/// The entity type for imports and exports of a module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityType {
    /// The entity is a function.
    Function(FuncType),
    /// The entity is a table.
    Table(TableType),
    /// The entity is a memory.
    Memory(MemoryType),
    /// The entity is a global.
    Global(GlobalType),
    /// The entity is a tag.
    Tag(TagType),
    /// The data symbol
    DataSymbol(()),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CopyEntity {
    AsIs,
    WithExport { export_name: String },
}

pub type PlannedGotInfo = super::DyLinkDeps<NewImportRef>;
#[derive(Clone, Debug)]
pub enum AddressingMode {
    Static,
    GotRelative(PlannedGotInfo),
}

pub type OutputId = String;
