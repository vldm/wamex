//! Code can be only in current module, all external symbols converted to:
//! - import fn
//! - trampoline to some indirect import (in general needs GOT base), but we use fixed offsets in indirect calls (part of layout)
//!
//! Data cannot be imported, so we need to know memory layout of external module, and we import GOT base for each dep module.
//! - main module, or current module should be referenced by offset
//! - other modules should be converted to GOT + offset.
//!
//! For relocs, we need to know:
//! - is it symbol rel based/or absolute.
//! - what GOT entry we need.
//!   so before processing relocs we need to provide:
//!
//! Common:
//! - Vec<MemLayout> for each module
//! - parent module: IndirectFnLayout
//!
//! Local info:
//! - Map<ImportDataSymbolRef, GotRef> // builded with module itself
//! - Map<(File, EntityKind), EntityKind> // mapping from input entity to entity in current module (How to deal with imported data?).
//! - Map<EntityKind, FileId> // to know where to search relocs/deps for each entity.
//!

use std::collections::HashMap;

use anyhow::Result;
use cranelift_entity::{PrimaryMap, packed_option::ReservedValue};

use crate::{
    emit::{
        memory_layout::{DataSymbolOffset, DataSymbolsOffsets},
        relocation::EntityLocation,
    },
    index::{Building, Finished, GappedMap},
    linkage::file_db::FileRelocs,
    typed::{
        EntitiesMultiMap, FileId, FileLoader, GlobalRef, Module, ModuleBuilder,
        common_index::{EntitiesSnapshot, EntityKind, FlatEntityRef},
        data::DataSymbolRef,
    },
};

///
/// Currently module doesn't provide mem layout.
/// So we need to recalculate it before emitting.
///
/// As a temporary solution this type exists.
// TODO: merge data_offsets awith module itself.
pub struct ModuleAndDataInfo<'src, S = Finished> {
    pub module: Module<'src, S>,
    pub data_offsets: DataSymbolsOffsets,
}
impl<'src> ModuleAndDataInfo<'src, Building> {
    pub fn new() -> Self {
        Self {
            module: Module::new(),
            data_offsets: DataSymbolsOffsets::new(),
        }
    }
}

pub struct OutputModules<'src> {
    modules: PrimaryMap<FileId, ModuleAndDataInfo<'src>>,
}

// pub struct OutputModuleInfo {
//     // Map<ImportDataSymbolRef, GotRef>
//     import_data_symbols: GappedMap<DataSymbolRef, GlobalRef>,
// }
#[derive(Debug)]
pub struct OutputFileInfo {
    // pub module: ModuleAndDataInfo<'src, Building>,

    // We don't copy relocs, so we need FileId to get needed `FileRelocs` and request it with relocs list for entity.
    src_map: EntitiesMultiMap<EntityLocation>,
    // Map from output entity to src entities.
    remapped_entity: HashMap<EntityLocation, EntityKind>,
}

impl OutputFileInfo {
    pub fn new() -> Self {
        Self {
            // module: ModuleAndDataInfo::new(),
            src_map: EntitiesMultiMap::default(),
            remapped_entity: HashMap::new(),
        }
    }
    pub fn add_entity_mapping(&mut self, src: EntityLocation, output: EntityKind) {
        self.src_map.insert(output, src);
        self.remapped_entity.insert(src, output);
    }

    /// Return src entity reference for given output entity, if exist.
    ///
    /// 1-st step of relocation processing:
    ///  - we need to know where to search array of relocs for given entity (get file id)
    pub fn get_entity_src(&self, output: EntityKind) -> Option<EntityLocation> {
        self.src_map.get(output).cloned()
    }

    /// Return output entity reference for given src entity, if exist.
    ///
    /// 2-nd step of relocation processing:
    ///  - we need to know where to search this entity
    pub fn get_output_entity(&self, src: &EntityLocation) -> Option<EntityKind> {
        self.remapped_entity.get(src).copied()
    }
}
