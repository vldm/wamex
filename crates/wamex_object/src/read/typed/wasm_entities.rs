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

use std::fmt::Debug;

use cranelift_entity::{EntityRef, SecondaryMap, packed_option::ReservedValue};

use super::imports::ExportEntry;
use crate::index::{GappedMap, IdVec, PrimaryKey};

pub struct EntitiesCollection<'src, IDX: CompoundRef> {
    // Both defined and imports entities.
    pub declared: CompoundList<'src, IDX>,

    /// Names for entities, if present.
    pub names: SecondaryMap<IDX, &'src str>,
    /// List of exported entries.
    // Because exports array are usually small, no need to store them as `IdMap`
    pub exports: Vec<ExportEntry<'src, IDX>>,
}

/// Compound reference to both imported and defined entity types.
pub trait CompoundRef: EntityRef {
    type ImportType<'src>: PrimaryKey;
    type DefinedType<'src>: PrimaryKey;
}

/// One place for storing imports and defined entities.
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

    /// Pushes new defined entity and returns its "defined" index.
    /// This defined index can be converted to compound by calling `get_compound_index`.
    pub fn push_defined(
        &mut self,
        defined: IDX::DefinedType<'src>,
    ) -> <IDX::DefinedType<'src> as PrimaryKey>::EntityType {
        self.defined.push(defined);
        <IDX::DefinedType<'src> as PrimaryKey>::EntityType::new(self.defined.len() - 1)
    }

    /// Returnts compound index for defined entity.
    pub fn get_compound_index(
        &self,
        defined_idx: <IDX::DefinedType<'src> as PrimaryKey>::EntityType,
    ) -> IDX {
        IDX::new(self.imports.len() + defined_idx.index())
    }
}

impl_entity_index! {
    pub struct InputFuncId;
}

impl CompoundRef for InputFuncId {
    type ImportType<'src> = super::imports::ImportedFunction<'src>;
    type DefinedType<'src> = crate::read::raw::FunctionWithBody<'src>;
}

//TODO: Move Output->input mapping to separate module?

/// A reference that possibly linked to another entity in input module.
pub trait LinkedToInput: CompoundRef {
    type InputEntity: PrimaryKey;
    /// Returns index of corresponding entity in input module.
    fn get_input_index(&self) -> OutputMapType<<Self::InputEntity as PrimaryKey>::EntityType>;
}

/// Collection of entities used in output module, with information about corresponding entity in input module.
pub struct EntitiesFromInput<'src, IDX>
where
    IDX: LinkedToInput + ReservedValue,
{
    entities: CompoundList<'src, IDX>,
    /// Map from input entity to coresponding output entity in `entities` collection.
    map: GappedMap<<IDX::InputEntity as PrimaryKey>::EntityType, IDX>,
}

// Type that maybe defined or imported.
// In WASM a lot of objects can be either defined or imported,
// and all imports are stored before defined, so index for defined is always shifted by imports count.
// Currently Defined or Import can be: function, global, table, memory, ..etc.
pub trait Defined<'src>: PrimaryKey {
    type Import: PrimaryKey;
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

pub trait OutputType<'src> {
    type InputType: PrimaryKey + 'src;

    // to use InputType::IndexType we need rtn
    fn get_input_index(&self) -> OutputMapType<<Self::InputType as PrimaryKey>::EntityType>;
}

/// One place for storing imports and defined items,
/// so we can use
#[derive(Debug)]
pub struct ImportsOrDefined<'src, D: Defined<'src>> {
    pub imports: Vec<D::Import>,
    pub defined: Vec<D>,
}

impl<'src, D: Defined<'src>> ImportsOrDefined<'src, D> {
    pub fn new(imports: Vec<D::Import>, defined: Vec<D>) -> Self {
        Self { imports, defined }
    }
    pub fn imports(&self) -> &[D::Import] {
        &self.imports
    }
    pub fn defined(&self) -> &[D] {
        &self.defined
    }

    pub fn push_import(&mut self, import: D::Import) -> D::EntityType {
        self.imports.push(import);
        EntityRef::new(self.imports.len() - 1)
    }

    /// After locking, no modification is allowed.
    #[allow(private_bounds)]
    pub fn lock(self) -> WithOriginalIndex<'src, D>
    where
        D: OutputType<'src>,
        D::Import: OutputType<'src, InputType = D::InputType>,
        <D as PrimaryKey>::EntityType: ReservedValue,
    {
        WithOriginalIndex::new(self)
    }
}

