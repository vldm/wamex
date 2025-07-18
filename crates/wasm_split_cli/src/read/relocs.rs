use anyhow::Result;
pub use std::ops::Range;
use vec_map::VecMap;
pub use wasmparser::RelocationEntry;
use wasmparser::SectionLimited;
pub type InputRange = Range<usize>;

use super::CustomSectionReader;
use crate::index::SectionIndex;

#[derive(Default, Debug)]
/// Information stored in "reloc.*" sections
pub struct Relocation {
    ///
    /// "reloc.*" sections.
    /// The key is an index to which relocation entry corresponds
    pub relocs: VecMap<RelocationSection>,
}
impl Relocation {
    pub(super) fn push_section(&mut self, reader: wasmparser::RelocSectionReader) -> Result<()> {
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

#[derive(Default, Debug)]
pub struct RelocationSection {
    pub entries: Vec<RelocationEntry>,
}

impl<'a> CustomSectionReader<'a> for RelocationSection {
    type Reader = SectionLimited<'a, RelocationEntry>;

    fn read(reader: Self::Reader) -> Result<Self> {
        let entries = reader.into_iter().collect::<Result<Vec<_>, _>>()?;
        Ok(RelocationSection { entries })
    }
}
