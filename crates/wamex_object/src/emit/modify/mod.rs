use anyhow::Result;

use crate::{
    SVec,
    emit::modify::cursor::Cursor,
    helpers::RangeExt,
    linkage::reloc::{EntityAddressMode, EntityRelocationEntry},
    typed::{EntityBodyCopy, common_index::EntityKind},
};

pub mod code_abs_to_got;
pub mod cursor;
pub mod wasm_emitter;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum OutputEntityRef {
    /// Entity that already exist in output module.
    Resolved(EntityKind),
    /// Entity that will be created in output module, but which id need to be resolved.
    /// The resolution bound with `FileId` and can be only done in context of processing some predefined entity from input module.
    FromInput(EntityKind),
}
impl OutputEntityRef {
    pub fn from_input(input: EntityKind) -> Self {
        Self::FromInput(input)
    }

    pub fn resolved(resolved: EntityKind) -> Self {
        Self::Resolved(resolved)
    }
}

pub type OutputRelocationEntry =
    crate::linkage::reloc::RelocationEntry<OutputEntityRef, EntityAddressMode>;

///
/// Represents a rewrite operation that modifies a specific range of bytes.
/// It can be new instructions or data placement, inside `Symbol`.
/// Or some removing/replacing of existing bytes, that cannot be done via relocations.
///
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Rewrite {
    /// Range in bytes that should be replaced with `new_bytes`.
    /// Or point for new insertion.
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
    pub original_reloc: EntityRelocationEntry,
    // if this reloc needs to be handled with extra data
    pub extra_info: D,
}

/// Implementation of modification routine.
/// Allows adding patches to the original code based on the original relocation entries.
///
/// The main purpose of this patches is to replace some symbol references with other types.
/// e.g. converting absoulte address to got-relative, or replacing a function call with an indirect call.
pub trait HandleFixups<'src> {
    type ExtraData;

    // TODO: Suport modification that need two or more relocs
    // e.g., for got-relative addressing
    /// Create a modification entry based on the original relocation entry and the current state of the code.
    /// Return None if no modification is needed for this entry.
    fn create_entry(
        &self,
        buffer: Cursor<'src>,
        entry: EntityRelocationEntry,
    ) -> Result<Option<(Rewrite, Self::ExtraData)>>;
}

pub fn create_fixup_for_entity<'src, H: HandleFixups<'src>>(
    entity: &mut EntityBodyCopy<'src>,
    entity_relocs: &[EntityRelocationEntry],
    handler: &H,
) -> Result<Vec<H::ExtraData>> {
    let mut result = vec![];
    let mut entries = entity_relocs.iter().peekable();
    let mut prev_range = ..0usize;

    // TODO: instead of marking buffer - enforce this guarantee in relocation collection.

    while let Some(entry) = entries.next() {
        let red_after = entries
            .peek()
            .map(|e| (e.offset as usize - entity.original_range.start)..)
            .unwrap_or(entity.bytes.len()..);

        let reloc = entry.shift_left(entity.original_range.start);

        let cursor = Cursor::new(
            entity.bytes,
            reloc.relocation_range(),
            prev_range,
            red_after,
        );

        if let Some((rewrite, extra_data)) = handler.create_entry(cursor, reloc)? {
            entity.fixups.push(rewrite);
            result.push(extra_data);
        }

        prev_range = ..reloc.relocation_range().end;
    }
    Ok(result)
}

const _ASSERT_SIZE: () = {
    assert!(
        std::mem::size_of::<OutputRelocationEntry>()
            >= std::mem::size_of::<EntityRelocationEntry>()
    );
};
