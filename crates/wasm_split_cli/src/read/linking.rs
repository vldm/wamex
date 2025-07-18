use anyhow::Result;
use vec_map::VecMap;

pub use std::ops::Range;
use wasmparser::{Comdat, InitFunc, Segment};

pub type InputRange = Range<usize>;

use super::CustomSectionReader;
use crate::index::{DataSegmentId, SymbolIndex};

pub mod section {
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

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
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
    pub globals: VecMap<section::SymInfo<'a>>,
    pub funcs: VecMap<section::SymInfo<'a>>,
    pub events: VecMap<section::SymInfo<'a>>,
    pub sections: VecMap<section::Section>,
    pub tables: VecMap<section::SymInfo<'a>>,
    // indexed by segment of data
    pub data_in_segments: VecMap<Vec<section::DataInSegment<'a>>>,
    pub undefined_data: Vec<section::Data<'a>>,

    // Map from original flat vector to index in coresponding vector.
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
                        .insert(index as usize, section::SymInfo { name, flags });
                    (SymbolType::Global, index as usize)
                }
                SymbolInfo::Func { name, flags, index } => {
                    info.funcs
                        .insert(index as usize, section::SymInfo { name, flags });
                    (SymbolType::Func, index as usize)
                }
                SymbolInfo::Event { name, flags, index } => {
                    info.events
                        .insert(index as usize, section::SymInfo { name, flags });
                    (SymbolType::Event, index as usize)
                }
                SymbolInfo::Section { flags, section } => {
                    info.sections
                        .insert(section as usize, section::Section { flags });
                    (SymbolType::Section, section as usize)
                }
                SymbolInfo::Table { flags, index, name } => {
                    info.tables
                        .insert(index as usize, section::SymInfo { name, flags });
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
                        vec.push(section::DataInSegment {
                            name,
                            flags,
                            offset: symbol.offset,
                            size: symbol.size,
                        });
                        let idx = vec.len() - 1;
                        (SymbolType::DataDefined(symbol.index as usize), idx)
                    } else {
                        info.undefined_data.push(section::Data { name, flags });
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
pub struct LinkingInfo<'a> {
    pub segments_info: Vec<Segment<'a>>,
    pub init_funcs: Vec<InitFunc>,
    pub comdat_info: Vec<Comdat<'a>>,
    pub linking_symbols: LinkingSymbolsInfo<'a>,
    pub unknown_linking: Vec<section::UnknownInfo<'a>>,
}

impl<'a> LinkingInfo<'a> {
    pub fn get_data_in_segment(
        &self,
        segment_id: DataSegmentId,
        idx: SymbolIndex,
    ) -> Option<&section::DataInSegment<'a>> {
        self.linking_symbols
            .data_in_segments
            .get(segment_id as usize)
            .and_then(|v| v.get(idx as usize))
    }
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
                        .push(section::UnknownInfo { ty, data, range });
                }
            }
        }

        Ok(linking)
    }
}
