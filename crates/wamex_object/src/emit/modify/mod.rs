use anyhow::Result;

use crate::{
    SVec,
    emit::modify::cursor::Cursor,
    linkage::{file_db::EntityRelocationEntry, reloc::EntitySymbol},
    typed::common_index::EntityKind,
};

pub mod code_abs_to_got;
pub mod cursor;
pub mod wasm_emitter;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum OutputEntityRef {
    /// Entity that already exist in output module.
    Resolved(EntitySymbol),
    /// Entity that will be created in output module, but which id need to be resolved.
    /// The resolution bound with `FileId` and can be only done in context of processing some predefined entity from input module.
    FromInput(EntitySymbol),
}
impl OutputEntityRef {
    pub fn from_input(input: EntitySymbol) -> Self {
        Self::FromInput(input)
    }

    pub fn resolved(resolved: EntitySymbol) -> Self {
        Self::Resolved(resolved)
    }
}

pub type OutputRelocationEntry = crate::linkage::reloc::RelocationEntry<OutputEntityRef>;
type RelocationEntry = EntityRelocationEntry;

const _ASSERT_SIZE: () = {
    assert!(std::mem::size_of::<OutputRelocationEntry>() == std::mem::size_of::<RelocationEntry>());
};

/// Body + relocations related to body.
#[derive(Debug, Clone, Copy)]
pub struct RelocTarget<'any, 'src> {
    pub body: &'src [u8],
    /// Offset of body related to start of segment, needed for shift of relocations.
    pub start_offset: usize,
    pub entries: &'any [RelocationEntry],
}
///
/// Represents a rewrite operation that modifies a specific range of bytes.
/// It can be new instructions or data placement, inside `Symbol`.
/// Or some removing/replacing of existing bytes, that cannot be done via relocations.
///
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Rewrite {
    pub old_range: std::ops::Range<usize>,
    /// Relocations with offsets relative to `Rewrite` start,
    /// and referencing either new symbol, or symbol existing in same file as one that we modify.
    pub new_relocs: SVec<OutputRelocationEntry, 3>,
    pub new_bytes: SVec<u8, 16>,
}

impl Rewrite {
    /// Get the size difference between new and old data.
    pub fn size(&self) -> isize {
        self.new_bytes.len() as isize - self.old_range.len() as isize
    }
}

/// Represents a modification entry that describes changes to be made
/// to a specific range of bytes in a WebAssembly module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModificationEntry<D = ()> {
    /// Indicates if this entry rewrites some of the original bytes.
    /// It could be replacing of
    pub rewrite: Option<Rewrite>,
    // Debug?/Trace info about original relocation
    pub original_reloc: RelocationEntry,
    // if this reloc needs to be handled with extra data
    pub extra_info: D,
}

/// Represents either a modification entry or a relocation entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModifyOrReloc<D> {
    Modify(ModificationEntry<D>),
    OriginalReloc(RelocationEntry),
}

/// Implementation of modification routine.
/// Allows adding patches to the original code based on the original relocation entries.
///
/// The main purpose of this patches is to replace some symbol references with other types.
/// e.g. converting absoulte address to got-relative, or replacing a function call with an indirect call.
pub trait HandleReloc<'src> {
    type ExtraData;

    /// Make setup specific for module
    ///
    /// e.g. init some funcs/globals/types in builder
    /// that will be used during modification entries creation or application.
    fn setup(&mut self, _builder: &mut crate::typed::ModuleBuilder<'src>) -> Result<()> {
        Ok(())
    }

    // TODO: Suport modification that need two or more relocs
    // e.g., for got-relative addressing
    fn create_entry(
        &self,
        buffer: Cursor<'src>,
        entry: RelocationEntry,
    ) -> Result<ModifyOrReloc<Self::ExtraData>>;

    fn create_entries(
        &self,
        target: RelocTarget<'_, 'src>,
    ) -> Result<Vec<ModifyOrReloc<Self::ExtraData>>> {
        let mut result = vec![];
        let mut entries = target.entries.iter().peekable();
        let mut prev_range = ..0usize;

        while let Some(entry) = entries.next() {
            let red_after = entries
                .peek()
                .map(|e| e.offset as usize..)
                .unwrap_or(target.body.len()..);
            let cursor = Cursor::new(target.body, entry.relocation_range(), prev_range, red_after);

            result.push(self.create_entry(cursor, *entry)?);

            prev_range = ..entry.offset as usize;
        }
        Ok(result)
    }

    // Handle extra data after all modifications are applied
    fn finalize(
        &self,
        _extra_data: Self::ExtraData,
        _builder: &mut crate::typed::ModuleBuilder<'src>,
    ) -> Result<()> {
        Ok(())
    }
}
