//!
//! Imports for wasm entities.
//!
//! as bonus it contains export entries representation =)
//!

use std::borrow::Cow;

use cranelift_entity::EntityRef;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct ExportEntry<'a, IDX: EntityRef> {
    pub name: &'a str,
    pub entity_index: IDX,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct ImportedFunction<'a> {
    pub module_name: Cow<'a, str>,
    pub func_name: Cow<'a, str>,
    pub func_type: wasmparser::FuncType,
}

impl_entity_index! {
    pub struct ImportedFuncId(for<'a> ImportedFunction<'a>);
}
