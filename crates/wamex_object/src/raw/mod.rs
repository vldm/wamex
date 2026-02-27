//! A thin layer over wasmparser that provide array like access to wasm file sections.

use std::fmt::Debug;

use anyhow::{Result, anyhow, bail};
use cranelift_entity::{EntityRef, PrimaryMap};
pub use indexes::*;
use vec_map::VecMap;
use wasm_encoder::CustomSection;
use wasmparser::{BinaryReader, FromReader, Payload, SectionLimited};
pub use wasmparser::{Element, Export, FuncType, Global, Import, MemoryType, Table, TagType};

use crate::{index::IndexedSection, typed::FunctionRef};

pub mod code;
pub mod data;
mod indexes;
pub mod linking;
pub mod names;
pub mod relocs;
mod target_features;

pub use code::{CodeSection, FunctionWithBody};
pub use data::DataSection;
pub use linking::LinkingInfo;
pub use names::Names;
pub use relocs::Relocation;
pub use target_features::TargetFeatures;

type Ind<T> = IndexedSection<T>;

/// Lossless representation of wasm object, without preprocessing
/// that can pass round-trip test without any loss. After round-trip section will have canonical order.
///
/// Only primitive types are copied, function/data segments and other fields borrow input data for zero-copy parsing.
///
/// By design it is inflated version of `wasmparser::Parser` with all sections traversed and stored in corresponding fields.
#[derive(Default)]
pub struct ObjectReader<'a> {
    // parsed sections
    pub types: PrimaryMap<FuncTypeId, FuncType>,
    pub imports: PrimaryMap<ImportId, Import<'a>>,
    pub exports: PrimaryMap<ExportId, Export<'a>>,
    pub tables: PrimaryMap<DefinedTableId, Table<'a>>,
    // elements is just a table initialisation
    pub elements: PrimaryMap<ElementId, Element<'a>>,
    // tags are used for exceptions
    pub tags: PrimaryMap<DefinedTagId, TagType>,
    pub globals: PrimaryMap<DefinedGlobalId, Global<'a>>,
    // Should be only one memory ?
    pub memories: PrimaryMap<DefinedMemoryId, MemoryType>,
    // code and data is only interested section for relocation application
    pub code: Ind<CodeSection<'a>>,
    pub data: Ind<DataSection<'a>>,

    // Custom sections
    // section "name"
    pub names: Names<'a>,
    // section "linking" (only partial)
    pub linking: LinkingInfo<'a>,
    // sections "reloc.*"
    pub relocs: Relocation,
    // Activated features
    pub target_features: TargetFeatures,
    // other sections
    pub custom_sections: VecMap<Ind<CustomSection<'a>>>,
}

