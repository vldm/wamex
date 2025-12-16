use anyhow::{Result, bail};
use vec_map::VecMap;

use super::CustomSectionReader;
use crate::index::{
    DataSegmentId, ElementId, FuncTypeId, IdMap, InputFuncId, InputGlobalId, MemoryId, TableId,
    TagId,
};

// Custom sections
#[derive(Default, Clone, Debug)]
pub struct Names<'a> {
    pub module: Option<&'a str>,
    pub functions: IdMap<InputFuncId, &'a str>,
    pub locals: VecMap<wasmparser::NameMap<'a>>,
    pub labels: VecMap<wasmparser::NameMap<'a>>,
    pub types: IdMap<FuncTypeId, &'a str>,
    pub tables: IdMap<TableId, &'a str>,
    pub memories: IdMap<MemoryId, &'a str>,
    pub globals: IdMap<InputGlobalId, &'a str>,
    pub elements: IdMap<ElementId, &'a str>,
    pub data_segments: IdMap<DataSegmentId, &'a str>,
    pub tags: IdMap<TagId, &'a str>,
}

impl<'a> CustomSectionReader<'a> for Names<'a> {
    type Reader = wasmparser::NameSectionReader<'a>;
    // fn new(data: &'a [u8], original_offset: usize) -> Result<Self> {
    //     let mut names: Self = Default::default();
    fn read(reader: Self::Reader) -> Result<Self> {
        let mut names: Self = Default::default();
        for part in reader {
            use wasmparser::Name;
            match part? {
                Name::Module { name, .. } => {
                    names.module = Some(name);
                }
                Name::Function(name_map) => {
                    names.functions = convert_name_map(name_map)?;
                }
                Name::Local(indirect_name_map) => {
                    names.locals = convert_indirect_name_map(indirect_name_map)?;
                }
                Name::Label(indirect_name_map) => {
                    names.labels = convert_indirect_name_map(indirect_name_map)?;
                }
                Name::Type(name_map) => {
                    names.types = convert_name_map(name_map)?;
                }
                Name::Table(name_map) => {
                    names.tables = convert_name_map(name_map)?;
                }
                Name::Memory(name_map) => {
                    names.memories = convert_name_map(name_map)?;
                }
                Name::Global(name_map) => {
                    names.globals = convert_name_map(name_map)?;
                }
                Name::Data(name_map) => {
                    names.data_segments = convert_name_map(name_map)?;
                }
                Name::Element(name_map) => {
                    names.elements = convert_name_map(name_map)?;
                }
                Name::Tag(name_map) => {
                    names.tags = convert_name_map(name_map)?;
                }
                Name::Field(_name_map) => {
                    bail!("Field names not supported");
                }
                Name::Unknown { ty, .. } => {
                    bail!("Unknown name subsection: {:?}", ty);
                }
            }
        }
        Ok(names)
    }
}

fn convert_name_map<'a, T>(
    name_map: wasmparser::NameMap<'a>,
) -> Result<IdMap<crate::index::Id<T>, &'a str>>
where
    crate::index::Id<T>: 'static,
{
    name_map
        .into_iter()
        .map(|r| r.map(|naming| (crate::index::Id::from_index(naming.index), naming.name)))
        .collect::<Result<IdMap<crate::index::Id<T>, &'a str>, _>>()
        .map_err(|e| e.into())
}

fn convert_indirect_name_map<'a>(
    indirect_name_map: wasmparser::IndirectNameMap<'a>,
) -> Result<VecMap<wasmparser::NameMap<'a>>> {
    indirect_name_map
        .into_iter()
        .map(|r| -> Result<(usize, wasmparser::NameMap<'a>)> {
            let indirect_naming = r?;
            Ok((indirect_naming.index as usize, indirect_naming.names))
        })
        .collect::<Result<VecMap<_>, _>>()
}
