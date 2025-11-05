//!
//! Diff implemented in two phases:
//! 1) Mapping symbols from left to right module by their names. (Remaining symbols are treated as added or removed)
//! 2) Calculate canonical hash + (child_ids) of function bodies for mapped functions and compare them to detect modified functions.
//!
//! Note: .L symbols is not guarateed to persist between compilations. So to detect this symbols we may use "contexts" (parent symbols).

use std::{
    collections::{BTreeMap, BTreeSet},
    mem,
};

use crate::{
    analysis::{self},
    index::{IdMap, SymbolId},
    Diff,
};

struct SymbolMapping {
    // Most of symbols are mapped.
    left_to_right: IdMap<SymbolId, SymbolId>,
    left_non_matched: Vec<SymbolId>,
    right_non_matched: Vec<SymbolId>,
}

impl SymbolMapping {
    pub fn mapped(&self) -> impl Iterator<Item = (SymbolId, SymbolId)> + use<'_> {
        self.left_to_right
            .iter()
            .map(|(right, left)| (right, *left))
    }
    pub fn left_only(&self) -> impl Iterator<Item = SymbolId> + use<'_> {
        self.left_non_matched.iter().copied()
    }
    pub fn right_only(&self) -> impl Iterator<Item = SymbolId> + use<'_> {
        self.right_non_matched.iter().copied()
    }
}

type SVec<T, const SIZE: usize = 4> = smallvec::SmallVec<[T; SIZE]>;

pub struct Differ<'any, 'one, 'another> {
    left: &'any analysis::ModuleInfo<'one>,
    right: &'any analysis::ModuleInfo<'another>,
}

impl<'any, 'one, 'another> Differ<'any, 'one, 'another> {
    pub fn new(
        left: &'any analysis::ModuleInfo<'one>,
        right: &'any analysis::ModuleInfo<'another>,
    ) -> Self {
        Self { left, right }
    }

    pub fn diff(&self) -> DiffResult {
        let mut mapping = self.build_name_mapping();
        self.refine_mapping(&mut mapping);
        self.build_diff(&mapping)
    }

    fn build_name_mapping(&self) -> SymbolMapping {
        //1. Build name -> symbol id maps for first_module;
        // If more than one symbol with same name found - we treat them as duplicates and compare by context later.
        let mut name_to_left_symbol: BTreeMap<&str, SymbolId> = BTreeMap::new();
        let mut duplicate_left: BTreeMap<&str, SVec<SymbolId>> = BTreeMap::new();
        for (sym_id, symbol) in self.left.symbols.iter() {
            if let Some(name) = &symbol.linking_name {
                if let Some(prev) = name_to_left_symbol.insert(name, sym_id) {
                    duplicate_left.entry(name).or_default().push(prev);
                }
            }
        }

        //1.2. Duplicate are handled separately - remove them from main map.
        let duplicate_names: SVec<_, 16> = duplicate_left.keys().cloned().collect();
        for name in duplicate_names {
            let last = name_to_left_symbol.remove(name).unwrap();
            let duplicates = duplicate_left.get_mut(name).unwrap();
            duplicates.push(last);
        }

        //2. Match left symbols to the right.
        let mut mapping = IdMap::new();

        let mut non_matched_right_symbols: Vec<SymbolId> = Vec::new();
        let mut dups: SVec<_, 16> = SVec::new();
        for (right_sym_id, right_symbol) in self.right.symbols.iter() {
            if let Some(name) = &right_symbol.linking_name {
                if let Some(&left_sym_id) = name_to_left_symbol.get(name) {
                    if let Some(dup) = mapping.insert(left_sym_id, right_sym_id) {
                        dups.push(left_sym_id);
                        non_matched_right_symbols.push(dup);
                    }
                    continue;
                }
            }
            non_matched_right_symbols.push(right_sym_id);
        }

        //2.2 handle duplicates
        for dup in dups {
            let right = mapping.remove(dup).unwrap();
            non_matched_right_symbols.push(right);
        }

        // TODO: optimize by itering over mapping keys.
        let non_matched_left_symbols: Vec<SymbolId> = self
            .left
            .symbols
            .iter()
            .filter_map(|(left_sym_id, _)| {
                if mapping.get(left_sym_id).is_none() {
                    Some(left_sym_id)
                } else {
                    None
                }
            })
            .collect();

        SymbolMapping {
            left_to_right: mapping,
            left_non_matched: non_matched_left_symbols,
            right_non_matched: non_matched_right_symbols,
        }
    }

