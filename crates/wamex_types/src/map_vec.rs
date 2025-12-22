use std::collections::{BTreeMap, BTreeSet};


/// Map optimized for small number of entries.
///
/// Allows fast lookup but in compromise of slower inserts and removes.
/// Based on Vec with binary search.
///
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MiniMap<K, V> {
    entries: smallvec::SmallVec<[(K, V); 8]>,
}

impl<K: Ord, V> MiniMap<K, V> {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.entries
            .binary_search_by(Self::compare_with(key))
            .ok()
            .map(|pos| &self.entries[pos].1)
    }
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.entries
            .binary_search_by(Self::compare_with(key))
            .ok()
            .map(move |pos| &mut self.entries[pos].1)
    }
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        match self.entries.binary_search_by(Self::compare_with(&key)) {
            Ok(pos) => {
                let old_value = std::mem::replace(&mut self.entries[pos].1, value);
                Some(old_value)
            }
            Err(pos) => {
                self.entries.insert(pos, (key, value));
                None
            }
        }
    }
    pub fn last(&self) -> Option<&(K, V)> {
        self.entries.last()
    }
    pub fn take_last(&mut self) -> Option<(K, V)> {
        self.entries.pop()
    }

    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if let Ok(pos) = self.entries.binary_search_by(Self::compare_with(&key)) {
            let (_, value) = self.entries.remove(pos);
            Some(value)
        } else {
            None
        }
    }
    pub fn entry(&mut self, key: K) -> Entry<'_, K, V> {
        match self.entries.binary_search_by(Self::compare_with(&key)) {
            Ok(pos) => Entry::Occupied(&mut self.entries[pos].1),
            Err(pos) => Entry::Vacant(VacantEntry {
                map: self,
                index: pos,
                key,
            }),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn iter(&self) -> impl Iterator<Item = &(K, V)> {
        self.entries.iter()
    }
    pub fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(&K, &V) -> bool,
    {
        self.entries.retain(|(k, v)| f(k, v));
    }

    fn compare_with<Q>(key: &Q) -> impl FnMut(&(K, V)) -> std::cmp::Ordering
    where
        K: std::borrow::Borrow<Q>,
        Q: Ord + ?Sized,
    {
        move |entry: &(K, V)| entry.0.borrow().cmp(key)
    }

    fn compare(a: &(K, V), b: &(K, V)) -> std::cmp::Ordering {
        a.0.cmp(&b.0)
    }
    fn dedup(a: &mut (K, V), b: &mut (K, V)) -> bool {
        matches!(Self::compare(a, b), std::cmp::Ordering::Equal)
    }
    // Optimized extend that adds all items and then resorts and dedups
    // Useful if initial collection is small and we want to add many items at once
    #[doc(hidden)]
    pub fn extend_and_resort(&mut self, iter: impl IntoIterator<Item = (K, V)>) {
        self.entries.extend(iter);
        self.entries.sort_unstable_by(Self::compare);
        self.entries.dedup_by(Self::dedup);
    }
}

impl<K, V> Default for MiniMap<K, V> {
    fn default() -> Self {
        Self {
            entries: smallvec::SmallVec::new(),
        }
    }
}

impl<K, V> FromIterator<(K, V)> for MiniMap<K, V>
where
    K: Ord + Copy,
{
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut map = MiniMap::new();
        map.extend_and_resort(iter);
        map
    }
}

impl<'a, K, V> IntoIterator for &'a MiniMap<K, V> {
    type Item = &'a (K, V);
    type IntoIter = std::slice::Iter<'a, (K, V)>;
    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}
impl<K, V> IntoIterator for MiniMap<K, V> {
    type Item = (K, V);
    type IntoIter = smallvec::IntoIter<[(K, V); 8]>;
    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<K, V> Extend<(K, V)> for MiniMap<K, V>
where
    K: Ord,
    V: Copy,
{
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iter: I) {
        for (key, value) in iter {
            self.insert(key, value);
        }
    }
}

