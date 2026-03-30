impl_entity_index! {
    #[display = "type"]
    pub struct FuncTypeId;
    #[display = "import"]
    pub struct ImportId;
    #[display = "export"]
    pub struct ExportId;
    #[display = "element"]
    pub struct ElementId;
    #[display = "segment"]
    pub struct SegmentId;
    // #[display = "func"]
    // pub struct InputFuncId(for<'a> InputFunction<'a>);
    #[display = "defined_func"]
    pub struct DefinedFuncId;
    // entities
    #[display = "defined_memory"]
    pub struct DefinedMemoryId;
    #[display = "defined_table"]
    pub struct DefinedTableId;
    #[display = "defined_global"]
    pub struct DefinedGlobalId;
    #[display = "defined_tag"]
    pub struct DefinedTagId;

}
