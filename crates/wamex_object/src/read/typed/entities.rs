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
//! unlike in `walrus` this information represented as structure of arrays
//!

use std::{borrow::Cow, fmt::Debug};

use cranelift_entity::{EntityRef, packed_option::ReservedValue};

use super::imports::ExportEntry;
use crate::index::{GappedMap, IdVec, NonDefault, PrimaryKey};

pub type Functions<'src> = EntitiesCollection<'src, FunctionRef>;
pub type Tables<'src> = EntitiesCollection<'src, TableRef>;
pub type Memories<'src> = EntitiesCollection<'src, MemoryRef>;
pub type Globals<'src> = EntitiesCollection<'src, GlobalRef>;
pub type Tags<'src> = EntitiesCollection<'src, TagRef>;

pub struct EntitiesCollection<'src, IDX: CompoundRef> {
    // Both defined and imports entities.
    pub items: CompoundList<'src, IDX>,

    /// Names for entities, if present.
    /// Name information is gathered from name section,
    /// if no name is present in name section, name is retrieved from export entry.
    pub names: GappedMap<IDX, NonDefault<&'src str>>,

    /// List of exported entries.
    // Because exports array are usually small, no need to store them as `IdMap`
    pub exports: Vec<ExportEntry<'src, IDX>>,
}

impl<'src, IDX> EntitiesCollection<'src, IDX>
where
    IDX: CompoundRef,
{
    pub fn new(
        declared: CompoundList<'src, IDX>,
        mut names: GappedMap<IDX, NonDefault<&'src str>>,
        exports: Vec<ExportEntry<'src, IDX>>,
    ) -> Self {
        for exports in &exports {
            // if name is not present in names map
            if names.get(exports.entity_index).is_none() {
                // and if it can be borrowed from export entry
                if let Cow::Borrowed(name) = exports.name {
                    // insert it into names map
                    names.insert(exports.entity_index, NonDefault::from(name));
                }
            }
        }

        Self {
            items: declared,
            names,
            exports,
        }
    }
    pub fn defined_iter(&self) -> impl Iterator<Item = (IDX, &IDX::DefinedType<'src>)> {
        self.items.defined_iter()
    }
    pub fn imports_iter(&self) -> impl Iterator<Item = (IDX, &IDX::ImportType<'src>)> {
        self.items.imports_iter()
    }
    pub fn iter(
        &self,
    ) -> impl Iterator<
        Item = (
            IDX,
            ImportOrDefined<&IDX::ImportType<'src>, &IDX::DefinedType<'src>>,
        ),
    > {
        self.items.iter().map(|(id, import_or_defined)| {
            let import_or_defined = match import_or_defined {
                ImportOrDefined::Import(import) => ImportOrDefined::Import(import),
                ImportOrDefined::Defined(defined) => ImportOrDefined::Defined(defined),
            };
            (id, import_or_defined)
        })
    }
    pub fn iter_all_ids(&self) -> impl ExactSizeIterator<Item = IDX> {
        (0..(self.items.imports.len() + self.items.defined.len())).map(EntityRef::new)
    }
}
/// Compound reference to both imported and defined entity types.
pub trait CompoundRef: EntityRef {
    type ImportType<'src>: PrimaryKey;
    type DefinedType<'src>: PrimaryKey;
}

/// One place for storing imports and defined entities.
///
#[derive(PartialEq, Eq, Clone)]
pub struct CompoundList<'src, IDX: CompoundRef> {
    pub imports: IdVec<IDX::ImportType<'src>>,
    pub defined: IdVec<IDX::DefinedType<'src>>,
}

impl<'src, IDX: CompoundRef> CompoundList<'src, IDX> {
    pub fn new(
        imports: IdVec<IDX::ImportType<'src>>,
        defined: IdVec<IDX::DefinedType<'src>>,
    ) -> Self {
        Self { imports, defined }
    }

    pub fn imports_slice(&self) -> &[IDX::ImportType<'src>] {
        self.imports.as_values_slice()
    }
    pub fn defined_slice(&self) -> &[IDX::DefinedType<'src>] {
        self.defined.as_values_slice()
    }

    /// Pushes new imported entity and returns its compound index.
    /// This is differ from `push_defined`, since later index is "shifted" by imports count.
    pub fn push_import(&mut self, import: IDX::ImportType<'src>) -> IDX {
        self.imports.push(import);
        IDX::new(self.imports.len() - 1)
    }

    /// Returns iterator over imported entities.
    /// The returned iterator yields pairs of (compound index, import reference).
    pub fn imports_iter(&self) -> impl ExactSizeIterator<Item = (IDX, &IDX::ImportType<'src>)> {
        self.imports
            .iter()
            // import index is same as compound
            .map(|(import_id, import)| (IDX::new(import_id.index()), import))
    }
    /// Returns iterator over defined entities.
    /// The returned iterator yields pairs of (compound index, defined reference).
    pub fn defined_iter(&self) -> impl ExactSizeIterator<Item = (IDX, &IDX::DefinedType<'src>)> {
        let num_imports = self.imports.len();
        self.defined.iter().map(move |(defined_id, defined)| {
            (
                // defined index is shifted by imports count
                IDX::new(num_imports + defined_id.index()),
                defined,
            )
        })
    }

