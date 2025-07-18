use anyhow::{anyhow, bail, ensure, Result};
use vec_map::VecMap;
use wasm_encoder::CustomSection;

use std::collections::HashMap;
pub use std::ops::Range;
use wasmparser::{
    BinaryReader, Comdat, ComdatMap, InitFunc, InitFuncMap, Payload, SectionLimited, Segment,
    SegmentMap, TypeRef,
};
pub use wasmparser::{
    Data, Element, Export, FuncType, FunctionBody, Global, Import, MemoryType, RelocationEntry,
    SymbolInfo, Table, TagType,
};
pub type InputRange = Range<usize>;

use crate::index::{DataSegmentId, FuncTypeId, IndexedSection, InputFuncId, SectionIndex};

#[derive(Debug)]
pub struct Function<'a> {
    pub type_id: FuncTypeId,
    pub body: FunctionBody<'a>,
}

#[derive(Debug, Default)]
pub struct CodeSection<'a> {
    pub start_func: Option<InputFuncId>,
    // function (CodeSectionEntry)
    pub defined_funcs: Vec<Function<'a>>,

    func_types: Vec<FuncTypeId>,
}
impl<'a> CodeSection<'a> {
    pub fn new(
        start: Option<InputFuncId>,
        funcs: Vec<FunctionBody<'a>>,
        func_types: Vec<FuncTypeId>,
        code_header: Option<(usize, usize, u32)>,
    ) -> Result<Ind<Self>> {
        let Some((code_start, section_index, count)) = code_header else {
            bail!("No code section start");
        };
        ensure!(
            count as usize == funcs.len(),
            "Function count mismatch: {} != {}",
            count,
            funcs.len()
        );
        ensure!(
            count as usize == func_types.len(),
            "Function types count mismatch: {} != {}",
            count,
            func_types.len()
        );
        Ok(Ind {
            starting_offset: code_start,
            section_index,
            section_payload: CodeSection {
                start_func: start,
                defined_funcs: funcs
                    .into_iter()
                    .zip(&func_types)
                    .map(|(body, ty)| Function { type_id: *ty, body })
                    .collect(),
                func_types,
            },
        })
    }
}

#[derive(Debug, Default)]
pub struct DataSection<'a> {
    pub data_segments: Vec<Data<'a>>,
}

type Ind<T> = IndexedSection<T>;

/// Lossless representation of wasm module, without preprocessing
/// That can pass round-trip test without any loss.
/// After round-trip section will have canonical order.
#[derive(Default)]
pub struct InputModule<'a> {
    // parsed sections
    pub types: Vec<FuncType>,
    pub imports: Vec<Import<'a>>,
    pub exports: Vec<Export<'a>>,
    pub tables: Vec<Table<'a>>,
    // elements is just a table initialisation
    pub elements: Vec<Element<'a>>,
    // tags are used for exceptions
    pub tags: Vec<TagType>,
    pub globals: Vec<Global<'a>>,
    // Should be only one memory ?
    pub memories: Vec<MemoryType>,
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
    // other sections
    pub custom_sections: Vec<Ind<CustomSection<'a>>>,
    // post-processed fields
    // pub imported_funcs: Vec<ImportId>,
    // pub imported_func_map: HashMap<ImportId, InputFuncId>,
    // pub data_symbols: Vec<DataSymbol>,
    // pub export_map: HashMap<(isize, usize), (usize, &'a str)>,
}

pub mod linking {
    use std::ops::Range;

    use wasmparser::SymbolFlags;

    #[derive(Default, Debug)]
    pub struct Data<'a> {
        /// The flags for the symbol.
        pub flags: SymbolFlags,
        /// The name for the symbol.
        pub name: &'a str,
    }

    #[derive(Default, Debug)]
    pub struct DataInSegment<'a> {
        /// The flags for the symbol.
        pub flags: SymbolFlags,
        /// The name for the symbol.
        pub name: &'a str,
        pub offset: u32,
        pub size: u32,
    }

    /// The symbol is a section.

    #[derive(Default, Debug)]
    pub struct Section {
        /// The flags for the symbol.
        pub flags: SymbolFlags,
    }
    /// The symbol is an event function or table.
    #[derive(Default, Debug)]
    pub struct SymInfo<'a> {
        /// The flags for the symbol.
        pub flags: SymbolFlags,
        /// The name for the event, if it is defined or uses an explicit name.
        pub name: Option<&'a str>,
    }

    #[derive(Default, Debug)]
    pub struct UnknownInfo<'a> {
        /// The identifier for this subsection.
        pub ty: u8,
        /// The contents of this subsection.
        pub data: &'a [u8],
        /// The range of bytes, relative to the start of the original data
        /// stream, that the contents of this subsection reside in.
        pub range: Range<usize>,
    }
}

