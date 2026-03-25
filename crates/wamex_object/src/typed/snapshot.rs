use crate::{
    raw::FuncTypeId,
    typed::{
        EntityKind, FunctionRef, GlobalRef, MemoryRef, Module, TableRef, TagRef,
        data::DataSymbolRef,
    },
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
// TODO: use range instead of counts to simplify unpacking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntitiesSnapshot {
    // if this module is loaded into same address space with others,
    // the offset of entities in flat index space is needed to pack/unpack refs.
    pub(crate) num_file_offset: u32,
    pub(crate) num_function_refs: u32,
    pub(crate) num_data_symbol_refs: u32,
    pub(crate) num_global_refs: u32,
    pub(crate) num_table_refs: u32,
    pub(crate) num_memory_refs: u32,
    pub(crate) num_tag_refs: u32,
}

impl EntitiesSnapshot {
    /// Create a snapshot with arbitrary numbers for testing purposes.
    pub fn for_testing() -> Self {
        Self {
            num_file_offset: 150,
            num_function_refs: 10,
            num_data_symbol_refs: 5,
            num_global_refs: 3,
            num_table_refs: 2,
            num_memory_refs: 1,
            num_tag_refs: 4,
        }
    }
    pub fn new(object: &Module<'_>) -> Self {
        Self {
            num_file_offset: 0,
            num_function_refs: object.functions.len() as u32,
            num_data_symbol_refs: object.data.len() as u32,
            num_global_refs: object.globals.len() as u32,
            num_table_refs: object.tables.len() as u32,
            num_memory_refs: object.memories.len() as u32,
            num_tag_refs: object.tags.len() as u32,
        }
    }

    #[inline]
    pub fn pack_ref(&self, symbol: impl Into<EntityKind>) -> FlatEntityRef {
        FlatEntityRef::from_u32(
            self.num_file_offset
                + match symbol.into() {
                    EntityKind::Function(f) => f.as_u32(),
                    EntityKind::Global(g) => g.as_u32() + self.num_function_refs,
                    EntityKind::Table(t) => {
                        t.as_u32() + self.num_function_refs + self.num_global_refs
                    }
                    EntityKind::Memory(m) => {
                        m.as_u32()
                            + self.num_function_refs
                            + self.num_global_refs
                            + self.num_table_refs
                    }
                    EntityKind::Tag(t) => {
                        t.as_u32()
                            + self.num_function_refs
                            + self.num_global_refs
                            + self.num_table_refs
                            + self.num_memory_refs
                    }
                    EntityKind::DataSymbol(d) => {
                        d.as_u32()
                            + self.num_function_refs
                            + self.num_global_refs
                            + self.num_table_refs
                            + self.num_memory_refs
                            + self.num_tag_refs
                    }
                    EntityKind::Type(t) => {
                        t.as_u32()
                            + self.num_function_refs
                            + self.num_global_refs
                            + self.num_table_refs
                            + self.num_memory_refs
                            + self.num_tag_refs
                            + self.num_data_symbol_refs
                    }
                },
        )
    }

    #[inline]
    pub fn unpack_ref(&self, any_ref: FlatEntityRef) -> EntityKind {
        let idx = any_ref.as_u32().checked_sub(self.num_file_offset).unwrap();
        if idx < self.num_function_refs {
            EntityKind::Function(FunctionRef::from_u32(idx))
        } else if idx < self.num_function_refs + self.num_global_refs {
            EntityKind::Global(GlobalRef::from_u32(idx - self.num_function_refs))
        } else if idx < self.num_function_refs + self.num_global_refs + self.num_table_refs {
            EntityKind::Table(TableRef::from_u32(
                idx - self.num_function_refs - self.num_global_refs,
            ))
        } else if idx
            < self.num_function_refs
                + self.num_global_refs
                + self.num_table_refs
                + self.num_memory_refs
        {
            EntityKind::Memory(MemoryRef::from_u32(
                idx - self.num_function_refs - self.num_global_refs - self.num_table_refs,
            ))
        } else if idx
            < self.num_function_refs
                + self.num_global_refs
                + self.num_table_refs
                + self.num_memory_refs
                + self.num_tag_refs
        {
            EntityKind::Tag(TagRef::from_u32(
                idx - self.num_function_refs
                    - self.num_global_refs
                    - self.num_table_refs
                    - self.num_memory_refs,
            ))
        } else if idx
            < self.num_function_refs
                + self.num_global_refs
                + self.num_table_refs
                + self.num_memory_refs
                + self.num_tag_refs
                + self.num_data_symbol_refs
        {
            EntityKind::DataSymbol(DataSymbolRef::from_u32(
                idx - self.num_function_refs
                    - self.num_global_refs
                    - self.num_table_refs
                    - self.num_memory_refs
                    - self.num_tag_refs,
            ))
        } else {
            EntityKind::Type(FuncTypeId::from_u32(
                idx - self.num_function_refs
                    - self.num_global_refs
                    - self.num_table_refs
                    - self.num_memory_refs
                    - self.num_tag_refs
                    - self.num_data_symbol_refs,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EntitiesSnapshot;
    use crate::{
        raw::FuncTypeId,
        typed::{EntityKind, FunctionRef, LoadedFile, data::DataSymbolRef},
    };
    #[test]
    fn test_entity_ref_mapping() {
        let file = crate::testfiles::EXAMPLE_WASM;
        let info = LoadedFile::from_wasm_bytes(file).unwrap();
        let module = info.module;

        let snapshot = EntitiesSnapshot::new(&module);

        module.functions.iter().for_each(|(func_ref, _)| {
            let entity_kind = EntityKind::Function(func_ref);
            let flat_ref = snapshot.pack_ref(func_ref);
            let unpacked = snapshot.unpack_ref(flat_ref);
            assert_eq!(entity_kind, unpacked);
        });

        module.data.iter().for_each(|(data_ref, _)| {
            let entity_kind = EntityKind::DataSymbol(data_ref);
            let flat_ref = snapshot.pack_ref(data_ref);
            let unpacked = snapshot.unpack_ref(flat_ref);
            assert_eq!(entity_kind, unpacked);
        });
    }

    #[test]
    fn check_each_entity_for_snapshot() {
        let snapshot = EntitiesSnapshot::for_testing();
        for fns in 0..snapshot.num_function_refs {
            let func_ref = EntityKind::Function(FunctionRef::from_u32(fns));
            let flat_ref = snapshot.pack_ref(func_ref);
            let unpacked = snapshot.unpack_ref(flat_ref);
            assert_eq!(func_ref, unpacked);
        }
        let invalid_fn = EntityKind::Function(FunctionRef::from_u32(snapshot.num_function_refs));
        let flat_ref = snapshot.pack_ref(invalid_fn);
        let unpacked = snapshot.unpack_ref(flat_ref);
        assert_ne!(invalid_fn, unpacked);

        for data in 0..snapshot.num_data_symbol_refs {
            let data_ref = EntityKind::DataSymbol(DataSymbolRef::from_u32(data));
            let flat_ref = snapshot.pack_ref(data_ref);
            let unpacked = snapshot.unpack_ref(flat_ref);
            assert_eq!(data_ref, unpacked);
        }
        let invalid_data =
            EntityKind::DataSymbol(DataSymbolRef::from_u32(snapshot.num_data_symbol_refs));
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
}