    /// Returns iterator over all entities, both imported and defined.
    /// The returned iterator yields pairs of (compound index, entity reference).
    /// Entity reference is wrapped in `ImportOrDefined` enum.
    pub fn iter(
        &self,
    ) -> impl Iterator<
        Item = (
            IDX,
            ImportOrDefined<&IDX::ImportType<'src>, &IDX::DefinedType<'src>>,
        ),
    > {
        let imports = self
            .imports_iter()
            .map(|(id, import)| (id, ImportOrDefined::Import(import)));
        let defined = self
            .defined_iter()
            .map(|(id, defined)| (id, ImportOrDefined::Defined(defined)));
        imports.chain(defined)
    }

    /// Pushes new defined entity and returns its "defined" index.
    /// This defined index can be converted to compound by calling `get_compound_index`.
    pub fn push_defined(
        &mut self,
        defined: IDX::DefinedType<'src>,
    ) -> <IDX::DefinedType<'src> as PrimaryKey>::EntityType {
        self.defined.push(defined);
        <IDX::DefinedType<'src> as PrimaryKey>::EntityType::new(self.defined.len() - 1)
    }

    /// Returns compound index for defined entity.
    /// Make sure that imports is not pushed after this call,
    /// otherwise the index of this defined item will be shifted, and need to be recalculated.
    pub fn calculate_compound_index(
        &self,
        defined_idx: <IDX::DefinedType<'src> as PrimaryKey>::EntityType,
    ) -> IDX {
        IDX::new(self.imports.len() + defined_idx.index())
    }
    /// Returns imported entity by index, if index is in imports range.
    /// For import by import index use `imports` field directly.
    pub fn get_import(&self, idx: IDX) -> Option<&IDX::ImportType<'src>> {
        let raw_idx = idx.index();
        if raw_idx < self.imports.len() {
            Some(&self.imports[EntityRef::new(raw_idx)])
        } else {
            None
        }
    }

    /// Returns defined entity by index, if index is in defined range.
    /// For import by defined index use `defined` field directly.
    pub fn get_defined(&self, idx: IDX) -> Option<&IDX::DefinedType<'src>> {
        let raw_idx = idx.index();
        if raw_idx >= self.imports.len() {
            Some(&self.defined[EntityRef::new(raw_idx - self.imports.len())])
        } else {
            None
        }
    }

    /// Returns either imported or defined entity by compound index.
    pub fn get_entity(
        &self,
        idx: IDX,
    ) -> ImportOrDefined<&IDX::ImportType<'src>, &IDX::DefinedType<'src>> {
        let raw_idx = idx.index();
        if raw_idx < self.imports.len() {
            let import_id = EntityRef::new(raw_idx);
            ImportOrDefined::Import(&self.imports[import_id])
        } else {
            let defined_id = EntityRef::new(raw_idx - self.imports.len());
            ImportOrDefined::Defined(&self.defined[defined_id])
        }
    }
}

/// Either imported or defined entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub enum ImportOrDefined<Import, Defined> {
    Import(Import),
    Defined(Defined),
}

impl_entity_index! {
    #[display = "func"]
    pub struct FunctionRef;
    #[display = "table"]
    pub struct TableRef;
    #[display = "memory"]
    pub struct MemoryRef;
    #[display = "global"]
    pub struct GlobalRef;
    #[display = "tag"]
    pub struct TagRef;
}

impl CompoundRef for FunctionRef {
    type ImportType<'src> = super::imports::ImportedFunction<'src>;
    type DefinedType<'src> = crate::read::raw::FunctionWithBody<'src>;
}
impl CompoundRef for TableRef {
    type ImportType<'src> = super::imports::ImportedTable<'src>;
    type DefinedType<'src> = crate::read::raw::Table<'src>;
}
impl CompoundRef for MemoryRef {
    type ImportType<'src> = super::imports::ImportedMemory<'src>;
    type DefinedType<'src> = crate::read::raw::MemoryType;
}
impl CompoundRef for GlobalRef {
    type ImportType<'src> = super::imports::ImportedGlobal<'src>;
    type DefinedType<'src> = crate::read::raw::Global<'src>;
}
impl CompoundRef for TagRef {
    type ImportType<'src> = super::imports::ImportedTag<'src>;
    type DefinedType<'src> = crate::read::raw::TagType;
}

//TODO: Move Output->input mapping to separate module?

/// A type that can provide index of corresponding entity in input module.
pub trait GetInputRef<InputRef> {
    // TODO: Module ID?
    /// Returns index of corresponding entity in input module.
    fn get_input_index(&self) -> OutputMapType<InputRef>;
}

/// A reference that possibly linked to another entity in input module.
pub trait LinkedToInputRef: ReservedValue {
    type InputRef: EntityRef;
}

