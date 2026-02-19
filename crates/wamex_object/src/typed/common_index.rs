use crate::{
    linkage::reloc::SymbolType,
    typed::{FunctionRef, GlobalRef, MemoryRef, Module, TableRef, TagRef, data::DataSymbolRef},
};

impl_entity_index! {
    #[display = "anyref"]
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
    /// DataSymbolRef is placed last because unlike others they count can be retrieved only after parsing linking section.
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntityKind {
    Function(FunctionRef),
    DataSymbol(DataSymbolRef),
    Global(GlobalRef),
    Table(TableRef),
    Memory(MemoryRef),
    Tag(TagRef),
}

/// A snapshot of the number of entities in a WebAssembly module.
/// Used to convert between `TaggedEntityRef` and `FlatEntityRef`.
pub struct EntitiesSnapshot {
    num_function_refs: u32,
    num_data_symbol_refs: u32,
    num_global_refs: u32,
    num_table_refs: u32,
    num_memory_refs: u32,
    num_tag_refs: u32,
}

impl EntitiesSnapshot {
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

    pub fn pack_ref(&self, symbol: EntityKind) -> FlatEntityRef {
        match symbol {
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
        } else {
            EntityKind::DataSymbol(DataSymbolRef::from_u32(
                idx - self.num_function_refs
                    - self.num_global_refs
                    - self.num_table_refs
                    - self.num_memory_refs
                    - self.num_tag_refs,
            ))
        }
    }
}
