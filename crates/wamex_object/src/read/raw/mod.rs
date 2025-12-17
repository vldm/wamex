use anyhow::{Result, anyhow, bail};
use target_features::TargetFeatures;
use vec_map::VecMap;
use wasm_encoder::CustomSection;
use wasmparser::{BinaryReader, Payload};
pub use wasmparser::{Element, Export, FuncType, Global, Import, MemoryType, Table, TagType};

use crate::index::{DefinedFuncId, FuncTypeId, IdVec2, IndexedSection};

pub mod code;
pub mod data;
pub mod linking;
pub mod names;
pub mod relocs;
mod target_features;

use code::CodeSection;
use data::DataSection;
use linking::LinkingInfo;
use names::Names;
use relocs::Relocation;

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
    pub types: IdVec2<FuncType>,
    pub imports: IdVec2<Import<'a>>,
    pub exports: IdVec2<Export<'a>>,
    pub tables: IdVec2<Table<'a>>,
    // elements is just a table initialisation
    pub elements: IdVec2<Element<'a>>,
    // tags are used for exceptions
    pub tags: IdVec2<TagType>,
    pub globals: IdVec2<Global<'a>>,
    // Should be only one memory ?
    pub memories: IdVec2<MemoryType>,
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
                        .collect::<Result<IdVec2<_>, _>>()?;
                }
                Payload::ImportSection(reader) => {
                    module.imports = reader.into_iter().collect::<Result<IdVec2<_>, _>>()?;
                }
                Payload::TableSection(reader) => {
                    module.tables = reader.into_iter().collect::<Result<IdVec2<_>, _>>()?;
                }
                Payload::MemorySection(reader) => {
                    module.memories = reader.into_iter().collect::<Result<IdVec2<_>, _>>()?;
                }
                Payload::TagSection(reader) => {
                    module.tags = reader.into_iter().collect::<Result<IdVec2<_>, _>>()?;
                }
                Payload::GlobalSection(reader) => {
                    module.globals = reader.into_iter().collect::<Result<IdVec2<_>, _>>()?;
                }
                Payload::ElementSection(reader) => {
                    module.elements = reader.into_iter().collect::<Result<IdVec2<_>, _>>()?;
                }
                Payload::FunctionSection(reader) => {
                    function_types = reader
                        .into_iter()
                        .map(|t| t.map(crate::index::FuncTypeId::from_u32))
                        .collect::<Result<Vec<_>, _>>()?;
                }
                Payload::ExportSection(reader) => {
                    module.exports = reader.into_iter().collect::<Result<IdVec2<_>, _>>()?;
                }
                Payload::StartSection { func, .. } => {
                    code_start = Some(crate::index::InputFuncId::from_index(func));
                }
                Payload::DataCountSection { count, .. } => {
                    data_count = Some(count as usize);
                }
                Payload::DataSection(reader) => {
                    let starting_offset = reader.range().start;

                    let data = DataSection {
                        data_segments: reader.into_iter().collect::<Result<IdVec2<_>, _>>()?,
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
        if let Some(data_count) = data_count {
            if data_count != module.data.section_payload.data_segments.len() {
                bail!(
                    "Data count mismatch: {} != {}",
                    data_count,
                    module.data.section_payload.data_segments.len()
                );
            }
        }

        // merge fields into code section
        module.code = CodeSection::new(code_start, funcs, function_types, code_reader_header)?;

        Ok(module)
    }
    pub fn defined_func_type_id(&self, id: DefinedFuncId) -> FuncTypeId {
        self.code.section_payload.defined_funcs[id].type_id
    }
}

trait CustomSectionReader<'a> {
    type Reader;

    fn read(reader: Self::Reader) -> Result<Self>
    where
        Self: Sized;
}
