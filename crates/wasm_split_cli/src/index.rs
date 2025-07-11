
pub type SymbolIndex = usize;

pub type SymbolId = usize;
pub type FuncTypeId = usize;
pub type InputFuncId = usize;
pub type TableId = usize;
pub type ImportId = usize;
pub type ExportId = usize;
pub type MemoryId = usize;
pub type GlobalId = usize;
pub type ElementId = usize;
pub type DataSegmentId = usize;
pub type TagId = usize;
pub type SectionIndex = usize;


/// Store additional information about section, to apply relocation
#[derive(Debug)]
pub struct IndexedSection<T> {
    pub section_payload: T,
    pub starting_offset: usize,

    pub section_index: usize,
}

impl<T: Default> Default for IndexedSection<T> {
    fn default() -> Self {
        Self {
            section_payload: T::default(),
            starting_offset: 0,
            section_index: usize::MAX,
        }
    }
}

impl<T> IndexedSection<T> {
    fn is_default(&self) -> bool {
        self.starting_offset == 0 && self.section_index == usize::MAX
    }
}