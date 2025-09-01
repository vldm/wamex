use anyhow::Result;
use vec_map::VecMap;
use wasmparser::{Comdat, InitFunc, Segment};

use super::CustomSectionReader;
use crate::index::{DataSegmentId, DataSymbolId, GlobalId, IdMap, IdVec, InputFuncId, SectionId};

#[allow(dead_code)]
pub mod section {

    use std::ops::Range;

    use wasmparser::SymbolFlags;

    use crate::index::AnySymbolId;
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
        pub linkage_symbol: AnySymbolId,
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
pub enum SymbolIndex {
    Func(InputFuncId),
    DataDefined(DataSegmentId, DataSymbolId),
    DataUndefined(usize),
    Global(GlobalId),
    Section(SectionId),
    Event(usize),
    Table(usize),
}
#[derive(Default, Debug)]
pub struct LinkingSymbolsInfo<'a> {
    pub globals: IdMap<GlobalId, section::SymInfo<'a>>,
    pub funcs: IdMap<InputFuncId, section::SymInfo<'a>>,
    pub events: VecMap<section::SymInfo<'a>>,
    pub sections: VecMap<section::Section>,
    pub tables: VecMap<section::SymInfo<'a>>,
    // indexed by segment of data
    pub data_in_segments: IdMap<DataSegmentId, IdVec<section::DataInSegment<'a>>>,
    pub undefined_data: Vec<section::Data<'a>>,

    // Map from original flat vector to index in coresponding vector.
    pub original_indexes: Vec<SymbolIndex>,
}

impl<'a> LinkingSymbolsInfo<'a> {
    fn try_from_reader(map: wasmparser::SymbolInfoMap<'a>) -> Result<Self> {
        use wasmparser::SymbolInfo;
        let mut info = Self::default();
        for sym_info in map.into_iter() {
            let symbol_index = match sym_info? {
                SymbolInfo::Global { name, flags, index } => {
                    let index = GlobalId::from_index(index);
                    info.globals.insert(index, section::SymInfo { name, flags });
                    SymbolIndex::Global(index)
                }
                SymbolInfo::Func { name, flags, index } => {
                    let index = InputFuncId::from_index(index);
                    info.funcs.insert(index, section::SymInfo { name, flags });
                    SymbolIndex::Func(index)
                }
                SymbolInfo::Event { name, flags, index } => {
                    info.events
                        .insert(index as usize, section::SymInfo { name, flags });
                    SymbolIndex::Event(index as usize)
                }
                SymbolInfo::Section { flags, section } => {
                    info.sections
                        .insert(section as usize, section::Section { flags });
                    SymbolIndex::Section(section as usize)
                }
                SymbolInfo::Table { flags, index, name } => {
                    info.tables
                        .insert(index as usize, section::SymInfo { name, flags });
                    SymbolIndex::Table(index as usize)
                }
                SymbolInfo::Data {
                    name,
                    flags,
                    symbol,
                } => {
                    let sym = if let Some(symbol) = symbol {
                        let vec = info
                            .data_in_segments
                            .entry(DataSegmentId::from_index(symbol.index))
                            .or_insert(IdVec::new());
                        let id = vec.push(section::DataInSegment {
                            name,
                            flags,
                            offset: symbol.offset,
                            size: symbol.size,
                            linkage_symbol: info.original_indexes.len(),
                        });
                        SymbolIndex::DataDefined(DataSegmentId::from_index(symbol.index), id)
                    } else {
                        info.undefined_data.push(section::Data { name, flags });
                        SymbolIndex::DataUndefined(info.undefined_data.len() - 1)
                    };
                    sym
                }
            };
            info.original_indexes.push(symbol_index);
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
        idx: DataSymbolId,
    ) -> Option<&section::DataInSegment<'a>> {
        self.linking_symbols
            .data_in_segments
            .get(segment_id)
            .and_then(|v| v.get(idx))
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
