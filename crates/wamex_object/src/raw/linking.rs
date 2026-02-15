use std::fmt::Debug;

use anyhow::Result;
use wasmparser::{Comdat, InitFunc, Segment};

use super::CustomSectionReader;

#[allow(dead_code)]
pub mod section {
    use std::ops::Range;

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

/// Store information by symbol type
#[derive(Debug, Default)]
pub struct LinkingSymbolsInfo<'a> {
    pub symbols: Vec<wasmparser::SymbolInfo<'a>>,
}

impl<'a> LinkingSymbolsInfo<'a> {
    fn try_from_reader(map: wasmparser::SymbolInfoMap<'a>) -> Result<Self> {
        let mut info = Self::default();
        for sym_info in map.into_iter() {
            info.symbols.push(sym_info?);
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
