use wasmparser::{Data, Element, Export, FuncType, Global, Import, MemoryType, Table, TagType};

use super::code::{FunctionWithBody, InputFunction};

impl_entity_index! {
    #[display = "type"]
    pub struct FuncTypeId(FuncType);
    #[display = "import"]
    pub struct ImportId(for<'a> Import<'a>);
    #[display = "export"]
    pub struct ExportId(for<'a> Export<'a>);
    #[display = "element"]
    pub struct ElementId(for<'a> Element<'a>);
    #[display = "data"]
    pub struct DataSegmentId(for<'a> Data<'a>);
    // #[display = "func"]
    // pub struct InputFuncId(for<'a> InputFunction<'a>);
    #[display = "defined_func"]
    pub struct DefinedFuncId(for<'a> FunctionWithBody<'a>);
    // entities
    #[display = "memory"]
    pub struct MemoryId(MemoryType);
    #[display = "table"]
    pub struct TableId(for<'a> Table<'a>);
    #[display = "global"]
    pub struct DefinedGlobalId(for<'a> Global<'a>);
    #[display = "tag"]
    pub struct TagId(TagType);

}

pub type InputGlobalId = crate::read::typed::GlobalRef;
pub type InputFuncId = crate::read::typed::FunctionRef;