/// Implementation of `GetInputRef` for `ImportOrDefined`.
impl<Imported, Defined, InputRef> GetInputRef<InputRef> for ImportOrDefined<Imported, Defined>
where
    Imported: GetInputRef<InputRef>,
    Defined: GetInputRef<InputRef>,
{
    fn get_input_index(&self) -> OutputMapType<InputRef> {
        match self {
            ImportOrDefined::Import(import) => import.get_input_index(),
            ImportOrDefined::Defined(defined) => defined.get_input_index(),
        }
    }
}

impl<'a, T, InputRef> GetInputRef<InputRef> for &'a T
where
    T: GetInputRef<InputRef>,
{
    fn get_input_index(&self) -> OutputMapType<InputRef> {
        (*self).get_input_index()
    }
}

/// Collection of entities used in build of output module, with information about corresponding entity in input module.
pub struct EntitiesFromInput<'src, IDX>
where
    IDX: LinkedToInputRef + CompoundRef,
{
    entities: CompoundList<'src, IDX>,
    /// Map from input entity to coresponding output entity in `entities` collection.
    from_input: GappedMap<IDX::InputRef, IDX>,
}

impl<'src, IDX> EntitiesFromInput<'src, IDX>
where
    IDX: LinkedToInputRef + CompoundRef,
    IDX::ImportType<'src>: GetInputRef<IDX::InputRef>,
    IDX::DefinedType<'src>: GetInputRef<IDX::InputRef>,
{
    pub fn new(entities: CompoundList<'src, IDX>) -> Self {
        let from_input = entities
            .iter()
            .filter_map(|(output_id, v)| {
                v.get_input_index()
                    .into_bidirectional()
                    .map(|input_id| (input_id, output_id))
            })
            .collect();
        Self {
            entities,
            from_input,
        }
    }

    pub fn get_output_id(&self, input_id: IDX::InputRef) -> Option<IDX> {
        self.from_input.get(input_id).copied()
    }
    pub fn get_input_id(&self, output_id: IDX) -> Option<IDX::InputRef> {
        let v = self.entities.get_entity(output_id);
        v.get_input_index().has_input()
    }

    pub fn imports(&self) -> impl ExactSizeIterator<Item = (IDX, &IDX::ImportType<'src>)> {
        self.entities.imports_iter()
    }
    pub fn defined(&self) -> impl ExactSizeIterator<Item = (IDX, &IDX::DefinedType<'src>)> {
        self.entities.defined_iter()
    }

    pub fn get_import_for_output_id(&self, output_id: IDX) -> Option<&IDX::ImportType<'src>> {
        let raw_id = output_id.index();
        self.entities.imports.get(EntityRef::new(raw_id))
    }

    pub fn get_defined_for_output_id(&self, output_id: IDX) -> Option<&IDX::DefinedType<'src>> {
        let raw_id = output_id.index();
        self.entities.defined.get(EntityRef::new(raw_id))
    }

    pub fn iter_all_ids(&self) -> impl ExactSizeIterator<Item = IDX> {
        (0..self.len()).map(EntityRef::new)
    }
    pub fn len(&self) -> usize {
        self.entities.imports.len() + self.entities.defined.len()
    }
}

pub enum OutputMapType<Input> {
    /// Each output type can be mapped to an input type and vice versa.
    BidirectionalMap(Input),
    /// Only Output -> Input mapping is guaranteed.
    OutputHasInput(Input),
    // The output is a new type that has no corresponding input.
    None,
}

impl<Input> OutputMapType<Input> {
    pub fn bidirectional_from_option(input: Option<Input>) -> Self {
        match input {
            Some(input) => OutputMapType::BidirectionalMap(input),
            None => OutputMapType::None,
        }
    }
    pub fn into_bidirectional(self) -> Option<Input> {
        match self {
            OutputMapType::BidirectionalMap(input) => Some(input),
            _ => None,
        }
    }
    pub fn has_input(self) -> Option<Input> {
        match self {
            OutputMapType::BidirectionalMap(input) | OutputMapType::OutputHasInput(input) => {
                Some(input)
            }
            OutputMapType::None => None,
        }
    }
}
impl<'src, IDX> Debug for CompoundList<'src, IDX>
where
    IDX: CompoundRef,
    IDX::ImportType<'src>: Debug,
    <IDX::ImportType<'src> as PrimaryKey>::EntityType: Debug,
    IDX::DefinedType<'src>: Debug,
    <IDX::DefinedType<'src> as PrimaryKey>::EntityType: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntitiesCollection")
            .field("imports", &self.imports)
            .field("defined", &self.defined)
            .finish()
    }
}

impl<'src, IDX> Debug for EntitiesCollection<'src, IDX>
where
    IDX: CompoundRef + Debug,
    IDX::ImportType<'src>: Debug,
    <IDX::ImportType<'src> as PrimaryKey>::EntityType: Debug,
    IDX::DefinedType<'src>: Debug,
    <IDX::DefinedType<'src> as PrimaryKey>::EntityType: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntitiesCollection")
            .field("items", &self.items)
            .field("names", &self.names)
            .field("exports", &self.exports)
            .finish()
    }
}
