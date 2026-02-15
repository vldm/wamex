use crate::{index::TempIndex, raw};

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