impl<'a> ObjectReader<'a> {
    pub fn parse(wasm: &'a [u8]) -> anyhow::Result<Self> {
        let mut module = Self {
            ..Default::default()
        };

        let mut section_index = 0;
        let mut end = None;

        let mut function_types: Vec<FuncTypeId> = Vec::new();
        let mut code_start = None;
        let mut code_reader_header = None;
        let mut funcs = Vec::new();

        let mut data_count = None;

        let parser = wasmparser::Parser::new(0);
        let mut parser = parser.parse_all(wasm);
        for payload in &mut parser {
            match payload? {
                Payload::TypeSection(reader) => {
                    module.types = reader
                        .into_iter_err_on_gc_types()
                        .collect::<Result<PrimaryMap<_, _>, _>>()?;
                }
                Payload::ImportSection(reader) => {
                    module.imports = read_map(reader)?;
                }
                Payload::TableSection(reader) => {
                    module.tables = read_map(reader)?;
                }
                Payload::MemorySection(reader) => {
                    module.memories = read_map(reader)?;
                }
                Payload::TagSection(reader) => {
                    module.tags = read_map(reader)?;
                }
                Payload::GlobalSection(reader) => {
                    module.globals = read_map(reader)?;
                }
                Payload::ElementSection(reader) => {
                    module.elements = read_map(reader)?;
                }
                Payload::FunctionSection(reader) => {
                    function_types = reader
                        .into_iter()
                        .map(|t| t.map(FuncTypeId::from_u32))
                        .collect::<Result<Vec<_>, _>>()?;
                }
                Payload::ExportSection(reader) => {
                    module.exports = read_map(reader)?;
                }
                Payload::StartSection { func, .. } => {
                    code_start = Some(FunctionRef::from_u32(func));
                }
                Payload::DataCountSection { count, .. } => {
                    data_count = Some(count as usize);
                }
                Payload::DataSection(reader) => {
                    let starting_offset = reader.range().start;

                    let data = DataSection {
                        data_segments: read_map(reader)?,
                    };
                    module.data = Ind {
                        section_payload: data,
                        section_index,
                        starting_offset,
                    };
                }
                // process after loop
                Payload::CodeSectionStart { range, count, .. } => {
                    code_reader_header = Some((range.start, section_index, count));
                }
                Payload::CustomSection(reader) => {
                    let name = reader.name();
                    if name == "name" {
                        let name_reader = wasmparser::NameSectionReader::new(BinaryReader::new(
                            reader.data(),
                            reader.data_offset(),
                        ));
                        module.names = Names::read(name_reader)?;
                    } else if name == "linking" {
                        let linking_reader = wasmparser::LinkingSectionReader::new(
                            BinaryReader::new(reader.data(), reader.data_offset()),
                        )?;
                        module.linking = LinkingInfo::read(linking_reader)?;
                    } else if name.starts_with("reloc.") {
                        let reloc_reader = wasmparser::RelocSectionReader::new(BinaryReader::new(
                            reader.data(),
                            reader.data_offset(),
                        ))?;
                        module.relocs.push_section(reloc_reader)?;
                    } else if name == "target_features" {
                        module.target_features = TargetFeatures::read(BinaryReader::new(
                            reader.data(),
                            reader.data_offset(),
                        ))?;
                    } else {
                        let custom_section = CustomSection {
                            name: reader.name().into(),
                            data: reader.data().into(),
                        };
                        module.custom_sections.insert(
                            section_index,
                            Ind {
                                section_payload: custom_section,
                                section_index,
                                starting_offset: reader.range().start,
                            },
                        );
                    }
                }
                // process after loop
                Payload::CodeSectionEntry(body) => {
                    funcs.push(body);
                    // (not a full section)
                    continue;
                }
                Payload::Version { .. } => continue,
                Payload::End(offset) => {
                    end = Some(offset);
                    break;
                }
                section => {
                    bail!("Unknown section: {:?}", section);
                }
            }

            section_index += 1;
        }
        let _end = end.ok_or_else(|| anyhow!("No end section"))?;
        if parser.next().is_some() {
            bail!("Unexpected trailing data");
        }
        if let Some(data_count) = data_count
            && data_count != module.data.section_payload.data_segments.len()
        {
            bail!(
                "Data count mismatch: {} != {}",
                data_count,
                module.data.section_payload.data_segments.len()
            );
        }

        // merge fields into code section
        module.code = CodeSection::new(
            code_start,
            funcs,
            function_types,
            &module.types,
            code_reader_header,
        )?;

        Ok(module)
    }
}

fn read_map<'lf, K, T>(reader: SectionLimited<'lf, T>) -> wasmparser::Result<PrimaryMap<K, T>>
where
    K: EntityRef,
    T: FromReader<'lf>,
{
    reader.into_iter().collect::<Result<PrimaryMap<_, _>, _>>()
}

trait CustomSectionReader<'a> {
    type Reader;

    fn read(reader: Self::Reader) -> Result<Self>
    where
        Self: Sized;
}

impl<'a> Debug for ObjectReader<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjectReader")
            .field("types", &self.types)
            .field("imports", &self.imports)
            .field("exports", &self.exports)
            .field("tables", &self.tables)
            //
            // .field("elements", &self.elements)
            .field("tags", &self.tags)
            .field("globals", &self.globals)
            .field("memories", &self.memories)
            .field("code", &self.code)
            .field("data", &self.data)
            .finish()
    }
}
