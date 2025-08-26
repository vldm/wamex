use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    mem, result,
};

use smallvec::SmallVec;

use crate::{
    analysis::{dep_graph::DepNode, ModuleInfo},
    emit::{DataSegment, ModuleEmitState, NamedData, SymbolRelation},
    helpers::RangeExt,
    index::{DataSegmentId, DataSymbolId, Id, IdMap, IdVec, InputFuncId},
    metadata::{
        graph_utils::Child,
        uniq::{GraphNode, NodeInfo, Structure},
        Hash, SymbolSignature,
    },
};

/// Result of diffing two structures
#[derive(Debug, Clone)]
pub struct DiffResult {
    pub same: Vec<DiffEntry>,
    pub changed: Vec<DiffEntry>,
}

impl DiffResult {
    pub fn debug(&self) {
        let replaced_iter = self.replaced();
        let added_iter = self.added();
        let removed_iter = self.removed();
        // Debug output
        println!(
            "Replaced: {}, Added: {}, Removed: {}, Same: {}",
            replaced_iter.clone().count(),
            added_iter.clone().count(),
            removed_iter.clone().count(),
            self.same.len()
        );
        for (i, entry) in added_iter.enumerate() {
            println!("Added[{}]: {:?}", i, entry.signature);
        }
        for (i, entry) in removed_iter.enumerate() {
            println!("Removed[{}]: {:?}", i, entry.signature);
        }
        for (i, entry) in replaced_iter.enumerate() {
            println!("Replaced[{}]: {:?}", i, entry.signature);
        }
        for (i, entry) in self.same.iter().enumerate() {
            println!("Same[{}]: {:?}", i, entry.signature);
        }
    }

    pub fn replaced(&self) -> impl Iterator<Item = &DiffEntry> + Clone {
        self.changed
            .iter()
            .filter(|entry| entry.old_node.is_some() && entry.new_node.is_some())
    }
    pub fn added(&self) -> impl Iterator<Item = &DiffEntry> + Clone {
        self.changed.iter().filter(|e| e.old_node.is_none())
    }
    pub fn removed(&self) -> impl Iterator<Item = &DiffEntry> + Clone {
        self.changed.iter().filter(|e| e.new_node.is_none())
    }
}

/// Entry in a diff result
#[derive(Debug, Clone)]
pub struct DiffEntry {
    pub signature: SymbolSignature,
    // TODO: old/new hashes
    pub content_hash: Hash,
    pub changed_content_hash: Option<Hash>,
    pub old_node: Option<GraphNode>, // None for added entries
    pub new_node: Option<GraphNode>, // None for removed entries
}

pub type SVec<T> = smallvec::SmallVec<[T; 2]>;

pub type SignHash = (Hash, SymbolSignature);
pub type SymbolMap = HashMap<SignHash, SVec<NodeContext>>;

/// Helper for contextual matching
#[derive(Debug, Clone)]
pub(crate) struct NodeContext {
    pub node: GraphNode,
    pub signature: SymbolSignature,
    pub content_hash: Hash,
    pub parents_hash: Hash,
    pub parents_signatures: SmallVec<[SignHash; 16]>,
}

impl PartialEq for NodeContext {
    fn eq(&self, other: &Self) -> bool {
        self.content_eq(other)
    }
}

impl NodeContext {
    pub fn new(structure: &Structure, node: GraphNode, info: &NodeInfo) -> Self {
        let parents_signatures: SmallVec<[SignHash; 16]> = info
            .parents
            .iter()
            .map(|&parent_node| {
                let node = &structure.nodes[&parent_node];
                (node.content_hash(), node.signature().clone())
            })
            .collect();
        let parents_hash = Hash::from_hashable(&parents_signatures);
        Self {
            node,
            signature: info.signature().clone(),
            content_hash: info.content_hash(),
            parents_hash,
            parents_signatures,
        }
    }

