use std::{
    fmt::Debug,
    hash::Hash,
    ops::{Deref, DerefMut},
};

use cranelift_entity::packed_option::PackedOption;
pub use cranelift_entity::{EntityRef, PrimaryMap, SecondaryMap, packed_option::ReservedValue};
use wasmparser::{Data, Element, Export, FuncType, Global, Import, MemoryType, Table, TagType};

use crate::{
    emit::SegmentLayout,
    read::code::{FunctionWithBody, InputFunction},
    symbols::SymbolRecord,
};

macro_rules! impl_entity_index {
    ( $( $ty:ident $(($( $type:tt)*))? $( => $display:literal)? );* $(;)? ) => {
        $(

            #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
            pub struct $ty(u32);
            impl_entity_index!(@entity $ty $(, $display)?);
            impl From<u32> for $ty {
                fn from(value: u32) -> Self {
                    $ty(value)
                }
            }
            // Default is reserved_value (U32::max) for easier debugging
            impl Default for $ty {
                fn default() -> Self {
                    $crate::index::ReservedValue::reserved_value()
                }
            }
            // Compatibility methods for migration from Id<T>
            impl $ty {
                pub fn as_raw_index(&self) -> usize {
                    cranelift_entity::EntityRef::index(*self)
                }

                pub fn from_index<T>(id: T) -> Self
                where
                    T: TryInto<u32> + std::fmt::Debug + Copy,
                {
                    $ty::from_u32(id.try_into().ok().unwrap_or_else(|| panic!("Invalid ID: {:?}", id)))
                }
                pub fn next(&self) -> Self {
                    debug_assert!(self.0 != u32::MAX);
                    $ty::from_u32(self.0 + 1)
                }
            }
            $(
                impl_entity_index!(@primary_key $ty $($type)*);
            )?
        )*
    };
    (@primary_key $entity:ident $type: ident) => {
        impl $crate::index::PrimaryKey for $type {
            type EntityType = $entity;
        }
    };
    (@primary_key $entity:ident for<$b: lifetime> $type: ty) => {
        impl<$b> $crate::index::PrimaryKey for $type {
            type EntityType = $entity;
        }
    };

    (@entity $ty:ident, $display:literal) => {
        cranelift_entity::entity_impl!($ty, $display);
    };
    (@entity $ty:ident) => {
        cranelift_entity::entity_impl!($ty);
    };
}

// A wrapper around `SecondaryMap` that handles small amount of gaps.
// And track length of valid items.
//
// Usefull for maps where some items are not set, but unlike `cranelift_entity::SparseMap`
// the amount of gaps, is small, therefore no need to store array of keys separately.
// As a penalty for that, iterating over all items is more expensive, and memory is reserved for invalid items.
#[derive(Clone, PartialEq, Eq, Hash, Default, Debug)]
pub struct GappedMap<K: EntityRef, V: ReservedValue + Clone> {
    map: SecondaryMap<K, PackedOption<V>>,
    // tracked length of inserted non-default items
    length: usize,
}

impl<K: EntityRef, V> GappedMap<K, V>
where
    V: Clone + ReservedValue,
{
    pub fn new() -> Self {
        GappedMap {
            map: SecondaryMap::with_default(PackedOption::default()),
            length: 0,
        }
    }
    /// Insert value, returning previous value if any.
    ///
    /// Not expecting default value to be inserted.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        debug_assert!(!value.is_reserved_value(), "Cannot insert default value");
        let prev = self.map.get(key).filter(|v| !v.is_none()).cloned();
        self.map[key] = value.into();

        if prev.is_none() {
            self.length += 1;
        }
        prev.and_then(PackedOption::expand)
    }
    pub fn push(&mut self, value: V) -> K {
        let last = self
            .map
            .iter()
            .rev()
            .next()
            .map(|(k, _)| K::new(k.index() + 1))
            .unwrap_or(K::new(0));
        self.insert(last, value);
        last
    }

    pub fn remove(&mut self, key: K) -> Option<V> {
        let prev = self.map.get(key).filter(|v| !v.is_none()).cloned();
        self.map[key] = PackedOption::default();
        if prev.is_some() {
            self.length -= 1;
        }
        prev.and_then(PackedOption::expand)
    }
    pub fn get(&self, key: K) -> Option<&V> {
        self.map[key].expand_ref()
    }
    pub fn iter(&self) -> impl Iterator<Item = (K, &V)> {
        self.map
            .iter()
            .filter_map(|(k, v)| v.expand_ref().map(|v| (k, v)))
    }
    pub fn len(&self) -> usize {
        self.length
    }
    pub fn entry(&mut self, key: K) -> IdMapEntry<'_, V> {
        IdMapEntry {
            reserved: &mut self.map[key],
        }
    }
}

/// Reference to an entry in `IdMap`
pub struct IdMapEntry<'a, V: ReservedValue + Clone> {
    reserved: &'a mut PackedOption<V>,
}

impl<'a, V: ReservedValue + Clone> IdMapEntry<'a, V> {
    pub fn or_insert(self, value: V) -> &'a mut V {
        self.or_insert_with(|| value)
    }
    pub fn or_insert_with<F: FnOnce() -> V>(self, f: F) -> &'a mut V {
        if self.reserved.is_none() {
            let value = f();
            debug_assert!(!value.is_reserved_value(), "Cannot insert reserved value");
            *self.reserved = value.into();
        }
        self.reserved.expand_mut().unwrap()
    }
}

