use std::borrow::Cow;

use super::modify;
use crate::{emit::ImportedEntity, read::FunctionRef};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefinedFunctionKind<'src> {
    Copied {
        // List of modifications that should be applied to this function.
        modifications: modify::newgen::CodeModifyResult<'src>,
    },
    IndirectTrampoline {
        /// Index of extra table entry after main module entries.
        table_index_offset: u32,
    },
    // Stub function generated for imported functions
    Trampoline {},
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinedFunction<'src> {
    pub(crate) export: bool,
    pub(crate) input_func_id: FunctionRef,
    pub(crate) kind: DefinedFunctionKind<'src>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ImportedFunction<'a> {
    pub(crate) input_func_id: FunctionRef,
    pub(crate) kind: ImportFunctionKind<'a>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ImportFunctionKind<'a> {
    // use existing import function
    Existing(crate::read::typed::ImportedFunction<'a>),
    // Add new import function from another module (e.g. main module).
    New {
        link_module: usize,
        output_function_index: usize,
        mangled_function_name: &'a str,
    },
}
impl ImportedEntity for ImportedFunction<'_> {
    fn import_name(&self) -> Cow<'_, str> {
        match self.kind {
            ImportFunctionKind::Existing(crate::read::typed::ImportedFunction {
                ref func_name,
                ..
            }) => func_name.clone(),
            ImportFunctionKind::New {
                mangled_function_name,
                ..
            } => format!("__wamex_{}", mangled_function_name).into(),
        }
    }

    fn module_name(&self) -> Cow<'_, str> {
        match self.kind {
            ImportFunctionKind::Existing(crate::read::typed::ImportedFunction {
                ref module_name,
                ..
            }) => module_name.clone(),
            ImportFunctionKind::New { .. } => {
                "__wamex".into()
                // format!("__wamex_link_{}", link_module)
            }
        }
    }
}

impl ImportedFunction<'_> {
    pub fn input_func_id(&self) -> FunctionRef {
        self.input_func_id
    }
}

// ignore relocations field in order
impl<'src> Ord for DefinedFunction<'src> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let tag = match self.kind {
            DefinedFunctionKind::Copied { .. } => 0,
            DefinedFunctionKind::IndirectTrampoline { .. } => 1,
            DefinedFunctionKind::Trampoline { .. } => 2,
        };
        let other_tag = match other.kind {
            DefinedFunctionKind::Copied { .. } => 0,
            DefinedFunctionKind::IndirectTrampoline { .. } => 1,
            DefinedFunctionKind::Trampoline { .. } => 2,
        };

        match (tag, self.input_func_id).cmp(&(other_tag, other.input_func_id)) {
            std::cmp::Ordering::Equal => self.export.cmp(&other.export),
            ord => ord,
        }
    }
}
impl<'src> PartialOrd for DefinedFunction<'src> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
