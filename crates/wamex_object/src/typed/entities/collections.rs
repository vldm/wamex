//! Wasm spec defines multiple types of entities https://webassembly.github.io/spec/core/syntax/types.html
//! for representing various concepts in wasm modules.
//!
//! Some of them are usefull for low-level representation of wasm modules, like value types (i32, i64, f32, f64, v128, funcref, externref),
//! that are used in function signatures, global types, table types, etc.
//!
//! Other types are more high-level and represent concepts like function types, table types, global types, memory types, etc.
//! These types are used to define the structure and behavior of wasm modules.
//!
//! This module agregate common functionality used for high-level wasm entities representation.
//! the list of supported types is based on `wasmparser::EntityType` and includes:
//! - Function types
//! - Table types
//! - Memory types
//! - Global types
//! - Tag types
//!
//! Common functionality includes:
//! - imports/defined indexing
//! - exports listing
//! - name resolution (from name section)
//!
//! unlike in `walrus` this information imported and defined entities are stored in different index spaces, so they don't need to be "relocated" during build.
//!

use std::borrow::Cow;

use cranelift_entity::{EntityRef, packed_option::ReservedValue};

use super::{FunctionRef, GlobalRef, MemoryRef, TableRef, TagRef, types::ExportEntry};
use crate::{
    index::{Building, CompoundList, GappedMap, ImportOrDefined, Locked, NonDefault, TempIndex},
    raw::FuncTypeId,
    typed::{
        DefinedDataChunk, DefinedFunction, DefinedGlobal, DefinedMemory, DefinedTable, DefinedTag,
        ImportedDataChunk, ImportedFunction, ImportedGlobal, ImportedMemory, ImportedTable,
        ImportedTag, WithExtraInfo, common_index::EntityKind, data::DataSymbolRef,
    },
};

pub type Functions<'src, BS = Locked> =
    EntityCollection<FunctionRef, ImportedFunction<'src>, DefinedFunction<'src>, BS>;
pub type Tables<'src, BS = Locked> =
    EntityCollection<TableRef, ImportedTable<'src>, DefinedTable<'src>, BS>;
pub type Globals<'src, BS = Locked> =
    EntityCollection<GlobalRef, ImportedGlobal<'src>, DefinedGlobal<'src>, BS>;
pub type Memories<'src, BS = Locked> =
    EntityCollection<MemoryRef, ImportedMemory<'src>, DefinedMemory<'src>, BS>;
pub type Tags<'src, BS = Locked> =
    EntityCollection<TagRef, ImportedTag<'src>, DefinedTag<'src>, BS>;

pub type DataChunks<'src, BS = Locked> =
    EntityCollection<DataSymbolRef, ImportedDataChunk<'src>, DefinedDataChunk<'src>, BS>;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EntityCollection<Ref, Import, Defined, BuilderState = Locked>
where
    Ref: TempIndex,
{
    /// Both defined and imports entities.
    pub items: CompoundList<Ref, Import, Defined, BuilderState>,
}

impl<Ref, Import, Defined> Default for EntityCollection<Ref, Import, Defined, Building>
where
    Ref: TempIndex,
{
    fn default() -> Self {
        Self {
            items: CompoundList::empty(),
        }
    }
}

impl<Ref, Import, Defined> EntityCollection<Ref, Import, Defined, Building>
where
    Ref: TempIndex,
{
    pub fn into_finished(self) -> EntityCollection<Ref, Import, Defined, Locked> {
        EntityCollection {
            items: self.items.into_finished(),
        }
    }
    /// Pushes a defined entity and returns its temporary reference.
    pub fn push_defined(&mut self, import: Defined) -> crate::index::Temp<Ref> {
        self.items.push_defined(import)
    }
    /// Pushes an import entity and returns its temporary reference.
    pub fn push_import(&mut self, import: Import) -> crate::index::Temp<Ref> {
        self.items.push_import(import)
    }
    /// Pushes either import or defined entity, depending on the variant of `ImportOrDefined`.
    pub fn push_entity(
        &mut self,
        entity: ImportOrDefined<Import, Defined>,
    ) -> crate::index::Temp<Ref> {
        match entity {
            ImportOrDefined::Import(import) => self.push_import(import),
            ImportOrDefined::Defined(defined) => self.push_defined(defined),
        }
    }
}

