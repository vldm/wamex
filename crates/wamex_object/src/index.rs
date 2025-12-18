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
    fmt::{Debug, Display},
    hash::Hash,
    ops::{Deref, DerefMut},
};

use cranelift_entity::packed_option::PackedOption;
pub use cranelift_entity::{EntityRef, PrimaryMap, SecondaryMap, packed_option::ReservedValue};

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
//
// Also API is slightly different - instead of using `Index` trait to access items, it uses `get` and `entry` methods.
// This is because `Index` trait does not allow returning `Option<&V>`, and `PackedOption<V>` is not very user-friendly.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
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
        GappedMap::default()
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

    pub fn entry(&mut self, key: K) -> GappedMapEntry<'_, V> {
        GappedMapEntry {
            reserved: &mut self.map[key],
        }
    }
}

impl<K: EntityRef, V: ReservedValue + Clone> std::ops::Index<K> for GappedMap<K, V> {
    type Output = PackedOption<V>;

    fn index(&self, index: K) -> &Self::Output {
        &self.map[index]
    }
}
impl<K: EntityRef, V: ReservedValue + Clone> std::ops::IndexMut<K> for GappedMap<K, V> {
    fn index_mut(&mut self, index: K) -> &mut Self::Output {
        &mut self.map[index]
    }
}

/// Reference to an entry in `IdMap`
pub struct GappedMapEntry<'a, V: ReservedValue + Clone> {
    reserved: &'a mut PackedOption<V>,
}

impl<'a, V: ReservedValue + Clone> GappedMapEntry<'a, V> {
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
impl<K: EntityRef, V: Clone + ReservedValue> Default for GappedMap<K, V> {
    fn default() -> Self {
        GappedMap {
            map: SecondaryMap::with_default(PackedOption::default()),
            length: 0,
        }
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

    #[allow(dead_code, reason = "used for compile test only")]
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

/// Wrapper of `<T>` that mark default value as reserved.
/// and ensure that `NonDefault<T>` cannot be created from `T::default()`
#[derive(Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
#[repr(transparent)]
pub struct NonDefault<T> {
    value: T,
}
impl<T> NonDefault<T> {
    pub fn into_inner(self) -> T {
        self.value
    }
}

impl<T: Default + Eq> ReservedValue for NonDefault<T> {
    fn reserved_value() -> Self {
        NonDefault {
            value: T::default(),
        }
    }

    fn is_reserved_value(&self) -> bool {
        self.value == T::default()
    }
}

impl<T: Default + Eq> From<T> for NonDefault<T> {
    fn from(value: T) -> Self {
        debug_assert!(
            value != T::default(),
            "Cannot create NonDefault with default value"
        );
        NonDefault { value }
    }
}

impl<T> Deref for NonDefault<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}
impl<T> PartialEq<T> for NonDefault<T>
where
    T: PartialEq,
{
    fn eq(&self, other: &T) -> bool {
        &self.value == other
    }
}

impl<T: Default + Eq> Default for NonDefault<T> {
    fn default() -> Self {
        Self::reserved_value()
    }
}

impl Display for NonDefault<&str> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.value, f)
    }
}
impl Debug for NonDefault<&str> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self.value, f)
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
