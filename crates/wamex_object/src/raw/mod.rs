//! A thin layer over wasmparser that provide array like access to wasm file sections.

use std::{borrow::Cow, fmt::Debug, ops::Range};

use anyhow::{Result, anyhow, bail};
use cranelift_entity::{EntityRef, PrimaryMap};
pub use indexes::*;
use vec_map::VecMap;
use wasm_encoder::CustomSection;
use wasmparser::{BinaryReader, FromReader, Payload, SectionLimited};
pub use wasmparser::{Element, Export, FuncType, Global, Import, MemoryType, Table, TagType};

use crate::{
    index::{IndexedSection, SectionId},
    typed::FunctionRef,
};

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionHeader<'a> {
    pub index: usize,
    pub id: SectionId,
    pub name: Cow<'a, str>,
    pub raw_start: usize,
    pub content_range: Range<usize>,
    pub count: Option<usize>,
}

/// Lossless representation of wasm object, without preprocessing
/// that can pass round-trip test without any loss. After round-trip section will have canonical order.
///
/// Only primitive types are copied, function/data segments and other fields borrow input data for zero-copy parsing.
///
/// By design it is inflated version of `wasmparser::Parser` with all sections traversed and stored in corresponding fields.
#[derive(Default)]
pub struct ObjectReader<'a> {
    pub section_headers: Vec<SectionHeader<'a>>,
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
    pub code: CodeSection<'a>,
    pub data: DataSection<'a>,

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
    pub(crate) tmp_src: &'a [u8],
}

impl<'a> ObjectReader<'a> {
    pub fn parse(wasm: &'a [u8]) -> anyhow::Result<Self> {
        let mut module = Self {
            tmp_src: wasm,
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
        let mut prev_end = 4; // MAGIC_NUMBER + VERSION
        macro_rules! push_section_header {
            ($id:ident, $reader:expr) => {
                module.section_headers.push(section_header(
                    section_index,
                    wasm_encoder::SectionId::$id as SectionId,
                    stringify!($id),
                    $reader.range(),
                    Some($reader.count() as usize),
                    prev_end,
                ));
                prev_end = $reader.range().end;
            };
            (@range $id:ident, $range:expr, $count:expr) => {
                module.section_headers.push(section_header(
                    section_index,
                    wasm_encoder::SectionId::$id as SectionId,
                    stringify!($id),
                    $range.clone(),
                    $count,
                    prev_end,
                ));
                prev_end = $range.end;
            };
            (@custom $name:expr, $reader:expr) => {
                module.section_headers.push(section_header(
                    section_index,
                    wasm_encoder::SectionId::Custom as SectionId,
                    format!("C({})", $name),
                    $reader.range(),
                    None,
                    prev_end,
                ));
                prev_end = $reader.range().end;
            };
        }
        for payload in &mut parser {
            match payload? {
                Payload::TypeSection(reader) => {
                    push_section_header!(Type, reader);
                    module.types = reader
                        .into_iter_err_on_gc_types()
                        .collect::<Result<PrimaryMap<_, _>, _>>()?;
                }
                Payload::ImportSection(reader) => {
                    push_section_header!(Import, reader);
                    module.imports = read_map(reader)?;
                }
                Payload::TableSection(reader) => {
                    push_section_header!(Table, reader);
                    module.tables = read_map(reader)?;
                }
                Payload::MemorySection(reader) => {
                    push_section_header!(Memory, reader);
                    module.memories = read_map(reader)?;
                }
                Payload::TagSection(reader) => {
                    push_section_header!(Tag, reader);
                    module.tags = read_map(reader)?;
                }
                Payload::GlobalSection(reader) => {
                    push_section_header!(Global, reader);
                    module.globals = read_map(reader)?;
                }
                Payload::ElementSection(reader) => {
                    push_section_header!(Element, reader);
                    module.elements = read_map(reader)?;
                }
                Payload::FunctionSection(reader) => {
                    push_section_header!(Function, reader);
                    function_types = reader
                        .into_iter()
                        .map(|t| t.map(FuncTypeId::from_u32))
                        .collect::<Result<Vec<_>, _>>()?;
                }
                Payload::ExportSection(reader) => {
                    push_section_header!(Export, reader);
                    module.exports = read_map(reader)?;
                }
                Payload::StartSection { func, range, .. } => {
                    push_section_header!(@range Start, range, None);
                    code_start = Some(FunctionRef::from_u32(func));
                }
                Payload::DataCountSection { count, range, .. } => {
                    push_section_header!(@range DataCount, range, None);
                    data_count = Some(count as usize);
                }
                Payload::DataSection(reader) => {
                    push_section_header!(Data, reader);

                    let data = DataSection {
                        data_segments: read_map(reader)?,
                    };
                    module.data = data;
                }
                // process after loop
                Payload::CodeSectionStart { range, count, .. } => {
                    push_section_header!(@range Code, range, Some(count as usize));
                    code_reader_header = Some((range.start, section_index, count));
                }
                Payload::CustomSection(reader) => {
                    let name = reader.name();
                    push_section_header!(@custom name, reader);
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
            && data_count != module.data.data_segments.len()
        {
            bail!(
                "Data count mismatch: {} != {}",
                data_count,
                module.data.data_segments.len()
            );
        }

        // merge fields into code section
        module.code = CodeSection::new(
            code_start,
            funcs,
            function_types,
            &module.types,
            code_reader_header,
        )?
        .section_payload;

        Ok(module)
    }

    pub fn code_starting_offset(&self) -> usize {
        self.section_headers
            .iter()
            .find(|h| h.id == wasm_encoder::SectionId::Code as SectionId)
            .map(|h| h.content_range.start)
            .unwrap_or(0)
    }
    pub fn data_starting_offset(&self) -> usize {
        self.section_headers
            .iter()
            .find(|h| h.id == wasm_encoder::SectionId::Data as SectionId)
            .map(|h| h.content_range.start)
            .unwrap_or(0)
    }
    pub fn code_section_index(&self) -> usize {
        self.section_headers
            .iter()
            .position(|h| h.id == wasm_encoder::SectionId::Code as SectionId)
            .unwrap_or(usize::MAX)
    }
    pub fn data_section_index(&self) -> usize {
        self.section_headers
            .iter()
            .position(|h| h.id == wasm_encoder::SectionId::Data as SectionId)
            .unwrap_or(usize::MAX)
    }
}

fn read_map<'lf, K, T>(reader: SectionLimited<'lf, T>) -> wasmparser::Result<PrimaryMap<K, T>>
where
    K: EntityRef,
    T: FromReader<'lf>,
{
    reader.into_iter().collect::<Result<PrimaryMap<_, _>, _>>()
}

fn section_header<'a>(
    index: usize,
    id: SectionId,
    name: impl Into<Cow<'a, str>>,
    range: Range<usize>,
    count: Option<usize>,
    prev_end: usize,
) -> SectionHeader<'a> {
    SectionHeader {
        index,
        id,
        name: name.into(),
        content_range: range,
        count,
        raw_start: prev_end,
    }
}

trait CustomSectionReader<'a> {
    type Reader;

