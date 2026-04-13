use cranelift_entity::PrimaryMap;

use crate::{
    emit::relocation::EntityLocation,
    layouts::DataSymbolRef,
    raw::FuncTypeId,
    typed::{EntityKind, FileId, FunctionRef, GlobalRef, MemoryRef, Module, TableRef, TagRef},
};

impl_entity_index! {
    #[display = ""] // default id print without prefix
    /// A reference to any entity in a WebAssembly module.
    /// Within flat index space of entities.
    /// It is used only for module with known structure (cannot be used for builder).
    ///
    /// Implementation note about mapping to actual entity types:
    /// - FunctionRef => FlatEntityRef::from_u32(func_ref)
    /// - GlobalRef => FlatEntityRef::from_u32(global_ref + num_function_refs )
    /// - TableRef => FlatEntityRef::from_u32(table_ref + num_function_refs + num_global_refs)
    /// - MemoryRef => FlatEntityRef::from_u32(memory_ref + num_function_refs + num_global_refs + num_table_refs)
    /// - TagRef => FlatEntityRef::from_u32(tag_ref + num_function_refs + num_global_refs + num_table_refs + num_memory_refs)
    /// - DataSymbolRef => FlatEntityRef::from_u32(data_symbol_ref + num_function_refs + num_global_refs + num_table_refs + num_memory_refs + num_tag_refs)
    /// - FuncTypeId => FlatEntityRef::from_u32(type_ref + num_function_refs + num_global_refs + num_table_refs + num_memory_refs + num_tag_refs + num_data_symbol_refs)
    pub struct FlatEntityRef;


}
/// A snapshot of the number of entities in a WebAssembly module.
/// Used to convert between `TaggedEntityRef` and `FlatEntityRef`.
///
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntitiesSnapshot {
    // if this module is loaded into same address space with others,
    // the offset of entities in flat index space is needed to pack/unpack refs.
    pub(crate) num_file_offset: u32,
    pub(crate) functions_end: u32,
    pub(crate) globals_end: u32,
    pub(crate) tables_end: u32,
    pub(crate) memories_end: u32,
    pub(crate) tags_end: u32,
    pub(crate) data_end: u32,
    pub(crate) total_end: u32,
}

impl EntitiesSnapshot {
    /// Create a snapshot with arbitrary numbers for testing purposes.
    #[must_use]
    pub fn for_testing() -> Self {
        Self {
            num_file_offset: 150,
            functions_end: 10,
            globals_end: 13,
            tables_end: 15,
            memories_end: 16,
            tags_end: 20,
            data_end: 25,
            total_end: 35,
        }
    }

    #[must_use]
    pub fn new_without_types(object: &Module<'_>) -> Self {
        let num_function_refs = object.functions.len() as u32;
        let num_global_refs = object.globals.len() as u32;
        let num_table_refs = object.tables.len() as u32;
        let num_memory_refs = object.memories.len() as u32;
        let num_tag_refs = object.tags.len() as u32;
        let num_data_symbol_refs = object.extra.mem_layout.len() as u32;

        Self {
            num_file_offset: 0,
            functions_end: num_function_refs,
            globals_end: num_function_refs + num_global_refs,
            tables_end: num_function_refs + num_global_refs + num_table_refs,
            memories_end: num_function_refs + num_global_refs + num_table_refs + num_memory_refs,
            tags_end: num_function_refs
                + num_global_refs
                + num_table_refs
                + num_memory_refs
                + num_tag_refs,
            data_end: num_function_refs
                + num_global_refs
                + num_table_refs
                + num_memory_refs
                + num_tag_refs
                + num_data_symbol_refs,
            total_end: num_function_refs
                + num_global_refs
                + num_table_refs
                + num_memory_refs
                + num_tag_refs
                + num_data_symbol_refs,
        }
    }

    #[must_use]
    pub fn with_num_type_refs(mut self, num_type_refs: u32) -> Self {
        self.total_end = self.data_end + num_type_refs;
        self
    }

    #[must_use]
    pub fn with_offset(mut self, num_file_offset: u32) -> Self {
        self.num_file_offset = num_file_offset;
        self
    }

    #[inline]
    #[must_use]
    pub fn num_function_refs(&self) -> u32 {
        self.functions_end
    }

    #[inline]
    #[must_use]
    pub fn num_global_refs(&self) -> u32 {
        self.globals_end - self.functions_end
    }

    #[inline]
    #[must_use]
    pub fn num_table_refs(&self) -> u32 {
        self.tables_end - self.globals_end
    }

    #[inline]
    #[must_use]
    pub fn num_memory_refs(&self) -> u32 {
        self.memories_end - self.tables_end
    }

    #[inline]
    #[must_use]
    pub fn num_tag_refs(&self) -> u32 {
        self.tags_end - self.memories_end
    }

    #[inline]
    #[must_use]
    pub fn num_data_symbol_refs(&self) -> u32 {
        self.data_end - self.tags_end
    }