/// After building this collection, no modification is allowed.
pub struct WithOriginalIndex<'src, T>
where
    T: OutputType<'src> + Defined<'src>,
    <T as PrimaryKey>::EntityType: ReservedValue,
{
    collection: ImportsOrDefined<'src, T>,
    map: GappedMap<<T::InputType as PrimaryKey>::EntityType, <T as PrimaryKey>::EntityType>,
}

impl<'src, T: Debug> Debug for WithOriginalIndex<'src, T>
where
    T: OutputType<'src> + Defined<'src>,
    T::Import: Debug,
    <T as PrimaryKey>::EntityType: ReservedValue,
    // TODO: better clause
    GappedMap<<T::InputType as PrimaryKey>::EntityType, <T as PrimaryKey>::EntityType>: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WithOriginalIndex")
            .field("collection", &self.collection)
            .field("map", &self.map)
            .finish()
    }
}

#[allow(private_bounds)]
impl<'src, T> WithOriginalIndex<'src, T>
where
    T: OutputType<'src> + Defined<'src>,
    T::Import: OutputType<'src, InputType = T::InputType>,
    <T as PrimaryKey>::EntityType: ReservedValue,
{
    pub fn new(collection: ImportsOrDefined<'src, T>) -> Self {
        let imports = collection.imports().iter().map(OutputType::get_input_index);
        let defined = collection.defined().iter().map(OutputType::get_input_index);
        let map = imports
            .chain(defined)
            .enumerate()
            .filter_map(|(i, input_id)| {
                input_id
                    .into_bidirectional()
                    .map(|input_id| (input_id, EntityRef::new(i)))
            })
            .collect();
        WithOriginalIndex { collection, map }
    }

    pub fn get_output_id(
        &self,
        input_id: <T::InputType as PrimaryKey>::EntityType,
    ) -> Option<<T as PrimaryKey>::EntityType> {
        self.map.get(input_id).cloned()
    }
    pub fn get_input_id(
        &self,
        output_id: <T as PrimaryKey>::EntityType,
    ) -> Option<<T::InputType as PrimaryKey>::EntityType> {
        let raw_output_id = output_id.index();
        if raw_output_id < self.collection.imports().len() {
            // If output_id is less than imports count, then it is import
            self.collection
                .imports()
                .get(raw_output_id)
                .and_then(|v| v.get_input_index().has_input())
        } else {
            // Otherwise it is defined
            let defined_index = raw_output_id - self.collection.imports().len();
            self.collection
                .defined()
                .get(defined_index)
                .and_then(|v| v.get_input_index().has_input())
        }
    }

    pub fn imports(
        &self,
    ) -> impl ExactSizeIterator<Item = (<T as PrimaryKey>::EntityType, &T::Import)> {
        self.collection
            .imports()
            .iter()
            .enumerate()
            .map(|(id, import)| (<T as PrimaryKey>::EntityType::new(id), import))
    }
    pub fn defined(&self) -> impl ExactSizeIterator<Item = (<T as PrimaryKey>::EntityType, &T)> {
        let num_imports = self.collection.imports().len();
        self.collection
            .defined()
            .iter()
            .enumerate()
            .map(move |(id, defined)| {
                (
                    <T as PrimaryKey>::EntityType::new(id + num_imports),
                    defined,
                )
            })
    }

    pub fn get_import_for_output_id(
        &self,
        output_id: <T as PrimaryKey>::EntityType,
    ) -> Option<&T::Import> {
        let raw_output_id = output_id.index();
        if raw_output_id < self.collection.imports().len() {
            self.collection.imports().get(raw_output_id)
        } else {
            None
        }
    }

    pub fn get_defined_for_output_id(
        &self,
        output_id: <T as PrimaryKey>::EntityType,
    ) -> Option<&T> {
        let raw_output_id = output_id.index();
        if raw_output_id > self.collection.imports().len() {
            self.collection
                .defined()
                .get(raw_output_id - self.collection.imports().len())
        } else {
            None
        }
    }

    pub fn iter_all_ids(&self) -> impl Iterator<Item = <T as PrimaryKey>::EntityType> {
        (0..self.len()).map(<T as PrimaryKey>::EntityType::new)
    }
    pub fn len(&self) -> usize {
        self.collection.imports().len() + self.collection.defined().len()
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