    fn read(reader: Self::Reader) -> Result<Self>
    where
        Self: Sized;
}

impl<'a> Debug for ObjectReader<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let elements = self
            .elements
            .iter()
            .map(|(id, elem)| (id, ElemWrapper(elem)))
            .collect::<Vec<_>>();

        f.debug_struct("ObjectReader")
            .field("types", &self.types)
            .field("imports", &self.imports)
            .field("exports", &self.exports)
            .field("tables", &self.tables)
            .field("elements", &elements)
            .field("tags", &self.tags)
            .field("globals", &self.globals)
            .field("memories", &self.memories)
            .field("code", &self.code)
            .field("data", &self.data)
            .field("names", &self.names)
            .field("linking", &self.linking)
            .field("relocs", &self.relocs)
            .finish()
    }
}

struct ElemWrapper<'o, 'a>(&'o Element<'a>);
impl<'o, 'a> Debug for ElemWrapper<'o, 'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match &self.0.kind {
            wasmparser::ElementKind::Active {
                table_index,
                offset_expr,
            } => {
                format!(
                    "Active(table={table_index:?}, offset_expr={:?})",
                    offset_expr
                )
            }
            wasmparser::ElementKind::Passive => "Passive".to_string(),
            wasmparser::ElementKind::Declared => "Declared".to_string(),
        };
        let items = match &self.0.items {
            wasmparser::ElementItems::Functions(funcs) => format!("Functions({:?})", funcs),
            wasmparser::ElementItems::Expressions(..) => String::from("Expressions()"),
        };
        f.debug_struct("Element")
            .field("kind", &kind)
            .field("items", &items)
            .finish()
    }
}
