use std::collections::BTreeMap;

use cranelift_entity::PrimaryMap;
use wasmparser::GlobalType;

use crate::typed::{EntityType, snapshot::FlatEntityRef};

impl_entity_index! {
    #[display = "new_import"]
    pub struct NewImportRef;
}
/// Content plan for a single emitted module.
#[derive(Clone, Debug)]
pub struct OutputModuleCopyPlan {
    /// Entities that copied as is from original module (can be imported or defined).
    /// Can have additional export names.
    pub entities: BTreeMap<FlatEntityRef, CopySpec>,
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
    // The reference to original entity in input module.
    // Used to create process relocations existing in copied entities.
    pub original_entity: Option<FlatEntityRef>,
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
            original_entity: None,
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
            original_entity: None,
        }
    }
    pub fn wamex_import(name: String, ty: EntityType, entity: FlatEntityRef) -> Self {
        Self {
            module: Self::WAMEX_DEFAULT_MODULE.to_string(),
            name,
            ty,
            original_entity: Some(entity),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CopySpec {
    AsIs,
    WithExport { export_name: String },
}

impl CopySpec {
    // we can add prefix or simplify name,
    // but if we modify name - we should keep special entities recognizable (__memory_base, __table_base, __stack_pointer, __indirect_function_table)
    pub fn wamex_export(export_name: String) -> Self {
        Self::WithExport { export_name }
    }
    pub fn export_as(&self) -> Option<&str> {
        match self {
            CopySpec::AsIs => None,
            CopySpec::WithExport { export_name } => Some(export_name.as_str()),
        }
    }
}

// pub fn add_wamex_prefix(name: &str) -> String {
//     format!("__wamex_{}", name)
// }

pub type PlannedGotInfo = super::DyLinkDeps<NewImportRef>;
#[derive(Clone, Debug)]
pub enum AddressingMode {
    Static,
    GotRelative(PlannedGotInfo),
}

pub type OutputId = String;