impl<K, V> FromIterator<(K, V)> for GappedMap<K, V>
where
    K: EntityRef,
    V: Clone + ReservedValue,
{
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut map = GappedMap::new();
        for (k, v) in iter {
            map.insert(k, v);
        }
        map
    }
}

///
/// Allows creating `IdVec` of some entity type with default index type.
///
pub trait PrimaryKey {
    type EntityType: EntityRef;
}

// A wrapper around `PrimaryMap` that allows only entities with defined `PrimaryKey`.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct IdVec<T: PrimaryKey>(PrimaryMap<T::EntityType, T>);
impl<T: PrimaryKey> IdVec<T> {
    pub fn new() -> Self {
        IdVec(PrimaryMap::new())
    }
}

impl<T> Deref for IdVec<T>
where
    T: PrimaryKey,
{
    type Target = PrimaryMap<T::EntityType, T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl<T> DerefMut for IdVec<T>
where
    T: PrimaryKey,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T: PrimaryKey> Default for IdVec<T> {
    fn default() -> Self {
        IdVec(PrimaryMap::new())
    }
}

impl<T: PrimaryKey> FromIterator<T> for IdVec<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut map = PrimaryMap::new();
        for item in iter {
            map.push(item);
        }
        IdVec(map)
    }
}

impl<T: PrimaryKey + Debug> Debug for IdVec<T>
where
    T::EntityType: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

// Test macro usage
#[cfg(debug_assertions)]
mod test_impl_entity_index {
    use std::marker::PhantomData;

    use super::SectionId;

    struct Test<'f> {
        _func: PhantomData<&'f ()>,
    }
    impl_entity_index! {
        First;
        SecondWithDisplay => "SecondWithDisplay";
        WithPrimary(SectionId) => "WithPrimary";
        WithPrimaryLf(for <'lf> Test<'lf>);
    }
}

impl_entity_index! {
    TagId(TagType) => "tag";
    FuncTypeId(FuncType) => "type";
    MemoryId(MemoryType) => "memory";
    ImportId(for<'a> Import<'a>) => "import";
    ExportId(for<'a> Export<'a>) => "export";
    TableId(for<'a> Table<'a>) => "table";
    InputGlobalId(for<'a> Global<'a>) => "global";
    ElementId(for<'a> Element<'a>) => "element";
    DataSegmentId(for<'a> Data<'a>) => "data";
    SymbolId(for<'a> SymbolRecord<'a>) => ""; // Basic symbol no need prefix for display
    BuilderSegmentId(for<'a> SegmentLayout<'a>) => "segment";
    InputFuncId(for<'a> InputFunction<'a>) => "func";
    DefinedFuncId(for<'a> FunctionWithBody<'a>) => "defined_func";
}

pub type AnySymbolId = usize;
pub type SectionId = usize;

// TODO: Maybe replace Vecs with id_arena?
// Currently the only difference is that we also use

/// Store additional information about section, to apply relocation
#[derive(Debug)]
pub struct IndexedSection<T> {
    pub starting_offset: usize,
    pub section_index: usize,
    pub section_payload: T,
}

impl<T> Deref for IndexedSection<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.section_payload
    }
}
impl<T: Default> Default for IndexedSection<T> {
    fn default() -> Self {
        Self {
            section_payload: T::default(),
            starting_offset: 0,
            section_index: usize::MAX,
        }
    }
}

// TODO: replace with macro_metavar_expr_concat
// Currently need explicitly define private type for each index
#[macro_export]
macro_rules! impl_standalone_index {
    ( $($ty:ident($priv:ident)),* ) => {
        $(
            pub enum $priv {}
            pub type $ty = $crate::index::Id<$priv>;
            impl $crate::index::Indexed for $priv {
                type StaticTypeTagForIndex = $priv;
                type IndexType = Id<$priv>;
            }
        )*
    };
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

trait PackedOptionExt<T> {
    fn expand_ref(&self) -> Option<&T>;
    fn expand_mut(&mut self) -> Option<&mut T>;
}

impl<T: ReservedValue> PackedOptionExt<T> for PackedOption<T> {
    fn expand_ref(&self) -> Option<&T> {
        if self.is_none() {
            None
        } else {
            //SAFETY: cast ref to inner type of repr(transparent) type
            Some(unsafe { std::mem::transmute(self) })
        }
    }
    fn expand_mut(&mut self) -> Option<&mut T> {
        if self.is_none() {
            None
        } else {
            //SAFETY: cast ref to inner type of repr(transparent) type
            Some(unsafe { std::mem::transmute(self) })
        }
    }
}

#[cfg(test)]
mod tests {

    struct WithReserved(u32);
    impl crate::index::ReservedValue for WithReserved {
        fn is_reserved_value(&self) -> bool {
            self.0 == u32::MAX
        }
        fn reserved_value() -> Self {
            WithReserved(u32::MAX)
        }
    }
    #[test]
    fn test_packed_ext() {
        use cranelift_entity::packed_option::PackedOption;

        use crate::index::PackedOptionExt;

        let v: PackedOption<WithReserved> = WithReserved(10).into();
        assert!(v.expand_ref().is_some());
        let v: PackedOption<WithReserved> = PackedOption::default();
        assert!(v.expand_ref().is_none());
    }
}
