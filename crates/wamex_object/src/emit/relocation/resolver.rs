use std::collections::HashMap;

use crate::{
    emit::relocation::EntityLocation,
    typed::{EntitiesMultiMap, EntityKind},
};

#[derive(Debug, Default)]
pub struct OutputEntitiesResolver {
    // pub module: ModuleAndDataInfo<'src, Building>,

    // We don't copy relocs, so we need FileId to get needed `FileRelocs` and request it with relocs list for entity.
    src_map: EntitiesMultiMap<EntityLocation>,
    // Map from output entity to src entities.
    remapped_entity: HashMap<EntityLocation, EntityKind>,
}

impl OutputEntitiesResolver {
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

    /// Iterate over all mapped entities.
    pub fn iter_mapped(&self) -> impl Iterator<Item = (&EntityLocation, &EntityKind)> {
        self.remapped_entity.iter()
    }
}