    // Calculate hash and check if hashes and childs are same
    fn is_same_content(
        &self,
        mapping: &SymbolMapping,
        left_sym_id: SymbolId,
        right_sym_id: SymbolId,
    ) -> Result<(), ReplaceDetail> {
        let left_symbol = &self.left.symbols.get(left_sym_id).unwrap();
        let right_symbol = &self.right.symbols.get(right_sym_id).unwrap();

        let left_content = left_symbol.stable_content(&self.left);
        let right_content = right_symbol.stable_content(&self.right);
        if left_content != right_content {
            return Err(ReplaceDetail::BodyChanged);
        }

        // 2. check childs
        let right_childs = right_symbol.childs().collect::<Vec<_>>();

        // List of right childs mapped to left symbols
        let mut left_childs: Vec<SymbolId> = Vec::new();
        for left_sym in left_symbol.childs() {
            let Some(&right_sym_mapped) = mapping.left_to_right.get(left_sym) else {
                return Err(ReplaceDetail::UnresolvedChildren(left_sym));
            };
            left_childs.push(right_sym_mapped);
        }

        if left_childs.len() != right_childs.len() {
            return Err(ReplaceDetail::ChildrenChanged);
        }

        Ok(())
    }

    // Try to match non-mapped symbols.
    // If symbols have not changed, matching can still fail if name is not unique, or not changed.
    // To deal with this, we can add more pieces of information to the matching process:
    // 1. Use stable name (don't use .L names that can change between compilations)
    // 2. Use content and list of childs to match symbols that have not changed.
    // 3. If not working - use context (parent symbols) to find where symbol was used.
    fn refine_mapping(&self, mapping: &mut SymbolMapping) {
        // 1. Build parent map for left symbols
        let mut left_parent_map: BTreeMap<SymbolId, SymbolContext> = BTreeMap::new();
        for (sym_id, symbol) in self.left.symbols.iter() {
            for child in symbol.childs() {
                left_parent_map
                    .entry(child)
                    .or_default()
                    .parents
                    .push(sym_id);
            }
        }

        // 2. Build parent map for right symbols
        let mut right_parent_map: BTreeMap<SymbolId, SymbolContext> = BTreeMap::new();
        for (sym_id, symbol) in self.right.symbols.iter() {
            for child in symbol.childs() {
                right_parent_map
                    .entry(child)
                    .or_default()
                    .parents
                    .push(sym_id);
            }
        }

        // 3. Build candidates
        let mut right_candidates = BTreeMap::<_, SVec<_>>::new();
        for right_sym in std::mem::take(&mut mapping.right_non_matched) {
            let right_symbol = &self.right.symbols.get(right_sym).unwrap();
            let context = right_parent_map
                .get(&right_sym)
                .cloned()
                .unwrap_or_default();

            let key = SymbolKey {
                stable_name: right_symbol.stable_name(),
            };

            let entry = right_candidates.entry(key).or_default();
            entry.push(SymbolWithContext {
                symbol: right_sym,
                context,
            });
        }
        //3.1. For left candidates not all parents/childs can be mapped so process is iterative.

        let mut queue = std::mem::take(&mut mapping.left_non_matched);

        let mut queue_len = queue.len();
        loop {
            queue_len = queue.len();
            let mut left_candidates = BTreeMap::<_, SVec<_>>::new();
            for left_sym in std::mem::take(&mut queue) {
                // Try match candidate.
                let left_mapped_candidate_key = {
                    let left_symbol = &self.left.symbols.get(left_sym).unwrap();

                    SymbolKey {
                        stable_name: left_symbol.stable_name(),
                    }
                };

                let mut context = left_parent_map.get(&left_sym).cloned().unwrap_or_default();
                let parents: Option<SVec<_>> = std::mem::take(&mut context.parents)
                    .into_iter()
                    .map(|parent| mapping.left_to_right.get(parent).copied())
                    .collect();
                let Some(parents) = parents else {
                    // Some parents are not mapped yet - skip for now.
                    queue.push(left_sym);
                    continue;
                };
                context.parents = parents;

                left_candidates
                    .entry(left_mapped_candidate_key)
                    .or_default()
                    .push(SymbolWithContext {
                        symbol: left_sym,
                        context,
                    });
            }

            for (key, mut left_syms) in left_candidates {
                let Some(mut right_syms) = right_candidates.remove(&key) else {
                    // No candidates on right side - push all left symbols back to removed list.

                    for s in left_syms {
                        mapping.left_non_matched.push(s.symbol);
                    }
                    continue;
                };
                mapping.match_list_by_context(&mut left_syms, &mut right_syms);
                // Return unmatched right symbols back to the map.
                // and left back to the queue.
                if !right_syms.is_empty() {
                    assert!(right_candidates.insert(key, right_syms).is_none());
                }
                for s in left_syms {
                    queue.push(s.symbol);
                }
            }

            if queue.is_empty() || queue.len() == queue_len {
                break;
            }
        }

        // 4. Return unmatched symbols back to the mapping.
        for (_, right_syms) in right_candidates {
            for s in right_syms {
                mapping.right_non_matched.push(s.symbol);
            }
        }

        for left_sym in queue {
            mapping.left_non_matched.push(left_sym);
        }
    }