impl<'src, Ref, Import, Defined> EntityCollection<Ref, Import, Defined>
where
    Ref: TempIndex,
{
    pub fn from_parts(
        mut declared: CompoundList<Ref, Import, Defined>,
        mut names: GappedMap<Ref, NonDefault<Cow<'src, str>>>,
        exports: Vec<ExportEntry<'src, Ref>>,
    ) -> Self
    where
        Import: WithExtraInfo<'src>,
        Defined: WithExtraInfo<'src>,
    {
        for exports in &exports {
            let no_name = names.get(exports.entity_index).is_none();
            let is_defined = declared
                .get_entity(exports.entity_index)
                .to_defined()
                .is_some();
            // if name is not present in names map
            if no_name && is_defined {
                // insert it into names map
                names.insert(exports.entity_index, NonDefault::from(exports.name.clone()));
            }
            declared
                .get_entity_mut(exports.entity_index)
                .export_as_mut()
                .add_export(exports.name.clone());
        }

        for (r, name) in names.iter() {
            declared
                .get_entity_mut(r)
                .set_name(name.clone().into_inner());
        }

        Self { items: declared }
    }
    pub fn defined_iter(&self) -> impl ExactSizeIterator<Item = (Ref, &Defined)> {
        self.items.defined_iter()
    }
    pub fn defined_iter_mut(&mut self) -> impl ExactSizeIterator<Item = (Ref, &mut Defined)> {
        self.items.defined_iter_mut()
    }
    pub fn imports_iter(&self) -> impl ExactSizeIterator<Item = (Ref, &Import)> {
        self.items.imports_iter()
    }
    pub fn iter(&self) -> impl Iterator<Item = (Ref, ImportOrDefined<&Import, &Defined>)> {
        self.items.iter()
    }
    pub fn iter_all_ids(&self) -> impl ExactSizeIterator<Item = Ref> {
        (0..(self.items.imports.len() + self.items.defined.len())).map(EntityRef::new)
    }
    pub fn get_entity(&self, entity: Ref) -> ImportOrDefined<&Import, &Defined> {
        self.items.get_entity(entity)
    }
    pub fn len(&self) -> usize {
        self.items.imports.len() + self.items.defined.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn add_exports(&mut self, entity: Ref, export_name: Cow<'src, str>)
    where
        Import: WithExtraInfo<'src>,
        Defined: WithExtraInfo<'src>,
    {
        self.items
            .get_entity_mut(entity)
            .export_as_mut()
            .add_export(export_name);
    }
    pub fn exports_iter(&self) -> impl Iterator<Item = (Ref, Cow<'src, str>)>
    where
        Import: WithExtraInfo<'src>,
        Defined: WithExtraInfo<'src>,
    {
        self.items.iter().flat_map(|(r, entity)| {
            let iter = entity.export_as().names.clone().into_iter();

            iter.map(move |export_name| (r, export_name))
        })
    }
}

#[cfg(debug_assertions)]
mod assert_covariance {
    use super::*;

    macro_rules! assert_covariance {
        ($v:ident) => {
            impl<'long, V> $v<'long, V> {
                pub fn _assert_covariance<'short>(self) -> $v<'short, V>
                where
                    'long: 'short,
                {
                    self
                }
            }
        };
    }
    #[allow(
        dead_code,
        reason = "static assertion for covariance, not used directly"
    )]
    struct Invariant<'a, V> {
        _marker: std::marker::PhantomData<fn(&'a ()) -> &'a ()>,
        _marker2: std::marker::PhantomData<V>,
    }
    #[allow(
        dead_code,
        reason = "static assertion for covariance, not used directly"
    )]
    struct Covariant<'a, V> {
        _marker: std::marker::PhantomData<&'a ()>,
        _marker2: std::marker::PhantomData<V>,
    }

    assert_covariance!(Covariant);
    // Failed at the moment:
    assert_covariance!(Functions);
    assert_covariance!(Tables);
    assert_covariance!(Memories);
    assert_covariance!(Globals);
    assert_covariance!(Tags);

    // Expected to fail test
    // assert_covariance!(Invariant);
}

/// A primary map from input entity to some value.
/// Abstract over key - use `EntityKind`.
/// The implementation may vary, but instead of using `PrimaryMap<FlatEntityRef, Value>`
/// this collection should allow using it when EntitiesSnapshot cannot be created.

#[derive(Debug)]
pub struct EntitiesMultiMap<V: ReservedValue + Clone> {
    functions: GappedMap<FunctionRef, V>,
    tables: GappedMap<TableRef, V>,
    memories: GappedMap<MemoryRef, V>,
    globals: GappedMap<GlobalRef, V>,
    tags: GappedMap<TagRef, V>,
    // non "wasm entities"
    data: GappedMap<DataSymbolRef, V>,
    types: GappedMap<FuncTypeId, V>,
}
macro_rules! for_entities {
    ($entity: expr => $self:ident.$method:ident $(($($args:expr),+))?) => {
        match $entity {
            EntityKind::Function(func_ref) => $self.functions.$method(func_ref $(,$($args),+)?),
            EntityKind::Table(table_ref) => $self.tables.$method(table_ref $(,$($args),+)?),
            EntityKind::Memory(mem_ref) => $self.memories.$method(mem_ref $(,$($args),+)?),
            EntityKind::Global(global_ref) => $self.globals.$method(global_ref $(,$($args),+)?),
            EntityKind::Tag(tag_ref) => $self.tags.$method(tag_ref $(,$($args),+)?),
            EntityKind::DataSymbol(data_ref) => $self.data.$method(data_ref $(,$($args),+)?),
            EntityKind::Type(func_type_id) => $self.types.$method(func_type_id $(,$($args),+)?),
        }
    };
}
impl<V: Default + ReservedValue + Clone> Default for EntitiesMultiMap<V> {
    fn default() -> Self {
        Self {
            functions: GappedMap::new(),
            tables: GappedMap::new(),
            memories: GappedMap::new(),
            globals: GappedMap::new(),
            tags: GappedMap::new(),
            data: GappedMap::new(),
            types: GappedMap::new(),
        }
    }
}
impl<V: ReservedValue + Clone> EntitiesMultiMap<V> {
    pub fn get(&self, entity: EntityKind) -> Option<&V> {
        for_entities!(entity => self.get)
    }
    pub fn get_mut(&mut self, entity: EntityKind) -> Option<&mut V> {
        for_entities!(entity => self.get_mut)
    }
    pub fn insert(&mut self, entity: EntityKind, value: V) -> Option<V> {
        for_entities!(entity => self.insert(value))
    }
}
