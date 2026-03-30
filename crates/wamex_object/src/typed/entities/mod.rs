use std::fmt::{Debug, Display};

use cranelift_entity::packed_option::ReservedValue;
use derive_more::{Display, From};

use crate::{
    index::{Temp, TempIndex},
    layouts::DataSymbolRef,
    raw::{self, FuncTypeId},
    typed::Module,
};

mod collections;
mod types;

pub use collections::*;
pub use types::*;

impl_entity_index! {
    #[display = "func"]
    pub struct FunctionRef;
    #[display = "table"]
    pub struct TableRef;
    #[display = "memory"]
    pub struct MemoryRef;
    #[display = "global"]
    pub struct GlobalRef;
    #[display = "tag"]
    pub struct TagRef;
}
pub type FnTypeRef = raw::FuncTypeId;

impl TempIndex for FunctionRef {
    fn as_u32(&self) -> u32 {
        FunctionRef::as_u32(*self)
    }
    fn from_u32(value: u32) -> Self {
        FunctionRef::from_u32(value)
    }
}

impl TempIndex for TableRef {
    fn as_u32(&self) -> u32 {
        TableRef::as_u32(*self)
    }
    fn from_u32(value: u32) -> Self {
        TableRef::from_u32(value)
    }
}

impl TempIndex for MemoryRef {
    fn as_u32(&self) -> u32 {
        MemoryRef::as_u32(*self)
    }
    fn from_u32(value: u32) -> Self {
        MemoryRef::from_u32(value)
    }
}

impl TempIndex for GlobalRef {
    fn as_u32(&self) -> u32 {
        GlobalRef::as_u32(*self)
    }
    fn from_u32(value: u32) -> Self {
        GlobalRef::from_u32(value)
    }
}
impl TempIndex for TagRef {
    fn as_u32(&self) -> u32 {
        TagRef::as_u32(*self)
    }
    fn from_u32(value: u32) -> Self {
        TagRef::from_u32(value)
    }
}

impl TempIndex for DataSymbolRef {
    fn as_u32(&self) -> u32 {
        DataSymbolRef::as_u32(*self)
    }
    fn from_u32(value: u32) -> Self {
        DataSymbolRef::from_u32(value)
    }
}

/// The entity type for imports and exports of a module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityType {
    /// The entity is a function.
    Function(wasmparser::FuncType),
    /// The entity is a table.
    Table(wasmparser::TableType),
    /// The entity is a memory.
    Memory(wasmparser::MemoryType),
    /// The entity is a global.
    Global(wasmparser::GlobalType),
    /// The entity is a tag.
    Tag(wasmparser::TagType),
    /// The data symbol
    DataSymbol(()),
}

/// A tagged reference to an entity in a WebAssembly module.
/// Can be converted to `FlatEntityRef` in order to get a unified index.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, From, Display)]
#[display("{_0}")]
pub enum EntityKind {
    Function(FunctionRef),
    DataSymbol(DataSymbolRef),
    Global(GlobalRef),
    Table(TableRef),
    Memory(MemoryRef),
    Tag(TagRef),
    Type(FuncTypeId),
}

impl Debug for EntityKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(self, f)
    }
}

impl ReservedValue for EntityKind {
    fn reserved_value() -> Self {
        EntityKind::Type(FuncTypeId::reserved_value())
    }

    fn is_reserved_value(&self) -> bool {
        matches!(self, EntityKind::Type(t) if t.is_reserved_value())
    }
}

impl EntityKind {
    pub fn is_function(&self) -> bool {
        matches!(self, EntityKind::Function(_))
    }
    pub fn is_data(&self) -> bool {
        matches!(self, EntityKind::DataSymbol(_))
    }
    pub fn is_type(&self) -> bool {
        matches!(self, EntityKind::Type(_))
    }
    pub fn to_inner_u32(&self) -> u32 {
        match self {
            EntityKind::Function(f) => f.as_u32(),
            EntityKind::Global(g) => g.as_u32(),
            EntityKind::Table(t) => t.as_u32(),
            EntityKind::Memory(m) => m.as_u32(),
            EntityKind::Tag(t) => t.as_u32(),
            EntityKind::DataSymbol(d) => d.as_u32(),
            EntityKind::Type(ty) => ty.as_u32(),
        }
    }
}

/// A temp version of `EntityKind` for building purposes, where imports/defined indexes are not stable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, From, Display)]
#[display("{_0}")]
pub enum TempEntityKind {
    Function(Temp<FunctionRef>),
    Global(Temp<GlobalRef>),
    Table(Temp<TableRef>),
    Memory(Temp<MemoryRef>),
    Tag(Temp<TagRef>),
    DataSymbol(Temp<DataSymbolRef>),
    // Type is only used for relocs, we don't really store their in separate array.
    // Type(Temp<FuncTypeId>),
}
impl TempEntityKind {
    pub fn to_stable(self, module: &Module<'_>) -> EntityKind {
        match self {
            TempEntityKind::Function(func_ref) => {
                EntityKind::Function(func_ref.to_stable(module.functions.imports_iter().len()))
            }
            TempEntityKind::Global(global_ref) => {
                EntityKind::Global(global_ref.to_stable(module.globals.imports_iter().len()))
            }
            TempEntityKind::Table(table_ref) => {
                EntityKind::Table(table_ref.to_stable(module.tables.imports_iter().len()))
            }
            TempEntityKind::Tag(tag_ref) => {
                EntityKind::Tag(tag_ref.to_stable(module.tags.imports_iter().len()))
            }
            TempEntityKind::Memory(mem_ref) => {
                EntityKind::Memory(mem_ref.to_stable(module.memories.imports_iter().len()))
            }
            TempEntityKind::DataSymbol(data_symbol_ref) => EntityKind::DataSymbol(
                data_symbol_ref.to_stable(module.extra.mem_layout.imports.len()),
            ),
        }
    }
}
impl ReservedValue for TempEntityKind {
    fn reserved_value() -> Self {
        // SAFETY: index usage can't cause memory unsafety.
        TempEntityKind::Tag(unsafe { Temp::from_raw(TagRef::reserved_value().as_bits()) })
    }

    fn is_reserved_value(&self) -> bool {
        matches!(self, TempEntityKind::Function(t) if TagRef::from_bits(t.as_bits()).is_reserved_value())
    }
}