    fn build_diff(&self, mapping: &SymbolMapping) -> DiffResult {
        let mut diff_result = DiffResult::new();

        // Process mapped symbols
        for (left_sym_id, right_sym_id) in mapping.mapped() {
            match self.is_same_content(mapping, left_sym_id, right_sym_id) {
                Ok(()) => diff_result.push_same(left_sym_id, right_sym_id),
                Err(detail) => diff_result.push_replaced(left_sym_id, right_sym_id, detail),
            }
        }

        // Process non-matched left symbols (removed)
        for left_sym_id in mapping.left_only() {
            diff_result.push_removed(left_sym_id);
        }

        // Process non-matched right symbols (added)
        for right_sym_id in mapping.right_only() {
            diff_result.push_added(right_sym_id);
        }

        diff_result
    }

    fn left_sym_name(&self, sym_id: SymbolId) -> Option<&str> {
        self.left.symbols.get(sym_id).map(|s| &*s.name)
    }
    fn right_sym_name(&self, sym_id: SymbolId) -> Option<&str> {
        self.right.symbols.get(sym_id).map(|s| &*s.name)
    }
    pub fn debug_diff(&self, diff: &DiffResult) {
        let replaced_iter = diff.replaced();
        let added_iter = diff.added();
        let removed_iter = diff.removed();
        // Debug output
        println!(
            "Replaced: {}, Added: {}, Removed: {}, Same: {}",
            replaced_iter.clone().count(),
            added_iter.clone().count(),
            removed_iter.clone().count(),
            diff.same().count()
        );

        for entry in added_iter {
            let DiffEntry::Added { right } = entry else {
                continue;
            };
            let name = self.right_sym_name(*right).unwrap_or("<unknown>");
            println!("Added: {name} [{index}]", index = right);
        }
        for entry in removed_iter {
            let DiffEntry::Removed { left } = entry else {
                continue;
            };
            let name = self.left_sym_name(*left).unwrap_or("<unknown>");
            println!("Removed: {name} [{index}]", index = left);
        }
        for entry in replaced_iter {
            let DiffEntry::Replaced {
                left,
                right,
                detail,
            } = entry
            else {
                continue;
            };
            let left_name = self.left_sym_name(*left).unwrap_or("<unknown>");
            let right_name = self.right_sym_name(*right).unwrap_or("<unknown>");

            let detail = match detail {
                ReplaceDetail::BodyChanged => {
                    let left_content = self
                        .left
                        .symbols
                        .get(*left)
                        .unwrap()
                        .stable_content(&self.left)
                        .unwrap_or_default();
                    let right_content = self
                        .right
                        .symbols
                        .get(*right)
                        .unwrap()
                        .stable_content(&self.right)
                        .unwrap_or_default();
                    format_args!(
                        "Body changed from {left_content} to {right_content}",
                        left_content = hex::encode(left_content),
                        right_content = hex::encode(right_content)
                    )
                }
                ReplaceDetail::ChildrenChanged => {
                    // TODO: add list of deps that were changed.
                    format_args!("Children changed")
                }
                ReplaceDetail::UnresolvedChildren(v) => {
                    format_args!("Unresolved child symbol id: {v}", v = *v)
                }
            };
            println!(
                "Replaced: {left_name} [{left_index}] -> {right_name} [{right_index}] Detail: {detail}",
                left_index = left,
                right_index = right
            );
        }
    }
}