impl<K, V> PartialEq<BTreeMap<K, V>> for MiniMap<K, V>
where
    K: Ord,
    V: PartialEq,
{
    fn eq(&self, other: &BTreeMap<K, V>) -> bool {
        if self.len() != other.len() {
            return false;
        }
        for item in self.iter() {
            let Some(other_value) = other.get(&item.0) else {
                return false;
            };
            if other_value != &item.1 {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MiniSet<T> {
    map: MiniMap<T, ()>,
}
impl<T> MiniSet<T>
where
    T: Ord,
{
    pub fn new() -> Self {
        Self::default()
    }
    pub fn contains(&self, node: &T) -> bool {
        self.map.get(node).is_some()
    }
    pub fn insert(&mut self, node: T) -> bool {
        self.map.insert(node, ()).is_none()
    }
    pub fn remove(&mut self, node: &T) -> bool {
        self.map.remove(node).is_some()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.map.iter().map(|(k, _)| k)
    }
    pub fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(&T) -> bool,
    {
        self.map.retain(|k, _| f(k));
    }
    #[doc(hidden)]
    pub fn extend_and_resort(&mut self, iter: impl IntoIterator<Item = T>) {
        self.map
            .extend_and_resort(iter.into_iter().map(|k| (k, ())));
    }
}

impl<T> Default for MiniSet<T> {
    fn default() -> Self {
        Self {
            map: MiniMap::default(),
        }
    }
}

impl<T> FromIterator<T> for MiniSet<T>
where
    T: Ord + Copy,
{
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut list = MiniSet::new();
        list.extend(iter);
        list
    }
}

impl<'a, T> IntoIterator for &'a MiniSet<T> {
    type Item = &'a T;
    type IntoIter = std::iter::Map<std::slice::Iter<'a, (T, ())>, fn(&'a (T, ())) -> &'a T>;
    fn into_iter(self) -> Self::IntoIter {
        (&self.map).into_iter().map(|(k, _)| k)
    }
}
impl<T> IntoIterator for MiniSet<T> {
    type Item = T;
    type IntoIter = std::iter::Map<smallvec::IntoIter<[(T, ()); 8]>, fn((T, ())) -> T>;
    fn into_iter(self) -> Self::IntoIter {
        self.map.into_iter().map(|(k, _)| k)
    }
}

impl<T> Extend<T> for MiniSet<T>
where
    T: Ord + Copy,
{
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for item in iter {
            self.insert(item);
        }
    }
}

impl<T> PartialEq<BTreeSet<T>> for MiniSet<T>
where
    T: Ord,
{
    fn eq(&self, other: &BTreeSet<T>) -> bool {
        if self.len() != other.len() {
            return false;
        }
        for item in self.iter() {
            if !other.contains(item) {
                return false;
            }
        }
        true
    }
}

pub struct VacantEntry<'a, K, V> {
    map: &'a mut MiniMap<K, V>,
    index: usize,
    key: K,
}

pub enum Entry<'a, K, V> {
    Vacant(VacantEntry<'a, K, V>),
    Occupied(&'a mut V),
}

impl<'a, K, V> Entry<'a, K, V>
where
    K: Ord,
{
    pub fn or_insert(self, value: V) -> &'a mut V {
        match self {
            Entry::Occupied(v) => v,
            Entry::Vacant(vacant) => {
                vacant.map.entries.insert(vacant.index, (vacant.key, value));
                &mut vacant.map.entries[vacant.index].1
            }
        }
    }
    pub fn or_insert_with<F: FnOnce() -> V>(self, func: F) -> &'a mut V {
        match self {
            Entry::Occupied(v) => v,
            Entry::Vacant(vacant) => {
                let value = func();
                vacant.map.entries.insert(vacant.index, (vacant.key, value));
                &mut vacant.map.entries[vacant.index].1
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::MiniMap;
    #[test]
    fn test_minimap_basic() {
        let mut map = MiniMap::new();
        assert!(map.is_empty());
        assert_eq!(map.len(), 0);

        assert_eq!(map.get(&1), None);
        assert_eq!(map.insert(1, "one"), None);
        assert_eq!(map.get(&1), Some(&"one"));
        assert_eq!(map.len(), 1);
        assert!(!map.is_empty());

        assert_eq!(map.insert(1, "uno"), Some("one"));
        assert_eq!(map.get(&1), Some(&"uno"));
        assert_eq!(map.len(), 1);

        assert_eq!(map.insert(2, "two"), None);
        assert_eq!(map.get(&2), Some(&"two"));
        assert_eq!(map.len(), 2);

        assert_eq!(map.remove(&1), Some("uno"));
        assert_eq!(map.get(&1), None);
        assert_eq!(map.len(), 1);

        assert_eq!(map.remove(&3), None);
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn test_entry_api() {
        let mut map = MiniMap::new();

        map.insert(1, "one");
        map.insert(3, "three");

        assert_eq!(map.entry(2).or_insert("two"), &"two");
        assert_eq!(map.entry(1).or_insert("uno"), &"one");
    }
}
