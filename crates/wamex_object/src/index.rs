//! This module provide functionality to define object that can be indexed in various ways - aka entity.
//!
//! It includes reexported `cranelift_entity`` types with additional functionality.
//!
//!  - trait `EntityRef` - any type-safe index for collection.
//!  - `PrimaryMap<K, V>` - map from `EntityRef` to some data. Think of it like `Vec<V>` where index is typed `K`.
//!  - `SecondaryMap<K, V>` - map from `EntityRef` to some data, but with default value in case if some entry wasn't initialized.
//!  - `GappedMap<K, V>` - wrapper around `SecondaryMap` that handles gaps in the map using trait `ReservedValue`
//!    that mark some defined state of `V` as invalid.
//!
//! macro `impl_entity_index!` - allows defining new `EntityRef` types with minimal boilerplate.
//!

use std::{
    borrow::Cow,
    fmt::{Debug, Display},
    hash::Hash,
    ops::Deref,
};

use cranelift_entity::packed_option::PackedOption;
pub use cranelift_entity::{EntityRef, PrimaryMap, SecondaryMap, packed_option::ReservedValue};
use derive_more::Display;

macro_rules! impl_entity_index {
    ( $(
        $(#[display = $display:literal])?
        $(#[doc = $doc:literal])*
        $visability:vis struct $entity:ident
    );* $(;)? ) => {$(

        $(#[doc = $doc])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(transparent)]
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
        impl core::ops::Add<u32> for $entity {
            type Output = Self;

            fn add(self, rhs: u32) -> Self::Output {
                let next = Self::from_u32(self.0 + rhs);
                debug_assert!(next != $crate::index::ReservedValue::reserved_value());
                next
            }
        }
        impl core::ops::Sub<u32> for $entity {
            type Output = Self;

            fn sub(self, rhs: u32) -> Self::Output {
                debug_assert!(self.0 >= rhs);
                let next = Self::from_u32(self.0 - rhs);
                next
            }
        }
    )*};

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
            .next_back()
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
    pub fn get_mut(&mut self, key: K) -> Option<&mut V> {
        self.map[key].expand_mut()
    }
    pub fn iter(&self) -> impl Iterator<Item = (K, &V)> {
        self.map
            .iter()
            .filter_map(|(k, v)| v.expand_ref().map(|v| (k, v)))
    }
    pub fn last_key(&self) -> Option<K> {
        self.map.iter().rfind(|(_, v)| !v.is_none()).map(|(k, _)| k)
    }
    pub fn next_key(&self) -> K {
        self.last_key()
            .map(|k| K::new(k.index() + 1))
            .unwrap_or(K::new(0))
    }
    pub fn len(&self) -> usize {
        self.length
    }
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    pub fn entry(&mut self, key: K) -> GappedMapEntry<'_, V> {
        GappedMapEntry {
            reserved: &mut self.map[key],
        }
    }
    pub fn extend(&mut self, iter: impl IntoIterator<Item = (K, V)>) {
        for (k, v) in iter {
            self.insert(k, v);
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

// Test macro usage
#[cfg(debug_assertions)]
mod test_impl_entity_index {
    use std::marker::PhantomData;

    #[allow(dead_code, reason = "used for compile test only")]
    struct Test<'f> {
        _func: PhantomData<&'f ()>,
    }
    impl_entity_index! {
        pub struct First;
        #[display = "SecondWithDisplay"]
        pub struct SecondWithDisplay;
    }
}

pub type AnySymbolId = usize;
pub type SectionId = u32;

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
            Some(unsafe { std::mem::transmute::<&PackedOption<T>, &T>(self) })
        }
    }
    fn expand_mut(&mut self) -> Option<&mut T> {
        if self.is_none() {
            None
        } else {
            //SAFETY: cast ref to inner type of repr(transparent) type
            Some(unsafe { std::mem::transmute::<&mut PackedOption<T>, &mut T>(self) })
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

impl<'a> From<NonDefault<&'a str>> for NonDefault<Cow<'a, str>> {
    fn from(value: NonDefault<&'a str>) -> Self {
        NonDefault {
            value: Cow::Borrowed(value.into_inner()),
        }
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

impl<T: Display> Display for NonDefault<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.value, f)
    }
}
impl<T: Debug> Debug for NonDefault<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self.value, f)
    }
}
pub trait TempIndex: EntityRef {
    fn from_u32(value: u32) -> Self;
    fn as_u32(&self) -> u32;
}
/// Temporary index type that gives packed representation of import|defined index.
/// Used during building phase, when final indexes are not known, because some imports may shift defined entities.
///
/// It has three states:
/// - [1<value>] - defined entity.
/// - [00<value>] - imported entity.
/// - [01<value>] - external entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Display)]
#[display("{_0}")]
pub struct Temp<Idx>(u32, std::marker::PhantomData<Idx>);
impl<Idx: TempIndex> Temp<Idx> {
    pub const DEFINED_FLAG: u32 = 1 << 31;
    pub const EXTERNAL_FLAG: u32 = 1 << 30;
    pub const MAX_VALUE: u32 = Self::EXTERNAL_FLAG - 1;

    pub fn from_import(index: usize) -> Self {
        debug_assert!(index <= (Self::MAX_VALUE as usize));
        Self(index as u32, std::marker::PhantomData)
    }
    pub fn from_defined(index: usize) -> Self {
        debug_assert!(index <= (Self::MAX_VALUE as usize));
        Self(
            (index as u32) | Self::DEFINED_FLAG,
            std::marker::PhantomData,
        )
    }
    pub fn from_external(index: usize) -> Self {
        debug_assert!(index <= (Self::MAX_VALUE as usize));
        Self(
            (index as u32) | Self::EXTERNAL_FLAG,
            std::marker::PhantomData,
        )
    }