    /// Compare signature/order/hash without considering parents
    fn content_eq(&self, other: &Self) -> bool {
        self.signature == other.signature && self.content_hash == other.content_hash
    }

    fn num_same_parents(&self, other: &Self) -> usize {
        self.parents_signatures
            .iter()
            .filter(|sig| other.parents_signatures.contains(sig))
            .count()
    }
}

/// Phase 2: Find exact matches and clean maps simultaneously
pub fn extract_exact_matches(
    old_map: &mut SymbolMap,
    new_map: &mut SymbolMap,
) -> BTreeMap<GraphNode, GraphNode> {
    let len_before = old_map.values().map(|v| v.len()).sum::<usize>()
        + new_map.values().map(|v| v.len()).sum::<usize>();

    let mut exact_matches = BTreeMap::new();

    let old_iter = mem::take(old_map);
    // Find matches and collect keys for removal
    for (key, old_nodes) in old_iter {
        let Some(new_nodes) = new_map.remove(&key) else {
            old_map.insert(key, old_nodes);
            continue;
        };
        if &old_nodes != &new_nodes {
            old_map.insert(key.clone(), old_nodes);
            new_map.insert(key, new_nodes);
            continue; // Only consider unique nodes for exact matching
        }

        for (old, new) in old_nodes.iter().zip(new_nodes.iter()) {
            exact_matches.insert(old.node, new.node);
        }
    }

    let len_after = old_map.values().map(|v| v.len()).sum::<usize>()
        + new_map.values().map(|v| v.len()).sum::<usize>()
        + exact_matches.len() * 2;
    debug_assert_eq!(len_before, len_after);

    exact_matches
}

/// Phase 3: Context-based signature matching
pub fn extract_fuzzy_matches(
    old_map: &mut SymbolMap,
    new_map: &mut SymbolMap,
) -> BTreeMap<GraphNode, GraphNode> {
    let len_before = old_map.values().map(|v| v.len()).sum::<usize>()
        + new_map.values().map(|v| v.len()).sum::<usize>();

    let mut fuzzy_matches = BTreeMap::new();

    let old_iter = mem::take(old_map);

    for (key, mut old_nodes) in old_iter {
        let Some(mut new_nodes) = new_map.remove(&key) else {
            old_map.insert(key, old_nodes);
            continue;
        };

        fuzzy_matches.extend(match_by_context(&mut old_nodes, &mut new_nodes));
        if old_nodes.len() > 0 {
            old_map.insert(key.clone(), old_nodes);
        }
        if new_nodes.len() > 0 {
            new_map.insert(key, new_nodes);
        }
    }

    let len_after = old_map.values().map(|v| v.len()).sum::<usize>()
        + new_map.values().map(|v| v.len()).sum::<usize>()
        + fuzzy_matches.len() * 2;

    debug_assert_eq!(len_before, len_after);

    fuzzy_matches
}

pub fn match_by_context(
    old_contexts: &mut SVec<NodeContext>,
    new_contexts: &mut SVec<NodeContext>,
) -> BTreeMap<GraphNode, GraphNode> {
    let len_before = old_contexts.len() + new_contexts.len();
    let mut matches = BTreeMap::new();

    // Priority 1: Exact parent context match
    let exact_parent_matches = match_by_exact_parents(old_contexts, new_contexts);
    matches.extend(exact_parent_matches);

    let len_after = old_contexts.len() + new_contexts.len() + matches.len() * 2;
    debug_assert_eq!(len_before, len_after);

    // Priority 2: Matching with partial parents similarity (added/removed parent)
    let order_matches = match_by_changed_parents(old_contexts, new_contexts);
    matches.extend(order_matches);

    let len_after = old_contexts.len() + new_contexts.len() + matches.len() * 2;
    debug_assert_eq!(len_before, len_after);

    matches
}

