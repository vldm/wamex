use cranelift_entity::packed_option::ReservedValue;

use crate::{
    Module,
    read::{FunctionRef, GlobalRef, MemoryRef, TableRef, TagRef, typed::data::DataSymbolRef},
};

impl_entity_index! {
    #[display = "anyref"]
    /// A reference to any entity in a WebAssembly module.
    /// It is used only for module with known structure (cannot be used for builder).
    ///
    /// Implementation note about mapping to actual entity types:
    /// - FunctionRef => AnyEntityRef::from_u32(func_ref)
    /// - GlobalRef => AnyEntityRef::from_u32(global_ref + num_function_refs )
    /// - TableRef => AnyEntityRef::from_u32(table_ref + num_function_refs + num_global_refs)
    /// - MemoryRef => AnyEntityRef::from_u32(memory_ref + num_function_refs + num_global_refs + num_table_refs)
    /// - TagRef => AnyEntityRef::from_u32(tag_ref + num_function_refs + num_global_refs + num_table_refs + num_memory_refs)
    /// - DataSymbolRef => AnyEntityRef::from_u32(data_symbol_ref + num_function_refs + num_global_refs + num_table_refs + num_memory_refs + num_tag_refs)
    /// DataSymbolRef is placed last because unlike others they count can be retrieved only after parsing linking section.
    pub struct AnyEntityRef;

    #[display = "entity"]
    /// A reference to entity which type is provided by external tag.
    /// used for relocs where symbols type is described by relocation type.
    // TODO: Maybe replace usage by `TaggedEntityRef`?
    pub struct ErasedEntityRef;
}

/// A tagged reference to an entity in a WebAssembly module.
/// Can be converted to `AnyEntityRef` in order to get a unified index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TaggedEntityRef {
    Function(FunctionRef),
    DataSymbol(DataSymbolRef),
    Global(GlobalRef),
    Table(TableRef),
    Memory(MemoryRef),
    Tag(TagRef),
}

/// A snapshot of the number of entities in a WebAssembly module.
/// Used to convert between `TaggedEntityRef` and `AnyEntityRef`.
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

    pub fn as_any_ref(&self, symbol: &TaggedEntityRef) -> AnyEntityRef {
        match symbol {
            TaggedEntityRef::Function(f) => AnyEntityRef::from_u32(f.as_u32()),

            TaggedEntityRef::Global(g) => {
                AnyEntityRef::from_u32(g.as_u32() + self.num_function_refs)
            }
            TaggedEntityRef::Table(t) => {
                AnyEntityRef::from_u32(t.as_u32() + self.num_function_refs + self.num_global_refs)
            }
            TaggedEntityRef::Memory(m) => AnyEntityRef::from_u32(
                m.as_u32() + self.num_function_refs + self.num_global_refs + self.num_table_refs,
            ),
            TaggedEntityRef::Tag(t) => AnyEntityRef::from_u32(
                t.as_u32()
                    + self.num_function_refs
                    + self.num_global_refs
                    + self.num_table_refs
                    + self.num_memory_refs,
            ),
            TaggedEntityRef::DataSymbol(d) => AnyEntityRef::from_u32(
                d.as_u32()
                    + self.num_function_refs
                    + self.num_global_refs
                    + self.num_table_refs
                    + self.num_memory_refs
                    + self.num_tag_refs,
            ),
        }
    }
    pub fn unpack_any_ref(&self, any_ref: AnyEntityRef) -> TaggedEntityRef {
        let idx = any_ref.as_u32();
        if idx < self.num_function_refs {
            TaggedEntityRef::Function(FunctionRef::from_u32(idx))
        } else if idx < self.num_function_refs + self.num_global_refs {
            TaggedEntityRef::Global(GlobalRef::from_u32(idx - self.num_function_refs))
        } else if idx < self.num_function_refs + self.num_global_refs + self.num_table_refs {
            TaggedEntityRef::Table(TableRef::from_u32(
                idx - self.num_function_refs - self.num_global_refs,
            ))
        } else if idx
            < self.num_function_refs
                + self.num_global_refs
                + self.num_table_refs
                + self.num_memory_refs
        {
            TaggedEntityRef::Memory(MemoryRef::from_u32(
                idx - self.num_function_refs - self.num_global_refs - self.num_table_refs,
            ))
        } else if idx
            < self.num_function_refs
                + self.num_global_refs
                + self.num_table_refs
                + self.num_memory_refs
                + self.num_tag_refs
        {
            TaggedEntityRef::Tag(TagRef::from_u32(
                idx - self.num_function_refs
                    - self.num_global_refs
                    - self.num_table_refs
                    - self.num_memory_refs,
            ))
        } else {
            TaggedEntityRef::DataSymbol(DataSymbolRef::from_u32(
                idx - self.num_function_refs
                    - self.num_global_refs
                    - self.num_table_refs
                    - self.num_memory_refs
                    - self.num_tag_refs,
            ))
        }
    }
}