/// Result of diffing two structures
#[derive(Debug, Clone)]
pub struct DiffResult {
    entries: Vec<DiffEntry>,
}

impl FromIterator<DiffEntry> for DiffResult {
    fn from_iter<T: IntoIterator<Item = DiffEntry>>(iter: T) -> Self {
        let mut diff_result = DiffResult::new();
        for entry in iter {
            diff_result.push(entry);
        }
        diff_result
    }
}

impl DiffResult {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn push(&mut self, entry: DiffEntry) {
        self.entries.push(entry);
    }
    pub fn push_added(&mut self, right: SymbolId) {
        self.entries.push(DiffEntry::Added { right });
    }
    pub fn push_removed(&mut self, left: SymbolId) {
        self.entries.push(DiffEntry::Removed { left });
    }
    pub fn push_replaced(&mut self, left: SymbolId, right: SymbolId, detail: ReplaceDetail) {
        self.entries.push(DiffEntry::Replaced {
            left,
            right,
            detail,
        });
    }
    pub fn push_same(&mut self, left: SymbolId, right: SymbolId) {
        self.entries.push(DiffEntry::Same { left, right });
    }

    /// List of nodes that remain same.
    /// This nodes should have same signature, body hash and childs but may have different parent nodes.
    pub fn same(&self) -> impl Iterator<Item = &DiffEntry> + Clone {
        self.entries
            .iter()
            .filter(|entry| matches!(entry, DiffEntry::Same { .. }))
    }
    /// List of nodes that cannot be matched in old and new structures.
    /// It will contain full list of removed, added or replaced nodes.
    ///
    /// This can be false positive, if signature or content hash was changed.
    pub fn all_changes(&self) -> impl Iterator<Item = &DiffEntry> + Clone {
        self.entries.iter()
    }
    /// List of nodes that was changed, but filter only those that was matched in old and new structures.
    pub fn replaced(&self) -> impl Iterator<Item = &DiffEntry> + Clone {
        self.entries.iter().filter(|entry| entry.is_replaced())
    }
    /// List nodes that was added in new structure.
    pub fn added(&self) -> impl Iterator<Item = &DiffEntry> + Clone {
        self.entries.iter().filter(|entry| entry.is_added())
    }
    /// List nodes that was removed in new structure.
    pub fn removed(&self) -> impl Iterator<Item = &DiffEntry> + Clone {
        self.entries.iter().filter(|entry| entry.is_removed())
    }

    pub fn entries(&self) -> impl Iterator<Item = &DiffEntry> + Clone {
        self.entries.iter()
    }
}