/// Match by exact parent contexts (same parents)
pub fn match_by_exact_parents(
    old_contexts: &mut SVec<NodeContext>,
    new_contexts: &mut SVec<NodeContext>,
) -> BTreeMap<GraphNode, GraphNode> {
    let mut matches = BTreeMap::new();

    let old_iter = mem::take(old_contexts);

    let mut new_vec = mem::take(new_contexts);

    for old_ctx in old_iter {
        let Some((id, _)) = new_vec
            .iter()
            .enumerate()
            .find(|(_, new_ctx)| old_ctx.parents_hash == new_ctx.parents_hash)
        else {
            old_contexts.push(old_ctx);
            continue;
        };
        let new_ctx = new_vec.remove(id);
        matches.insert(old_ctx.node, new_ctx.node);
    }

    *new_contexts = new_vec;

    matches
}

// Compare nodes with parents partially equal.
pub fn match_by_changed_parents(
    old_contexts: &mut SVec<NodeContext>,
    new_contexts: &mut SVec<NodeContext>,
) -> BTreeMap<GraphNode, GraphNode> {
    let mut matches = BTreeMap::new();

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

        new_vec.sort_by_key(|(_, b)| b.num_same_parents(&old_ctx));

        // If more than one candidate context is found, then we cannot uniquely match
        if new_vec.len() > 1 && new_vec[new_vec.len() - 2].1.num_same_parents(&old_ctx) > 0 {
            old_contexts.push(old_ctx);
            continue;
        }

        let (_, new_ctx) = new_vec.pop().unwrap();

        matches.insert(old_ctx.node, new_ctx.node);
    }
    new_vec.sort_by_key(|(original_order, _)| *original_order);
    *new_contexts = new_vec.into_iter().map(|(_, ctx)| ctx).collect();

    matches
}

/// Phase 4: Result classification
pub fn classify_results(
    old_structure: &Structure,
    old_map: SymbolMap,
    new_map: SymbolMap,
    exact_matches: BTreeMap<GraphNode, GraphNode>,
) -> DiffResult {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut same = Vec::new();

    // Process exact matches -> same category
    for (old_node, new_node) in exact_matches {
        let old_info = &old_structure.nodes[&old_node];
        same.push(DiffEntry {
            signature: old_info.signature().clone(),
            content_hash: old_info.content_hash(),
            changed_content_hash: None,
            old_node: Some(old_node),
            new_node: Some(new_node),
        });
    }

    // Process completely unmatched nodes

    for contexts in old_map.into_values().flatten() {
        removed.push(DiffEntry {
            signature: contexts.signature.clone(),
            content_hash: contexts.content_hash.clone(),
            changed_content_hash: None,
            old_node: Some(contexts.node),
            new_node: None,
        });
    }

    for contexts in new_map.into_values().flatten() {
        added.push(DiffEntry {
            signature: contexts.signature.clone(),
            content_hash: contexts.content_hash.clone(), // TODO: remove old?
            changed_content_hash: Some(contexts.content_hash.clone()),
            old_node: None,
            new_node: Some(contexts.node),
        });
    }

    let mut changed = Vec::new();
    let mut replaced = Vec::new();

    // merge added and removed by signature
    for mut added_entry in added {
        // if we found a matching removed entry - mark it as replaced
        let mut removed_iter = removed
            .iter()
            .enumerate()
            .filter(|(_, e)| e.signature == added_entry.signature)
            .map(|(i, _)| i);

        if removed_iter.clone().count() != 1 {
            // either no match or multiple matches - cannot be replaced
            changed.push(added_entry);
            continue;
        }

        let removed_index = removed_iter.next().unwrap();

        let removed_entry = removed.remove(removed_index);
        added_entry.old_node = removed_entry.old_node;
        added_entry.content_hash = removed_entry.content_hash;
        replaced.push(added_entry);
    }

    changed.extend(removed.into_iter());
    changed.extend(replaced.into_iter());

    DiffResult { changed, same }
}