#[derive(Debug, Clone, Copy)]
pub enum SymbolType {
    Func,
    DataDefined(DataSegmentId),
    DataUndefined,
    Global,
    Section,
    Event,
    Table,
}
#[derive(Default, Debug)]
pub struct LinkingSymbolsInfo<'a> {
    pub globals: VecMap<linking::SymInfo<'a>>,
    pub funcs: VecMap<linking::SymInfo<'a>>,
    pub events: VecMap<linking::SymInfo<'a>>,
    pub sections: VecMap<linking::Section>,
    pub tables: VecMap<linking::SymInfo<'a>>,
    // indexed by segment of data
    pub data_in_segments: VecMap<Vec<linking::DataInSegment<'a>>>,
    pub undefined_data: Vec<linking::Data<'a>>,

    pub original_indexes: Vec<(usize, SymbolType)>,
}
impl<'a> LinkingSymbolsInfo<'a> {
    fn try_from_reader(map: wasmparser::SymbolInfoMap<'a>) -> Result<Self> {
        use wasmparser::SymbolInfo;
        let mut info = Self::default();
        for sym_info in map.into_iter() {
            let (sym_type, idx) = match sym_info? {
                SymbolInfo::Global { name, flags, index } => {
                    info.globals
                        .insert(index as usize, linking::SymInfo { name, flags });
                    (SymbolType::Global, index as usize)
                }
                SymbolInfo::Func { name, flags, index } => {
                    info.funcs
                        .insert(index as usize, linking::SymInfo { name, flags });
                    (SymbolType::Func, index as usize)
                }
                SymbolInfo::Event { name, flags, index } => {
                    info.events
                        .insert(index as usize, linking::SymInfo { name, flags });
                    (SymbolType::Event, index as usize)
                }
                SymbolInfo::Section { flags, section } => {
                    info.sections
                        .insert(section as usize, linking::Section { flags });
                    (SymbolType::Section, section as usize)
                }
                SymbolInfo::Table { flags, index, name } => {
                    info.tables
                        .insert(index as usize, linking::SymInfo { name, flags });
                    (SymbolType::Table, index as usize)
                }
                SymbolInfo::Data {
                    name,
                    flags,
                    symbol,
                } => {
                    let sym = if let Some(symbol) = symbol {
                        let vec = info
                            .data_in_segments
                            .entry(symbol.index as usize)
                            .or_insert(Vec::new());
                        vec.push(linking::DataInSegment {
                            name,
                            flags,
                            offset: symbol.offset,
                            size: symbol.size,
                        });
                        let idx = vec.len() - 1;
                        (SymbolType::DataDefined(symbol.index as usize), idx)
                    } else {
                        info.undefined_data.push(linking::Data { name, flags });
                        (SymbolType::DataUndefined, info.undefined_data.len() - 1)
                    };
                    sym
                }
            };
            info.original_indexes.push((idx, sym_type));
        }
        Ok(info)
    }
}

#[derive(Default, Debug)]
pub struct RelocationSection {
    pub entries: Vec<RelocationEntry>,
}

trait CustomSectionReader<'a> {
    type Reader;

    fn read(reader: Self::Reader) -> Result<Self>
    where
        Self: Sized;
}

// Custom sections
#[derive(Default, Clone)]
pub struct Names<'a> {
    pub module: Option<&'a str>,
    pub functions: VecMap<&'a str>,
    pub locals: VecMap<wasmparser::NameMap<'a>>,
    pub labels: VecMap<wasmparser::NameMap<'a>>,
    pub types: VecMap<&'a str>,
    pub tables: VecMap<&'a str>,
    pub memories: VecMap<&'a str>,
    pub globals: VecMap<&'a str>,
    pub elements: VecMap<&'a str>,
    pub data_segments: VecMap<&'a str>,
    pub tags: VecMap<&'a str>,
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

#[derive(Default, Debug)]
pub struct LinkingInfo<'a> {
    pub segments_info: Vec<Segment<'a>>,
    pub init_funcs: Vec<InitFunc>,
    pub comdat_info: Vec<Comdat<'a>>,
    pub linking_symbols: LinkingSymbolsInfo<'a>,
    pub unknown_linking: Vec<linking::UnknownInfo<'a>>,
}

impl<'a> CustomSectionReader<'a> for LinkingInfo<'a> {
    type Reader = wasmparser::LinkingSectionReader<'a>;

    fn read(reader: Self::Reader) -> Result<Self> {
        use wasmparser::Linking;
        let mut linking = LinkingInfo::default();
        for subsection in reader.subsections() {
            match subsection? {
                Linking::SegmentInfo(s) => {
                    linking
                        .segments_info
                        .extend(&s.into_iter().collect::<Result<Vec<_>, _>>()?);
                }
                Linking::InitFuncs(i) => {
                    linking
                        .init_funcs
                        .extend(&i.into_iter().collect::<Result<Vec<_>, _>>()?);
                }
                Linking::ComdatInfo(c) => {
                    linking
                        .comdat_info
                        .extend(c.into_iter().collect::<Result<Vec<_>, _>>()?);
                }
                Linking::SymbolTable(map) => {
                    linking.linking_symbols = LinkingSymbolsInfo::try_from_reader(map)?;
                }
                Linking::Unknown { ty, data, range } => {
                    linking
                        .unknown_linking
                        .push(linking::UnknownInfo { ty, data, range });
                }
            }
        }

        Ok(linking)
    }
}