    #[inline]
    #[must_use]
    pub fn num_type_refs(&self) -> u32 {
        self.total_end - self.data_end
    }

    #[inline]
    #[must_use]
    pub fn local_end(&self) -> u32 {
        self.total_end
    }

    #[inline]
    #[must_use]
    pub fn global_start(&self) -> u32 {
        self.num_file_offset
    }

    #[inline]
    #[must_use]
    pub fn global_end(&self) -> u32 {
        self.num_file_offset + self.local_end()
    }

    #[inline]
    #[must_use]
    pub fn contains(&self, any_ref: FlatEntityRef) -> bool {
        let idx = any_ref.as_u32();
        self.global_start() <= idx && idx < self.global_end()
    }

    #[inline]
    pub fn pack_ref(&self, symbol: impl Into<EntityKind>) -> FlatEntityRef {
        FlatEntityRef::from_u32(
            self.num_file_offset
                + match symbol.into() {
                    EntityKind::Function(f) => f.as_u32(),
                    EntityKind::Global(g) => g.as_u32() + self.functions_end,
                    EntityKind::Table(t) => t.as_u32() + self.globals_end,
                    EntityKind::Memory(m) => m.as_u32() + self.tables_end,
                    EntityKind::Tag(t) => t.as_u32() + self.memories_end,
                    EntityKind::DataSymbol(d) => d.as_u32() + self.tags_end,
                    EntityKind::Type(t) => t.as_u32() + self.data_end,
                },
        )
    }

