use anyhow::{Result, bail};
use vec_map::VecMap;

use super::{
    CustomSectionReader,
    indexes::{DataSegmentId, ElementId, FuncTypeId},
};
use crate::{
    index::{GappedMap, NonDefault, SecondaryMap},
    read::typed::{FunctionRef, GlobalRef, MemoryRef, TableRef, TagRef},
};

type Str<'a> = NonDefault<&'a str>;

// Custom sections
#[derive(Default, Clone, Debug)]
pub struct Names<'a> {
    pub module: Option<&'a str>,
    pub locals: VecMap<wasmparser::NameMap<'a>>,
    pub labels: VecMap<wasmparser::NameMap<'a>>,
    pub types: GappedMap<FuncTypeId, Str<'a>>,
    pub elements: GappedMap<ElementId, Str<'a>>,
    pub data_segments: GappedMap<DataSegmentId, Str<'a>>,
    // entities
    pub functions: GappedMap<FunctionRef, Str<'a>>,
    pub tables: GappedMap<TableRef, Str<'a>>,
    pub memories: GappedMap<MemoryRef, Str<'a>>,
    pub globals: GappedMap<GlobalRef, Str<'a>>,
    pub tags: GappedMap<TagRef, Str<'a>>,
}

impl<'a> CustomSectionReader<'a> for Names<'a> {
    type Reader = wasmparser::NameSectionReader<'a>;

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

fn convert_name_map<'a, Idx>(name_map: wasmparser::NameMap<'a>) -> Result<GappedMap<Idx, Str<'a>>>
where
    Idx: crate::index::EntityRef + From<u32>,
{
    name_map
        .into_iter()
        .map(|r| r.map(|naming| (naming.index.into(), naming.name.into())))
        .collect::<Result<GappedMap<Idx, Str<'a>>, _>>()
        .map_err(|e| e.into())
}
