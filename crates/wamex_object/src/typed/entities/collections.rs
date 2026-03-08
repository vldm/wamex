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
    index::{GappedMap, NonDefault, Temp, TempIndex},
    raw::FuncTypeId,
    typed::{
        Building, DefinedDataChunk, DefinedEntity, DefinedFunction, DefinedGlobal, DefinedMemory,
        DefinedTable, DefinedTag, ExportNames, ImportedDataChunk, ImportedEntity, ImportedFunction,
        ImportedGlobal, ImportedMemory, ImportedTable, ImportedTag, Locked, WithExtraInfo,
        WithoutBody, common_index::EntityKind, data::DataSymbolRef,
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

///
/// One place for storing imports and defined entities.
///
/// It can have two states:
/// - `Building` - allows adding new entities, and returns temporary `Temp<Ref>` index.
/// - `Finished` - works with fixed structure, and returns/receives stable `Ref` index.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EntityCollection<Ref, Import, Defined, BuilderState = Locked>
where
    Ref: TempIndex,
{
    pub imports: Vec<Import>,
    pub defined: Vec<Defined>,
    _pd: std::marker::PhantomData<Ref>,
    _state: std::marker::PhantomData<BuilderState>,
}

impl<Ref, Import, Defined> Default for EntityCollection<Ref, Import, Defined, Building>
where
    Ref: TempIndex,
{
    fn default() -> Self {
        Self::empty()
    }
}

impl<Ref, Import, Defined> EntityCollection<Ref, Import, Defined, Building>
where
    Ref: TempIndex,
{
    pub fn empty() -> Self {
        Self {
            imports: Vec::new(),
            defined: Vec::new(),
            _pd: std::marker::PhantomData,
            _state: std::marker::PhantomData,
        }
    }

    pub fn new_raw(imports: Vec<Import>, defined: Vec<Defined>) -> Self {
        Self {
            imports,
            defined,
            _pd: std::marker::PhantomData,
            _state: std::marker::PhantomData,
        }
    }

    pub fn imports_slice(&self) -> &[Import] {
        self.imports.as_slice()
    }
    pub fn defined_slice(&self) -> &[Defined] {
        self.defined.as_slice()
    }

    pub fn into_finished(self) -> EntityCollection<Ref, Import, Defined, Locked> {
        EntityCollection {
            imports: self.imports,
            defined: self.defined,
            _pd: std::marker::PhantomData,
            _state: std::marker::PhantomData,
        }
    }

    /// Returns iterator over imported entities.
    /// The returned iterator yields pairs of (compound index, import reference).
    pub fn imports_iter(&self) -> impl ExactSizeIterator<Item = (Temp<Ref>, &Import)> {
        self.imports
            .iter()
            .enumerate()
            // import index is same as compound
            .map(|(import_id, import)| (Temp::from_import(import_id), import))
    }

    /// Returns iterator over defined entities.
    /// The returned iterator yields pairs of (compound index, defined reference).
    pub fn defined_iter(&self) -> impl ExactSizeIterator<Item = (Temp<Ref>, &Defined)> {
        self.defined
            .iter()
            .enumerate()
            .map(move |(defined_id, defined)| {
                (
                    // defined index is shifted by imports count
                    Temp::from_defined(defined_id),
                    defined,
                )
            })
    }

    /// Returns iterator over all entities, both imported and defined.
    /// The returned iterator yields pairs of (compound index, entity reference).
    /// Entity reference is wrapped in `ImportOrDefined` enum.
    pub fn iter(&self) -> impl Iterator<Item = (Temp<Ref>, ImportOrDefined<&Import, &Defined>)> {
        let imports = self
            .imports_iter()
            .map(|(id, import)| (id, ImportOrDefined::Import(import)));
        let defined = self
            .defined_iter()
            .map(|(id, defined)| (id, ImportOrDefined::Defined(defined)));
        imports.chain(defined)
    }

    /// Returns imported entity by index, if index is in imports range.
    /// For import by import index use `imports` field directly.
    pub fn get_import(&self, idx: Temp<Ref>) -> Option<&Import> {
        let import_idx = idx.as_import()?;
        Some(&self.imports[import_idx.index()])
    }

    /// Returns defined entity by index, if index is in defined range.
    /// For import by defined index use `defined` field directly.
    pub fn get_defined(&self, idx: Temp<Ref>) -> Option<&Defined> {
        let defined_idx = idx.as_defined()?;
        Some(&self.defined[defined_idx.index()])
    }

    /// Returns either imported or defined entity by compound index.
    pub fn get_entity(&self, idx: Temp<Ref>) -> ImportOrDefined<&Import, &Defined> {
        if idx.as_bits() & Temp::<Ref>::DEFINED_FLAG == 0 {
            let import_id = idx.as_bits() as usize;
            ImportOrDefined::Import(&self.imports[import_id])
        } else {
            let defined_id = (idx.as_bits() & !Temp::<Ref>::DEFINED_FLAG) as usize;
            ImportOrDefined::Defined(&self.defined[defined_id])
        }
    }

    /// Pushes new imported entity and returns its compound index.
    /// This is differ from `push_defined`, since later index is "shifted" by imports count.
    pub fn push_import(&mut self, import: impl Into<Import>) -> Temp<Ref> {
        self.imports.push(import.into());
        Temp::from_import(self.imports.len() - 1)
    }

    /// Pushes new defined entity and returns its "defined" index.
    /// This defined index can be converted to compound by calling `get_compound_index`.
    pub fn push_defined(&mut self, defined: impl Into<Defined>) -> Temp<Ref> {
        self.defined.push(defined.into());
        Temp::from_defined(self.defined.len() - 1)
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
    pub fn extend_with_info(
        mut self,
        mut names: GappedMap<Ref, NonDefault<Cow<'src, str>>>,
        exports: Vec<ExportEntry<'src, Ref>>,
    ) -> Self
    where
        Import: WithExtraInfo<'src>,
        Defined: WithExtraInfo<'src>,
    {
        for exports in &exports {
            let no_name = names.get(exports.entity_index).is_none();
            let is_defined = self.get_entity(exports.entity_index).to_defined().is_some();
            // if name is not present in names map
            if no_name && is_defined {
                // insert it into names map
                names.insert(exports.entity_index, NonDefault::from(exports.name.clone()));
            }
            self.get_entity_mut(exports.entity_index)
                .export_as_mut()
                .add_export(exports.name.clone());
        }

        for (r, name) in names.iter() {
            self.get_entity_mut(r).set_name(name.clone().into_inner());
        }

        self
    }
    pub fn iter_all_ids(&self) -> impl ExactSizeIterator<Item = Ref> {
        (0..(self.imports.len() + self.defined.len())).map(EntityRef::new)
    }
    pub fn len(&self) -> usize {
        self.imports.len() + self.defined.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn add_exports(&mut self, entity: Ref, export_name: Cow<'src, str>)
    where
        Import: WithExtraInfo<'src>,
        Defined: WithExtraInfo<'src>,
    {
        self.get_entity_mut(entity)
            .export_as_mut()
            .add_export(export_name);
    }

    pub fn exports_iter(&self) -> impl Iterator<Item = (Ref, Cow<'src, str>)>
    where
        Import: WithExtraInfo<'src>,
        Defined: WithExtraInfo<'src>,
    {
        self.iter().flat_map(|(r, entity)| {
            let iter = entity.export_as().names.clone().into_iter();

            iter.map(move |export_name| (r, export_name))
        })
    }
    // Convert temporary index to stable index.
    pub fn stable_id(&self, idx: Temp<Ref>) -> Ref {
        idx.to_stable(self.imports.len())
    }
    /// Returns iterator over imported entities.
    /// The returned iterator yields pairs of (compound index, import reference).
    pub fn imports_iter(&self) -> impl ExactSizeIterator<Item = (Ref, &Import)> {
        self.imports
            .iter()
            .enumerate()
            // import index is same as compound
            .map(|(import_id, import)| (Ref::new(import_id), import))
    }

    /// Returns iterator over defined entities.
    /// The returned iterator yields pairs of (compound index, defined reference).
    pub fn defined_iter(&self) -> impl ExactSizeIterator<Item = (Ref, &Defined)> {
        self.defined
            .iter()
            .enumerate()
            .map(move |(defined_id, defined)| {
                (
                    // defined index is shifted by imports count
                    Ref::new(defined_id + self.imports.len()),
                    defined,
                )
            })
    }
    pub fn defined_iter_mut(&mut self) -> impl ExactSizeIterator<Item = (Ref, &mut Defined)> {
        let imports_len = self.imports.len();
        self.defined
            .iter_mut()
            .enumerate()
            .map(move |(defined_id, defined)| {
                (
                    // defined index is shifted by imports count
                    Ref::new(defined_id + imports_len),
                    defined,
                )
            })
    }

    /// Returns iterator over all entities, both imported and defined.
    /// The returned iterator yields pairs of (compound index, entity reference).
    /// Entity reference is wrapped in `ImportOrDefined` enum.
    pub fn iter(&self) -> impl Iterator<Item = (Ref, ImportOrDefined<&Import, &Defined>)> {
        let imports = self
            .imports_iter()
            .map(|(id, import)| (id, ImportOrDefined::Import(import)));
        let defined = self
            .defined_iter()
            .map(|(id, defined)| (id, ImportOrDefined::Defined(defined)));
        imports.chain(defined)
    }
    /// Returns either imported or defined entity by index.
    pub fn get_entity(&self, stable_index: Ref) -> ImportOrDefined<&Import, &Defined> {
        self.try_get_entity(stable_index)
            .expect("Index out of bounds")
    }
    pub fn get_entity_mut(
        &mut self,
        stable_index: Ref,
    ) -> ImportOrDefined<&mut Import, &mut Defined> {
        self.try_get_entity_mut(stable_index)
            .expect("Index out of bounds")
    }
    pub fn try_get_entity_mut(
        &mut self,
        stable_index: Ref,
    ) -> Option<ImportOrDefined<&mut Import, &mut Defined>> {
        let num_imports = self.imports.len();
        if stable_index.index() < num_imports {
            Some(ImportOrDefined::Import(
                &mut self.imports[stable_index.index()],
            ))
        } else {
            let defined_index = stable_index.index() - num_imports;
            if defined_index < self.defined.len() {
                Some(ImportOrDefined::Defined(&mut self.defined[defined_index]))
            } else {
                None
            }
        }
    }
    /// Returns entity by index, or `None` if index is out of bounds.
    pub fn try_get_entity(&self, stable_index: Ref) -> Option<ImportOrDefined<&Import, &Defined>> {
        let num_imports = self.imports.len();
        if stable_index.index() < num_imports {
            Some(ImportOrDefined::Import(&self.imports[stable_index.index()]))
        } else {
            let defined_index = stable_index.index() - num_imports;
            if defined_index < self.defined.len() {
                Some(ImportOrDefined::Defined(&self.defined[defined_index]))
            } else {
                None
            }
        }
    }
}

/// Either imported or defined entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub enum ImportOrDefined<Import, Defined> {
    Import(Import),
    Defined(Defined),
}

impl<Import, Defined> ImportOrDefined<Import, Defined> {
    pub fn to_defined(self) -> Option<Defined> {
        match self {
            ImportOrDefined::Defined(d) => Some(d),
            _ => None,
        }
    }
    pub fn to_imported(self) -> Option<Import> {
        match self {
            ImportOrDefined::Import(i) => Some(i),
            _ => None,
        }
    }
}
impl<Import, Defined> ImportOrDefined<&Import, &Defined> {
    pub fn cloned(&self) -> ImportOrDefined<Import, Defined>
    where
        Import: Clone,
        Defined: Clone,
    {
        match *self {
            ImportOrDefined::Import(i) => ImportOrDefined::Import(i.clone()),
            ImportOrDefined::Defined(d) => ImportOrDefined::Defined(d.clone()),
        }
    }
}

trait WithType {
    type Type;
    fn get_type(&self) -> &Self::Type;
}
impl<T> WithType for DefinedEntity<'_, T> {
    type Type = T;

    fn get_type(&self) -> &Self::Type {
        &self.entity_type
    }
}

impl<T> WithType for WithoutBody<'_, T> {
    type Type = T;

    fn get_type(&self) -> &Self::Type {
        &self.entity_type
    }
}

impl<'any, ImportInner, D> ImportOrDefined<&'any ImportedEntity<'_, ImportInner>, &'any D>
where
    D: WithType<Type = ImportInner>,
{
    pub fn get_type(&self) -> &'any ImportInner {
        match self {
            ImportOrDefined::Import(import) => &import.entity_type,
            ImportOrDefined::Defined(defined) => defined.get_type(),
        }
    }
}

impl<'src, Import, Defined> ImportOrDefined<&Import, &Defined>
where
    Import: WithExtraInfo<'src>,
    Defined: WithExtraInfo<'src>,
{
    pub fn export_as(&self) -> &ExportNames<'src> {
        match self {
            ImportOrDefined::Import(import) => import.export_as(),
            ImportOrDefined::Defined(defined) => defined.export_as(),
        }
    }
    pub fn name(&self) -> Option<&Cow<'src, str>> {
        match self {
            ImportOrDefined::Import(import) => import.name(),
            ImportOrDefined::Defined(defined) => defined.name(),
        }
    }
}

impl<'src, Import, Defined> ImportOrDefined<&mut Import, &mut Defined>
where
    Import: WithExtraInfo<'src>,
    Defined: WithExtraInfo<'src>,
{
    pub fn export_as_mut(&mut self) -> &mut ExportNames<'src> {
        match self {
            ImportOrDefined::Import(import) => import.export_as_mut(),
            ImportOrDefined::Defined(defined) => defined.export_as_mut(),
        }
    }
    pub fn set_name(&mut self, name: Cow<'src, str>) {
        match self {
            ImportOrDefined::Import(import) => import.set_name(name),
            ImportOrDefined::Defined(defined) => defined.set_name(name),
        }
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