#[derive(Debug, Clone, Copy)]
pub enum DiffEntry {
    Replaced {
        left: SymbolId,
        right: SymbolId,
        detail: ReplaceDetail,
    },
    Same {
        left: SymbolId,
        right: SymbolId,
    },
    Added {
        right: SymbolId,
    },
    Removed {
        left: SymbolId,
    },
}
impl DiffEntry {
    fn is_added(&self) -> bool {
        matches!(self, DiffEntry::Added { .. })
    }
    fn is_removed(&self) -> bool {
        matches!(self, DiffEntry::Removed { .. })
    }
    fn is_replaced(&self) -> bool {
        matches!(self, DiffEntry::Replaced { .. })
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ReplaceDetail {
    /// Content is not equal
    BodyChanged,
    /// Relocations symbols are different
    ChildrenChanged,
    /// Some of children cannot be mapped,
    /// and we cannot be sure if they are same or not.
    UnresolvedChildren(SymbolId),
}

// Fuzzy matching based on symbol content and context.

#[derive(Ord, PartialOrd, PartialEq, Eq)]
struct SymbolKey<'a> {
    stable_name: Option<&'a str>,
    // childs: SVec<SymbolId>,
    // content: Option<Vec<u8>>,
}

#[derive(Default, Debug, Clone, Ord, PartialOrd, PartialEq, Eq)]
struct SymbolContext {
    parents: SVec<SymbolId>,
}

impl SymbolContext {
    fn num_same_parents(&self, other: &SymbolContext) -> usize {
        let mut same = 0;
        for parent in &self.parents {
            if other.parents.contains(parent) {
                same += 1;
            }
        }
        same
    }
}

#[derive(Debug, Clone, Ord, PartialOrd, PartialEq, Eq)]
struct SymbolWithContext {
    pub symbol: SymbolId,
    pub context: SymbolContext,
}

impl SymbolMapping {
    fn match_list_by_context(
        &mut self,
        old_contexts: &mut SVec<SymbolWithContext>,
        new_contexts: &mut SVec<SymbolWithContext>,
    ) {
        let len_before = old_contexts.len() + new_contexts.len() + self.left_to_right.len() * 2;

        // Priority 1: Exact parent context match
        self.match_by_exact_parents(old_contexts, new_contexts);

        let len_after = old_contexts.len() + new_contexts.len() + self.left_to_right.len() * 2;
        debug_assert_eq!(len_before, len_after);

        // // Priority 2: Matching with partial parents similarity (added/removed parent)
        self.match_by_changed_parents(old_contexts, new_contexts);

        let len_after = old_contexts.len() + new_contexts.len() + self.left_to_right.len() * 2;
        debug_assert_eq!(len_before, len_after);
    }

    /// Match by exact parent contexts (same parents)
    fn match_by_exact_parents(
        &mut self,
        old_contexts: &mut SVec<SymbolWithContext>,
        new_contexts: &mut SVec<SymbolWithContext>,
    ) {
        let old_iter = mem::take(old_contexts);

        let mut new_vec = mem::take(new_contexts);

        for old_ctx in old_iter {
            let with_same_context = new_vec
                .iter()
                .enumerate()
                .find(|(_, new_ctx)| &old_ctx.context == &new_ctx.context);

            let Some((id, _)) = with_same_context else {
                old_contexts.push(old_ctx);
                continue;
            };
            let new_ctx = new_vec.remove(id);
            self.left_to_right.insert(old_ctx.symbol, new_ctx.symbol);
        }

        *new_contexts = new_vec;
    }

    // Compare nodes with parents partially equal.
    fn match_by_changed_parents(
        &mut self,
        old_contexts: &mut SVec<SymbolWithContext>,
        new_contexts: &mut SVec<SymbolWithContext>,
    ) {
        let old_iter = mem::take(old_contexts);

        let mut new_vec = mem::take(new_contexts)
            .into_iter()
            .enumerate()
            .collect::<Vec<_>>();

        for old_ctx in old_iter {
            if new_vec.is_empty() {
                old_contexts.push(old_ctx);
                continue;
            }

            new_vec.sort_by_key(|(_, b)| b.context.num_same_parents(&old_ctx.context));

            // If more than one candidate context is found, then we cannot uniquely match
            if new_vec.len() > 1
                && new_vec[new_vec.len() - 2]
                    .1
                    .context
                    .num_same_parents(&old_ctx.context)
                    > 0
            {
                old_contexts.push(old_ctx);
                continue;
            }

            let (_, new_ctx) = new_vec.pop().unwrap();

            // Symbol is same by content and partially by context.
            log::warn!(
                "Matched symbol by changed context: old {:?}, new {:?}",
                old_ctx,
                new_ctx
            );

            self.left_to_right.insert(old_ctx.symbol, new_ctx.symbol);
        }
        new_vec.sort_by_key(|(original_order, _)| *original_order);
        *new_contexts = new_vec.into_iter().map(|(_, ctx)| ctx).collect();
    }
}