    ///
    /// # Safety
    /// Caller should ensure that value has valid import/defined entity,
    /// before converting `to_stable`.
    pub unsafe fn from_bits(value: u32) -> Self {
        Self(value, std::marker::PhantomData)
    }
    pub fn as_bits(&self) -> u32 {
        self.0
    }

    #[inline]
    pub fn as_import(&self) -> Option<Idx> {
        if (self.0 & Self::DEFINED_FLAG) == 0 && (self.0 & Self::EXTERNAL_FLAG) == 0 {
            Some(Idx::from_u32(self.0))
        } else {
            None
        }
    }

    #[inline]
    pub fn as_defined(&self) -> Option<u32> {
        if (self.0 & Self::DEFINED_FLAG) != 0 {
            Some(self.0 & !Self::DEFINED_FLAG)
        } else {
            None
        }
    }

    #[inline]
    pub fn to_stable(self, num_imports: usize, num_defined: usize) -> Idx {
        let tag = (self.0 & (Self::DEFINED_FLAG | Self::EXTERNAL_FLAG)) >> 30;
        match dbg!(tag) {
            0x0 => {
                // import
                let import_index = self.0 & Self::MAX_VALUE;
                Idx::from_u32(import_index)
            }
            0x2 => {
                // defined
                let defined_index = self.0 & Self::MAX_VALUE;
                Idx::from_u32(num_imports as u32 + defined_index)
            }
            0x1 => {
                // extern
                let extern_index = self.0 & Self::MAX_VALUE;
                Idx::from_u32(num_imports as u32 + num_defined as u32 + extern_index)
            }
            _ => unreachable!(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WithStart<Idx, Val> {
    start: Idx,
    value: Vec<Val>,
}

impl<Idx, Val> WithStart<Idx, Val> {
    pub fn new(start: Idx, value: Vec<Val>) -> Self {
        Self { start, value }
    }
    #[allow(
        clippy::should_implement_trait,
        reason = "Trait impl is harder because cranelift doesn't expose IntoIter type."
    )]
    #[inline]
    pub fn into_iter(self) -> impl Iterator<Item = (Idx, Val)>
    where
        Idx: EntityRef,
    {
        self.value
            .into_iter()
            .enumerate()
            .map(move |(i, v)| (Idx::new(self.start.index() + i), v))
    }
    pub fn iter(&self) -> impl Iterator<Item = (Idx, &Val)>
    where
        Idx: EntityRef,
    {
        self.value
            .iter()
            .enumerate()
            .map(move |(i, v)| (Idx::new(self.start.index() + i), v))
    }
    pub fn get(&self, idx: Idx) -> Option<&Val>
    where
        Idx: EntityRef,
    {
        let offset = idx.index().checked_sub(self.start.index())?;
        self.value.get(offset)
    }

    pub fn as_mut_slice(&mut self) -> &mut [Val] {
        &mut self.value
    }
    pub fn as_slice(&self) -> &[Val] {
        &self.value
    }
    pub fn len(&self) -> usize {
        self.value.len()
    }
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use cranelift_entity::EntityRef;

    use crate::{
        index::{GappedMap, Temp},
        layouts::DataSymbolRef,
        typed::SymbolId,
    };

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

    #[test]
    fn test_gapped_map() {
        // push at 0
        // push at 2
        // push at 3
        // push at 4
        // push at 5
        // remove (3,5)

        let val = SymbolId::from_u32(0);
        let mut gapped_map = GappedMap::new();

        assert!(gapped_map.last_key().is_none());

        gapped_map.insert(val, val);
        assert_eq!(gapped_map.last_key().unwrap(), val);

        for key in 2..=5 {
            let key = SymbolId::from_u32(key);
            gapped_map.insert(key, val);
            assert_eq!(gapped_map.last_key().unwrap(), key);
        }
        for key in [3, 5] {
            let key = SymbolId::from_u32(key);
            gapped_map.remove(key);
        }

        assert_eq!(gapped_map.last_key().unwrap(), SymbolId::from_u32(4));

        let res: Vec<_> = gapped_map.iter().map(|(k, v)| (k, *v)).collect();

        let expected = vec![
            (SymbolId::from_u32(0), val),
            (SymbolId::from_u32(2), val),
            (SymbolId::from_u32(4), val),
        ];

        assert_eq!(res, expected)
    }

    #[test]
    fn test_temp_index() {
        let imports = 0..10;
        let defined = 0..5;
        let extern_ref = 0..3;

        for i in imports.clone() {
            let temp = Temp::<DataSymbolRef>::from_import(i);
            assert_eq!(temp.as_import().unwrap(), DataSymbolRef::new(i));
            assert!(temp.as_defined().is_none());
            assert_eq!(
                temp.to_stable(imports.len(), defined.len()),
                DataSymbolRef::new(i)
            );
        }

        for i in defined.clone() {
            let temp = Temp::<DataSymbolRef>::from_defined(i);
            let expected_ref = DataSymbolRef::new(imports.len() + i);
            assert_eq!(temp.as_defined().unwrap(), i as u32);
            assert!(temp.as_import().is_none());
            assert_eq!(temp.to_stable(imports.len(), defined.len()), expected_ref);
        }
        for i in extern_ref.clone() {
            let temp = Temp::<DataSymbolRef>::from_external(i);
            let expected_ref = DataSymbolRef::new(imports.len() + defined.len() + i);
            assert!(temp.as_import().is_none());
            assert!(temp.as_defined().is_none());
            assert_eq!(temp.to_stable(imports.len(), defined.len()), expected_ref);
        }
    }
}
