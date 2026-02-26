use cranelift_entity::packed_option::ReservedValue;
use derive_more::{Display, From};

use crate::{
    linkage::reloc::SymbolType,
    raw::FuncTypeId,
    typed::{FunctionRef, GlobalRef, MemoryRef, Module, TableRef, TagRef, data::DataSymbolRef},
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

    #[display = "entity"]
    /// A reference to entity which type is provided by external tag.
    /// used for relocs where symbols type is described by relocation type.
    ///
    /// In might be intuitive to replace symbol_id + symbol_type in reloc entry with tagged `EntityKind`,
    /// but symbol_type contain not only information about entity type, but also "mode" in which this symbol is used
    /// (e.g. function index vs function offset vs table index, memaddr vs memlocrel).
    ///
    pub struct ErasedEntityRef;
}

impl ErasedEntityRef {
    pub fn combine(self, tag: SymbolType) -> EntityKind {
        match tag {
            SymbolType::FunctionIndex | SymbolType::TableIndex => {
                EntityKind::Function(FunctionRef::from_u32(self.as_u32()))
            }
            SymbolType::GlobalIndex => EntityKind::Global(GlobalRef::from_u32(self.as_u32())),
            SymbolType::TableNumber => EntityKind::Table(TableRef::from_u32(self.as_u32())),
            SymbolType::MemoryAddr => {
                EntityKind::DataSymbol(DataSymbolRef::from_u32(self.as_u32()))
            }
            SymbolType::TypeIndex => EntityKind::Type(FuncTypeId::from_u32(self.as_u32())),
            SymbolType::EventIndex => EntityKind::Tag(TagRef::from_u32(self.as_u32())),
            _ => panic!("Unsupported symbol type for entity reference: {:?}", tag),
        }
    }
}

impl From<GlobalRef> for ErasedEntityRef {
    fn from(global_ref: GlobalRef) -> Self {
        ErasedEntityRef::from_u32(global_ref.as_u32())
    }
}
impl From<FunctionRef> for ErasedEntityRef {
    fn from(func_ref: FunctionRef) -> Self {
        ErasedEntityRef::from_u32(func_ref.as_u32())
    }
}
impl From<TableRef> for ErasedEntityRef {
    fn from(table_ref: TableRef) -> Self {
        ErasedEntityRef::from_u32(table_ref.as_u32())
    }
}
impl From<DataSymbolRef> for ErasedEntityRef {
    fn from(data_symbol_ref: DataSymbolRef) -> Self {
        ErasedEntityRef::from_u32(data_symbol_ref.as_u32())
    }
}

/// A tagged reference to an entity in a WebAssembly module.
/// Can be converted to `FlatEntityRef` in order to get a unified index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, From, Display)]
#[display("{_0}")]
pub enum EntityKind {
    Function(FunctionRef),
    DataSymbol(DataSymbolRef),
    Global(GlobalRef),
    Table(TableRef),
    Memory(MemoryRef),
    Tag(TagRef),
    Type(FuncTypeId),
}

impl ReservedValue for EntityKind {
    fn reserved_value() -> Self {
        EntityKind::Type(FuncTypeId::from_u32(u32::MAX))
    }

    fn is_reserved_value(&self) -> bool {
        matches!(self, EntityKind::Type(t) if t.as_u32() == u32::MAX)
    }
}
impl EntityKind {
    pub fn is_function(&self) -> bool {
        matches!(self, EntityKind::Function(_))
    }
    pub fn is_data(&self) -> bool {
        matches!(self, EntityKind::DataSymbol(_))
    }
}

/// A snapshot of the number of entities in a WebAssembly module.
/// Used to convert between `TaggedEntityRef` and `FlatEntityRef`.
///
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntitiesSnapshot {
    num_function_refs: u32,
    num_data_symbol_refs: u32,
    num_global_refs: u32,
    num_table_refs: u32,
    num_memory_refs: u32,
    num_tag_refs: u32,
}

impl EntitiesSnapshot {
    /// Create a snapshot with arbitrary numbers for testing purposes.
    pub fn for_testing() -> Self {
        Self {
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
            num_function_refs: object.functions.len() as u32,
            num_data_symbol_refs: object.data.len() as u32,
            num_global_refs: object.globals.len() as u32,
            num_table_refs: object.tables.len() as u32,
            num_memory_refs: object.memories.len() as u32,
            num_tag_refs: object.tags.len() as u32,
        }
    }

    pub fn pack_ref(&self, symbol: impl Into<EntityKind>) -> FlatEntityRef {
        match symbol.into() {
            EntityKind::Function(f) => FlatEntityRef::from_u32(f.as_u32()),

            EntityKind::Global(g) => FlatEntityRef::from_u32(g.as_u32() + self.num_function_refs),
            EntityKind::Table(t) => {
                FlatEntityRef::from_u32(t.as_u32() + self.num_function_refs + self.num_global_refs)
            }
            EntityKind::Memory(m) => FlatEntityRef::from_u32(
                m.as_u32() + self.num_function_refs + self.num_global_refs + self.num_table_refs,
            ),
            EntityKind::Tag(t) => FlatEntityRef::from_u32(
                t.as_u32()
                    + self.num_function_refs
                    + self.num_global_refs
                    + self.num_table_refs
                    + self.num_memory_refs,
            ),
            EntityKind::DataSymbol(d) => FlatEntityRef::from_u32(
                d.as_u32()
                    + self.num_function_refs
                    + self.num_global_refs
                    + self.num_table_refs
                    + self.num_memory_refs
                    + self.num_tag_refs,
            ),
            EntityKind::Type(t) => FlatEntityRef::from_u32(
                t.as_u32()
                    + self.num_function_refs
                    + self.num_global_refs
                    + self.num_table_refs
                    + self.num_memory_refs
                    + self.num_tag_refs
                    + self.num_data_symbol_refs,
            ),
        }
    }
    pub fn unpack_ref(&self, any_ref: FlatEntityRef) -> EntityKind {
        let idx = any_ref.as_u32();
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
        typed::{FunctionRef, LinkingFile, common_index::EntityKind, data::DataSymbolRef},
    };

    #[test]
    fn test_entity_ref_mapping() {
        let file = crate::testfiles::EXAMPLE_WASM;
        let info = LinkingFile::from_wasm_bytes(file).unwrap();
        let module = info.module;

        let snapshot = EntitiesSnapshot::new(&module);

        module.functions.items.iter().for_each(|(func_ref, _)| {
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
