//! This module provide functionality to define object that can be indexed in various ways - aka entity.
//!
//! It includes reexported `cranelift_entity`` types with additional functionality.
//!
//!  - trait `EntityRef` - any type-safe index for collection.
//!  - `PrimaryMap<K, V>` - map from `EntityRef` to some data. Think of it like `Vec<V>` where index is typed `K`.
//!  - trait `PrimaryKey` - allows defining default index type for some data type.
//!  - `IdVec<T>` - wrapper around `PrimaryMap` that allows only types with defined `PrimaryKey`,
//! think of it like `PrimaryMap<_, T>`` where `K` is inferred automatically.
//!  - `SecondaryMap<K, V>` - map from `EntityRef` to some data, but with default value in case if some entry wasn't initialized.
//!  - `GappedMap<K, V>` - wrapper around `SecondaryMap` that handles gaps in the map using trait `ReservedValue`
//! that mark some defined state of `V` as invalid.
//!
//! macro `impl_entity_index!` - allows defining new `EntityRef` types with minimal boilerplate.
//!

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
    ( $(
        $(#[display = $display:literal])?
        $visability:vis struct $entity:ident $(($( $type:tt)*))?
    );* $(;)? ) => {$(

        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        $visability struct $entity(u32);
        impl_entity_index!(@entity $entity $(, $display)?);
        impl From<u32> for $entity {
            fn from(value: u32) -> Self {
                $entity(value)
            }
        }
        // Default is reserved_value (U32::max) for easier debugging
        impl Default for $entity {
            fn default() -> Self {
                $crate::index::ReservedValue::reserved_value()
            }
        }
        // Compatibility methods for migration from Id<T>
        impl $entity {
            #[allow(dead_code, reason = "macro-generated code")]
            #[inline]
            pub fn next(&self) -> Self {
                let next = Self::from_u32(self.0 + 1);
                debug_assert!(next != $crate::index::ReservedValue::reserved_value());
                next
            }
        }
        // Impl primary key if needed
        $(
            impl_entity_index!(@primary_key $entity $($type)*);
        )?
    )*};
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

    (@entity $entity:ident, $display:literal) => {
        cranelift_entity::entity_impl!($entity, $display);
    };
    (@entity $entity:ident) => {
        cranelift_entity::entity_impl!($entity);
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
        pub struct First;
        #[display = "SecondWithDisplay"]
        pub struct SecondWithDisplay;
        #[display = "WithPrimary"]
        pub struct WithPrimary(SectionId);
        pub struct WithPrimaryLf(for <'lf> Test<'lf>);
    }
}

impl_entity_index! {
    #[display = "tag"]
    pub struct TagId(TagType);
    #[display = "type"]
    pub struct FuncTypeId(FuncType);
    #[display = "memory"]
    pub struct MemoryId(MemoryType);
    #[display = "import"]
    pub struct ImportId(for<'a> Import<'a>);
    #[display = "export"]
    pub struct ExportId(for<'a> Export<'a>);
    #[display = "table"]
    pub struct TableId(for<'a> Table<'a>);
    #[display = "global"]
    pub struct InputGlobalId(for<'a> Global<'a>);
    #[display = "element"]
    pub struct ElementId(for<'a> Element<'a>);
    #[display = "data"]
    pub struct DataSegmentId(for<'a> Data<'a>);
    #[display = ""] // Basic symbol no need prefix for display
    pub struct SymbolId(for<'a> SymbolRecord<'a>);
    #[display = "segment"]
    pub struct BuilderSegmentId(for<'a> SegmentLayout<'a>);
    #[display = "func"]
    pub struct InputFuncId(for<'a> InputFunction<'a>);
    #[display = "defined_func"]
    pub struct DefinedFuncId(for<'a> FunctionWithBody<'a>);

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