#[derive(Default, Debug)]
/// Information stored in "reloc.*" sections
pub struct Relocation {
    ///
    /// "reloc.*" sections.
    /// The key is an index to which relocation entry corresponds
    pub relocs: VecMap<RelocationSection>,
}
impl Relocation {
    fn push_section(&mut self, reader: wasmparser::RelocSectionReader) -> Result<()> {
        self.relocs.insert(
            reader.section_index() as SectionIndex,
            RelocationSection::read(reader.entries())?,
        );
        Ok(())
    }
    pub fn get_section(&self, index: SectionIndex) -> Option<&RelocationSection> {
        self.relocs.get(index)
    }
}

impl<'a> CustomSectionReader<'a> for RelocationSection {
    type Reader = SectionLimited<'a, RelocationEntry>;

    fn read(reader: Self::Reader) -> Result<Self> {
        let entries = reader.into_iter().collect::<Result<Vec<_>, _>>()?;
        Ok(RelocationSection { entries })
    }
}

impl<'a> InputModule<'a> {
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
                        .collect::<Result<Vec<_>, _>>()?;
                }
                Payload::ImportSection(reader) => {
                    module.imports = reader.into_iter().collect::<Result<Vec<_>, _>>()?;
                }
                Payload::TableSection(reader) => {
                    module.tables = reader.into_iter().collect::<Result<Vec<_>, _>>()?;
                }
                Payload::MemorySection(reader) => {
                    module.memories = reader.into_iter().collect::<Result<Vec<_>, _>>()?;
                }
                Payload::TagSection(reader) => {
                    module.tags = reader.into_iter().collect::<Result<Vec<_>, _>>()?;
                }
                Payload::GlobalSection(reader) => {
                    module.globals = reader.into_iter().collect::<Result<Vec<_>, _>>()?;
                }
                Payload::ElementSection(reader) => {
                    module.elements = reader.into_iter().collect::<Result<Vec<_>, _>>()?;
                }
                Payload::FunctionSection(reader) => {
                    function_types = reader
                        .into_iter()
                        .map(|t| t.map(|id| id as FuncTypeId))
                        .collect::<Result<Vec<_>, _>>()?;
                }
                Payload::ExportSection(reader) => {
                    module.exports = reader.into_iter().collect::<Result<Vec<_>, _>>()?;
                }
                Payload::StartSection { func, .. } => {
                    code_start = Some(func as usize);
                }
                Payload::DataCountSection { count, .. } => {
                    data_count = Some(count as usize);
                }
                Payload::DataSection(reader) => {
                    let starting_offset = reader.range().start;

                    let data = DataSection {
                        data_segments: reader.into_iter().collect::<Result<Vec<_>, _>>()?,
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
                    } else {
                        let custom_section = CustomSection {
                            name: reader.name().into(),
                            data: reader.data().into(),
                        };
                        module.custom_sections.push(Ind {
                            section_payload: custom_section,
                            section_index,
                            starting_offset: reader.range().start,
                        });
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
    pub fn func_type_id(&self, id: InputFuncId) -> FuncTypeId {
        self.code.section_payload.func_types[id as usize]
    }
}

fn convert_name_map<'a>(name_map: wasmparser::NameMap<'a>) -> Result<VecMap<&'a str>> {
    name_map
        .into_iter()
        .map(|r| r.map(|naming| (naming.index as usize, naming.name)))
        .collect::<Result<VecMap<&'a str>, _>>()
        .map_err(|e| e.into())
}

fn convert_indirect_name_map<'a>(
    indirect_name_map: wasmparser::IndirectNameMap<'a>,
) -> Result<VecMap<wasmparser::NameMap<'a>>> {
    Ok(indirect_name_map
        .into_iter()
        .map(|r| -> Result<(usize, wasmparser::NameMap<'a>)> {
            let indirect_naming = r?;
            Ok((indirect_naming.index as usize, indirect_naming.names))
        })
        .collect::<Result<VecMap<_>, _>>()?)
}

// self.generate_type_section()?;
// self.generate_import_section();
// self.generate_table_section();
// self.generate_function_section();
// self.generate_memory_section();
// self.generate_global_section();
// self.generate_export_section();
// self.generate_start_section();
// self.generate_element_section()?;
// self.generate_data_count_section();
// self.generate_code_section()?;
// self.generate_data_section()?;
// self.generate_wasm_bindgen_sections();
// self.generate_name_section()?;
// mod read;