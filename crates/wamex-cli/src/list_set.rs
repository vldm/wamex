// TODO: optimize inserts in dep graph building
/// List of dependency, optimized for small number of entries.
/// Allows fast lookup but in compromise of slower inserts and removes.
/// Based on Vec with binary search.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MiniSet<T> {
    nodes: smallvec::SmallVec<[T; 8]>,
}
impl<T> MiniSet<T>
where
    T: Ord,
{
    pub fn new() -> Self {
        Self::default()
    }
    pub fn contains(&self, node: &T) -> bool {
        self.nodes.binary_search(node).is_ok()
    }
    pub fn insert(&mut self, node: T) -> bool {
        match self.nodes.binary_search(&node) {
            Ok(_) => false, // already exists
            Err(pos) => {
                self.nodes.insert(pos, node);
                true
            }
        }
    }
    pub fn remove(&mut self, node: &T) -> bool {
        if let Ok(pos) = self.nodes.binary_search(node) {
            self.nodes.remove(pos);
            true
        } else {
            false
        }
    }
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
    pub fn len(&self) -> usize {
        self.nodes.len()
    }
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.nodes.iter()
    }
    // Optimized extend that adds all items and then resorts and dedups
    // Usefull if initial collection is small and we want to add many items at once
    pub(crate) fn extend_and_resort(&mut self, iter: impl IntoIterator<Item = T>) {
        self.nodes.extend(iter);
        self.nodes.sort_unstable();
        self.nodes.dedup();
    }
}

impl<T> Default for MiniSet<T> {
    fn default() -> Self {
        Self {
            nodes: smallvec::SmallVec::new(),
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
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.nodes.iter()
    }
}
impl<T> IntoIterator for MiniSet<T> {
    type Item = T;
    type IntoIter = smallvec::IntoIter<[T; 8]>;
    fn into_iter(self) -> Self::IntoIter {
        self.nodes.into_iter()
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
