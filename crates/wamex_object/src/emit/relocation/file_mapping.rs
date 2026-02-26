use anyhow::Result;
use cranelift_entity::{PrimaryMap, packed_option::ReservedValue};

use crate::{
    emit::memory_layout::{DataSymbolOffset, DataSymbolsOffsets},
    index::GappedMap,
    linkage::file_db::FileRelocs,
    typed::{
        EntitiesMultiMap, FileId, Module,
        common_index::{EntitiesSnapshot, EntityKind, FlatEntityRef},
    },
};

/// Composite reference to an entity in some file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileEntityRef {
    pub file_id: FileId,
    pub entity_kind: EntityKind,
}
impl FileEntityRef {
    pub fn from_parts(file_id: FileId, entity_kind: EntityKind) -> Self {
        Self {
            file_id,
            entity_kind,
        }
    }
}

/// Input file information:
/// - list of relocs for entities in file - we avoid storing them in entity itself, since every reloc is bound to other symbol.
/// - Information where to find entity in output modules (output_map) - needed to find lib base and entity offset/index of specific relocs.
/// - also contain snapshot - just to be able to process FlatEntityRef of specific file.
struct InputFileInfo {
    relocs: FileRelocs,
    // Map from entity in input module to entity in output module.
    // Used to apply relocs.
    // There could be more than one output module, in case of split.
    output_map: GappedMap<FlatEntityRef, FileEntityRef>,

    snapshot: EntitiesSnapshot,
}
impl InputFileInfo {
    pub fn new(module: &Module, relocs: FileRelocs) -> Self {
        Self {
            snapshot: EntitiesSnapshot::new(module),
            relocs,
            output_map: GappedMap::new(),
        }
    }
    pub fn push_mapping(
        &mut self,
        src: impl Into<EntityKind>,
        output_file: FileId,
        output_ref: impl Into<EntityKind>,
    ) {
        let src = self.snapshot.pack_ref(src.into());
        self.output_map.insert(
            src,
            FileEntityRef::from_parts(output_file, output_ref.into()),
        );
    }

    pub fn get_output_ref(&self, src: impl Into<EntityKind>) -> Option<FileEntityRef> {
        let src = self.snapshot.pack_ref(src.into());
        self.get_output_flat_ref(src)
    }

    pub fn get_output_flat_ref(&self, src: FlatEntityRef) -> Option<FileEntityRef> {
        self.output_map.get(src).cloned()
    }
}

///
/// Information about linkages between files.
///
struct FileLinkageInfo<'src> {
    input_files: PrimaryMap<FileId, InputFileInfo>,
    output_files: PrimaryMap<FileId, OutputFileInfo<'src>>,
}

struct OutputFileInfo<'src> {
    module: Module<'src>,
    data_offsets: DataSymbolsOffsets,
    // We don't copy relocs, so we need FileId to get needed `FileRelocs` and request it with relocs list for entity.
    input_map: EntitiesMultiMap<FileEntityRef>,
}

impl OutputFileInfo<'_> {
    // fn apply_relocs(
    //     &self,
    //     target: &mut [u8],
    //     relocs: impl Iterator<Item = RelocationEntry<ErasedEntityRef>>,
    // ) -> Result<()> {
    //     for reloc in relocs {
    //         todo!()
    //     }
    //     Ok(())
    // }
}

impl ReservedValue for FileEntityRef {
    fn reserved_value() -> Self {
        FileEntityRef {
            file_id: FileId::reserved_value(),
            entity_kind: EntityKind::reserved_value(),
        }
    }

    fn is_reserved_value(&self) -> bool {
        self.file_id.is_reserved_value() && self.entity_kind.is_reserved_value()
    }
}
