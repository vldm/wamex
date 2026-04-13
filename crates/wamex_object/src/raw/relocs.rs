use anyhow::Result;
use vec_map::VecMap;
pub use wasmparser::RelocationEntry;
use wasmparser::SectionLimited;

use super::CustomSectionReader;
use crate::index::SectionId;

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
        let exist = self.relocs.insert(
            reader.section_index() as usize,
            RelocationSection::read(reader.entries())?,
        );
        if exist.is_some() {
            log::error!(
                "duplicate reloc section for index {}",
                reader.section_index()
            );
        }
        Ok(())
    }
    #[must_use]
    pub fn get_section(&self, index: SectionId) -> Option<&RelocationSection> {
        self.relocs.get(index as usize)
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