    #[inline]
    #[must_use]
    pub fn unpack_ref(&self, any_ref: FlatEntityRef) -> EntityKind {
        let idx = any_ref.as_u32().checked_sub(self.num_file_offset).unwrap();
        if idx < self.functions_end {
            EntityKind::Function(FunctionRef::from_u32(idx))
        } else if idx < self.globals_end {
            EntityKind::Global(GlobalRef::from_u32(idx - self.functions_end))
        } else if idx < self.tables_end {
            EntityKind::Table(TableRef::from_u32(idx - self.globals_end))
        } else if idx < self.memories_end {
            EntityKind::Memory(MemoryRef::from_u32(idx - self.tables_end))
        } else if idx < self.tags_end {
            EntityKind::Tag(TagRef::from_u32(idx - self.memories_end))
        } else if idx < self.data_end {
            EntityKind::DataSymbol(DataSymbolRef::from_u32(idx - self.tags_end))
        } else {
            EntityKind::Type(FuncTypeId::from_u32(idx - self.data_end))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultiSnapshot {
    files: PrimaryMap<FileId, EntitiesSnapshot>,
    total_entities: u32,
}

impl MultiSnapshot {
    #[must_use]
    pub fn single(module: &Module<'_>) -> Self {
        Self::new([EntitiesSnapshot::new_without_types(module)])
    }

    pub fn new(snapshots: impl IntoIterator<Item = EntitiesSnapshot>) -> Self {
        let mut total_entities = 0;
        let files = snapshots
            .into_iter()
            .map(|snapshot| {
                let snapshot = snapshot.with_offset(total_entities);
                total_entities = snapshot.global_end();
                snapshot
            })
            .collect();

        Self {
            files,
            total_entities,
        }
    }

    #[inline]
    #[must_use]
    pub fn total_entities(&self) -> u32 {
        self.total_entities
    }

    #[inline]
    #[must_use]
    pub fn num_files(&self) -> usize {
        self.files.len()
    }

    #[inline]
    #[must_use]
    pub fn file_snapshot(&self, file_id: FileId) -> &EntitiesSnapshot {
        &self.files[file_id]
    }

    #[inline]
    #[must_use]
    pub fn pack_ref(&self, loc: EntityLocation) -> FlatEntityRef {
        self.file_snapshot(loc.file_id).pack_ref(loc.entity)
    }

    #[must_use]
    pub fn unpack_ref(&self, any_ref: FlatEntityRef) -> EntityLocation {
        let flat = any_ref.as_u32();
        assert!(
            flat < self.total_entities,
            "Flat entity ref {flat} is out of bounds"
        );

        let idx = self
            .files
            .as_values_slice()
            .partition_point(|snapshot| snapshot.global_start() <= flat)
            .checked_sub(1)
            .expect("MultiSnapshot should contain at least one file for unpacking");

        let file_id = FileId::from_u32(idx as u32);
        let snapshot = &self.files[file_id];
        EntityLocation::from_parts(file_id, snapshot.unpack_ref(any_ref))
    }
}

#[cfg(test)]
mod tests {
    use super::{EntitiesSnapshot, MultiSnapshot};
    use crate::{
        emit::relocation::EntityLocation,
        layouts::DataSymbolRef,
        raw::FuncTypeId,
        typed::{EntityKind, FileId, FileLoader, FunctionRef, LoadedFile},
    };
    #[test]
    fn test_entity_ref_mapping() {
        let file = crate::testfiles::EXAMPLE_WASM;
        let info = LoadedFile::from_wasm_bytes(file).unwrap();
        let module = info.module;

        let snapshot = EntitiesSnapshot::new_without_types(&module);

        module.functions.iter().for_each(|(func_ref, _)| {
            let entity_kind = EntityKind::Function(func_ref);
            let flat_ref = snapshot.pack_ref(func_ref);
            let unpacked = snapshot.unpack_ref(flat_ref);
            assert_eq!(entity_kind, unpacked);
        });

        module.extra.mem_layout.iter().for_each(|(data_ref, _)| {
            let entity_kind = EntityKind::DataSymbol(data_ref);
            let flat_ref = snapshot.pack_ref(data_ref);
            let unpacked = snapshot.unpack_ref(flat_ref);
            assert_eq!(entity_kind, unpacked);
        });
    }

    #[test]
    fn test_entity_ref_multi_mapping() {
        let file = crate::testfiles::EXAMPLE_WASM;
        let mut loader = FileLoader::new();
        let id = loader
            .load_from_bytes(file.to_vec().into_boxed_slice())
            .unwrap();

        let module = &loader.get_file(id).module;

        let snapshot = loader.get_snapshot();

        module.functions.iter().for_each(|(func_ref, _)| {
            let entity_kind = EntityLocation::from_parts(id, EntityKind::Function(func_ref));
            let flat_ref = snapshot.pack_ref(entity_kind);
            let unpacked = snapshot.unpack_ref(flat_ref);
            assert_eq!(entity_kind, unpacked);
        });

        module.extra.mem_layout.iter().for_each(|(data_ref, _)| {
            let entity_kind = EntityLocation::from_parts(id, EntityKind::DataSymbol(data_ref));
            let flat_ref = snapshot.pack_ref(entity_kind);
            let unpacked = snapshot.unpack_ref(flat_ref);
            assert_eq!(entity_kind, unpacked);
        });
    }

    #[test]
    fn check_each_entity_for_snapshot() {
        let snapshot = EntitiesSnapshot::for_testing();
        for fns in 0..snapshot.num_function_refs() {
            let func_ref = EntityKind::Function(FunctionRef::from_u32(fns));
            let flat_ref = snapshot.pack_ref(func_ref);
            let unpacked = snapshot.unpack_ref(flat_ref);
            assert_eq!(func_ref, unpacked);
        }
        let invalid_fn = EntityKind::Function(FunctionRef::from_u32(snapshot.num_function_refs()));
        let flat_ref = snapshot.pack_ref(invalid_fn);
        let unpacked = snapshot.unpack_ref(flat_ref);
        assert_ne!(invalid_fn, unpacked);

        for data in 0..snapshot.num_data_symbol_refs() {
            let data_ref = EntityKind::DataSymbol(DataSymbolRef::from_u32(data));
            let flat_ref = snapshot.pack_ref(data_ref);
            let unpacked = snapshot.unpack_ref(flat_ref);
            assert_eq!(data_ref, unpacked);
        }
        let invalid_data =
            EntityKind::DataSymbol(DataSymbolRef::from_u32(snapshot.num_data_symbol_refs()));
        let flat_ref = snapshot.pack_ref(invalid_data);
        let unpacked = snapshot.unpack_ref(flat_ref);
        assert_ne!(invalid_data, unpacked);

        for ty in 0..10 {
            // any number
            let tag_ref = EntityKind::Type(FuncTypeId::from_u32(ty));
            let flat_ref = snapshot.pack_ref(tag_ref);
            let unpacked = snapshot.unpack_ref(flat_ref);
            assert_eq!(tag_ref, unpacked);
        }
    }

    #[test]
    fn multi_snapshot_roundtrip_keeps_file_boundaries() {
        let snapshots = [
            EntitiesSnapshot::new_without_types(
                &LoadedFile::from_wasm_bytes(crate::testfiles::EXAMPLE_WASM)
                    .unwrap()
                    .module,
            ),
            EntitiesSnapshot::for_testing().with_offset(0),
        ];
        let snapshot = MultiSnapshot::new(snapshots);

        let file0_ref = snapshot.pack_ref(EntityLocation::from_parts(
            FileId::from_u32(0),
            EntityKind::Function(FunctionRef::from_u32(0)),
        ));
        let file1_ref = snapshot.pack_ref(EntityLocation::from_parts(
            FileId::from_u32(1),
            EntityKind::DataSymbol(DataSymbolRef::from_u32(2)),
        ));

        assert_eq!(
            snapshot.unpack_ref(file0_ref),
            EntityLocation::from_parts(
                FileId::from_u32(0),
                EntityKind::Function(FunctionRef::from_u32(0))
            )
        );
        assert_eq!(
            snapshot.unpack_ref(file1_ref),
            EntityLocation::from_parts(
                FileId::from_u32(1),
                EntityKind::DataSymbol(DataSymbolRef::from_u32(2))
            )
        );
        assert!(file0_ref.as_u32() < file1_ref.as_u32());
    }
}
