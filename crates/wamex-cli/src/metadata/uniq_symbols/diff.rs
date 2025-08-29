use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    mem,
};

use crate::metadata::{
    uniq_symbols::{GraphNode, NodeInfo, NodeMarker, Structure},
    Hash, SymbolSignature,
};
pub type SVec<T> = smallvec::SmallVec<[T; 2]>;

pub type SignHash = (Hash, SymbolSignature);
pub type SymbolMap = HashMap<SignHash, SVec<NodeHashContext>>;

/// Result of diffing two structures
#[derive(Debug, Clone)]
pub struct DiffResult {
    same: Vec<DiffEntry>,
    changed: Vec<DiffEntry>,
}

impl DiffResult {
    pub fn new() -> Self {
        Self {
            same: Vec::new(),
            changed: Vec::new(),
        }
    }

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
            println!("Added[{}]: {:?}", i, entry.signature());
        }
        for (i, entry) in removed_iter.enumerate() {
            println!("Removed[{}]: {:?}", i, entry.signature());
        }
        for (i, entry) in replaced_iter.enumerate() {
            println!("Replaced[{}]: {:?}", i, entry.signature());
        }
        // for (i, entry) in self.same.iter().enumerate() {
        //     println!("Same[{}]: {:?}", i, entry.signature());
        // }
    }

    /// List of nodes that remain same.
    /// This nodes should have same signature, body hash and childs but may have different parent nodes.
    pub fn same(&self) -> impl Iterator<Item = &DiffEntry> + Clone {
        self.same.iter()
    }
    /// List of nodes that cannot be matched in old and new structures.
    /// It will contain full list of removed, added or replaced nodes.
    ///
    /// This can be false positive, if signature or content hash was changed.
    pub fn all_changes(&self) -> impl Iterator<Item = &DiffEntry> + Clone {
        self.changed.iter()
    }
    /// List of nodes that was changed, but filter only those that was matched in old and new structures.
    pub fn replaced(&self) -> impl Iterator<Item = &DiffEntry> + Clone {
        self.changed.iter().filter(|entry| entry.has_old_and_new())
    }
    /// List nodes that was added in new structure.
    pub fn added(&self) -> impl Iterator<Item = &DiffEntry> + Clone {
        self.changed.iter().filter(|e| e.is_added())
    }
    /// List nodes that was removed in new structure.
    pub fn removed(&self) -> impl Iterator<Item = &DiffEntry> + Clone {
        self.changed.iter().filter(|e| e.is_removed())
    }
}

/// Entry in a diff result
#[derive(Debug, Clone)]
pub struct DiffEntry {
    pub old: Option<NodeHashContext>, // None for added entries
    pub new: Option<NodeHashContext>, // None for removed entries
}

impl DiffEntry {
    pub fn signature(&self) -> &SymbolSignature {
        self.old
            .as_ref()
            .map(|n| n.signature())
            .or_else(|| self.new.as_ref().map(|n| n.signature()))
            .expect("Either old or new node must be present")
    }
    pub fn is_added(&self) -> bool {
        self.old.is_none() && self.new.is_some()
    }
    pub fn is_removed(&self) -> bool {
        self.old.is_some() && self.new.is_none()
    }
    pub fn has_old_and_new(&self) -> bool {
        self.old.is_some() && self.new.is_some()
    }
    pub fn is_same(&self) -> bool {
        match (&self.old, &self.new) {
            (Some(old), Some(new)) => old.is_content_eq(new),
            _ => false,
        }
    }
    /// Returns true if symbols have similar signature
    pub fn is_same_signature(&self) -> bool {
        match (&self.old, &self.new) {
            (Some(old), Some(new)) => old.is_signature_eq(new),
            _ => false,
        }
    }
}

/// Helper for contextual matching
#[derive(Debug, Clone)]
pub(crate) struct NodeHashContext {
    pub node_id: GraphNode,
    pub node_info: NodeInfo,

    pub content_hash: Hash,
    pub parents_hash: Hash,
    pub parents_signatures: smallvec::SmallVec<[SignHash; 16]>,
}

impl NodeHashContext {
    fn new(node_id: GraphNode, info: &NodeInfo) -> Self {
        Self {
            node_id,
            node_info: info.clone(),
            content_hash: info.body_hash(),
            parents_hash: Hash(0),
            parents_signatures: smallvec::SmallVec::new(),
        }
    }
    pub fn node_id(&self) -> GraphNode {
        self.node_id
    }
    pub fn signature(&self) -> &SymbolSignature {
        &self.node_info.signature
    }

    /// Compare signature/order/hash without considering parents
    fn is_content_eq(&self, other: &Self) -> bool {
        self.node_info.signature == other.node_info.signature
            && self.content_hash == other.content_hash
    }

    fn is_signature_body_eq(&self, other: &Self) -> bool {
        self.node_info.signature == other.node_info.signature
            && self.node_info.body_hash == other.node_info.body_hash
    }

    fn is_signature_eq(&self, other: &Self) -> bool {
        self.node_info.signature == other.node_info.signature
    }

    fn num_same_parents(&self, other: &Self) -> usize {
        self.parents_signatures
            .iter()
            .filter(|sig| other.parents_signatures.contains(sig))
            .count()
    }

    pub fn children_hashes(&self, node_infos: &BTreeMap<GraphNode, NodeHashContext>) -> Vec<Hash> {
        self.node_info
            .children
            .iter()
            .map(|c| match c {
                NodeMarker::Static(hash) => *hash,
                // TODO: use salt
                NodeMarker::Lazy { node, salt } => {
                    let content_hash = node_infos.get(node).unwrap().content_hash;
                    Hash::from_hashable(&(content_hash, salt))
                }
            })
            .collect()
    }

    // Refine hash using kWL H(n)_Symbol = H(H0_Symbol, marker, [H(n-1)_child, ...])
    // where marker = signature, H0_Symbol = content_hash
    // and child hashes include salt (offset for data symbols)
    fn refine_hashes(nodes: &mut BTreeMap<GraphNode, NodeHashContext>, max_iterations: usize) {
        let starting_info = std::mem::take(nodes);

        let mut prev_infos = starting_info.clone();

        let mut nodes_to_update: BTreeSet<_> = starting_info.keys().copied().collect();
        for _ in 0..max_iterations {
            log::trace!("Refining hashes, {} nodes left", nodes_to_update.len());
            let mut next_hashes = prev_infos.clone();
            for node in std::mem::take(&mut nodes_to_update) {
                let children = prev_infos
                    .get(&node)
                    .expect("node should be in the registry")
                    .children_hashes(&prev_infos);

                let node_context = prev_infos.get(&node).unwrap();
                let info = &node_context.node_info;
                let new_hash =
                    Hash::from_hashable(&(&info.body_hash(), info.signature(), children));
                if node_context.content_hash == new_hash {
                    continue; // No change in hash, skip
                }
                // update content_hash
                let mut node_context = node_context.clone();
                node_context.content_hash = new_hash.clone();

                next_hashes.insert(node, node_context);
                nodes_to_update.insert(node);
            }
            prev_infos = next_hashes;
        }
        if !nodes_to_update.is_empty() {
            log::debug!(
                "{} nodes left with non-finalized hashes after {} iterations",
                nodes_to_update.len(),
                max_iterations,
            );
        }

        *nodes = prev_infos;
    }
    fn set_parents(nodes: &mut BTreeMap<GraphNode, NodeHashContext>) {
        let mut new_nodes = nodes.clone();

        for (node_id, context) in new_nodes.iter_mut() {
            debug_assert_eq!(*node_id, context.node_id);
            debug_assert_eq!(context.parents_hash, Hash(0));
            debug_assert!(context.parents_signatures.is_empty());

            let parents_signatures: smallvec::SmallVec<[SignHash; 16]> = context
                .node_info
                .parents
                .iter()
                .map(|&parent_node| {
                    let node = &nodes[&parent_node];
                    (node.content_hash, node.signature().clone())
                })
                .collect();
            let parents_hash = Hash::from_hashable(&parents_signatures);
            context.parents_hash = parents_hash;
            context.parents_signatures = parents_signatures;
        }

        *nodes = new_nodes;
    }

    pub fn trace_dups(context: &str, map: &SymbolMap) {
        for (key, nodes) in map {
            if nodes.len() <= 1 {
                continue;
            }
            log::debug!(
                "Found {} duplicates in context: {} for same signature+hash: {:?}",
                nodes.len(),
                context,
                key
            );
            for node in nodes {
                let signature = node.signature();
                log::trace!(
                    "  Symbol: {}, node: {:?}, hash: {:?}",
                    signature.display_signature(),
                    node.node_id,
                    node.content_hash,
                );
            }
            let nodes = nodes
                .iter()
                .fold(BTreeMap::new(), |mut map: BTreeMap<Hash, Vec<_>>, n| {
                    map.entry(n.parents_hash).or_default().push(n);
                    map
                });

            for (parent, nodes) in nodes {
                if nodes.len() > 1 {
                    log::debug!("  {} of them have same parents: {:?}", nodes.len(), parent);
                }
            }
        }
    }

    pub fn build_symbol_map(structure: &Structure) -> SymbolMap {
        const MAX_ITERATIONS: usize = 5;

        let mut nodes = structure
            .nodes
            .iter()
            .map(|(&id, info)| (id, NodeHashContext::new(id, info)))
            .collect();
        Self::refine_hashes(&mut nodes, MAX_ITERATIONS);
        Self::set_parents(&mut nodes);

        let mut symbol_map = HashMap::new();
        for (_, info) in nodes {
            let key = (info.content_hash, info.signature().clone());
            let entry = symbol_map.entry(key).or_insert_with(SVec::new);
            entry.push(info);
        }

        symbol_map
    }
}

impl DiffResult {
    fn symbols_content_eq(one: &[NodeHashContext], other: &[NodeHashContext]) -> bool {
        one.len() == other.len() && one.iter().zip(other).all(|(a, b)| a.is_content_eq(b))
    }

    /// Phase 2 and 3
    pub fn extract_matches(&mut self, old_map: &mut SymbolMap, new_map: &mut SymbolMap) {
        let len_before = old_map.values().map(|v| v.len()).sum::<usize>()
            + new_map.values().map(|v| v.len()).sum::<usize>()
            + self.same.len() * 2;

        // let mut exact_matches = BTreeMap::new();

        let old_iter = mem::take(old_map);
        // Find matches and collect keys for removal
        for (key, old_nodes) in old_iter {
            let Some(new_nodes) = new_map.remove(&key) else {
                old_map.insert(key, old_nodes);
                continue;
            };
            match Self::symbols_content_eq(&old_nodes, &new_nodes) {
                // Phase 2: Find exact matches and clean maps simultaneously
                true => {
                    for (old, new) in old_nodes.iter().zip(new_nodes.iter()) {
                        self.same.push(DiffEntry {
                            old: Some(old.clone()),
                            new: Some(new.clone()),
                        });
                    }
                }
                // Phase 3: Context-based signature matching
                false => {
                    let mut old_nodes = old_nodes;
                    let mut new_nodes = new_nodes;
                    self.match_list_by_context(&mut old_nodes, &mut new_nodes);
                    // return back non-touched nodes
                    if old_nodes.len() > 0 {
                        old_map.insert(key.clone(), old_nodes);
                    }
                    if new_nodes.len() > 0 {
                        new_map.insert(key, new_nodes);
                    }
                }
            }
        }

        let len_after = old_map.values().map(|v| v.len()).sum::<usize>()
            + new_map.values().map(|v| v.len()).sum::<usize>()
            + self.same.len() * 2;
        debug_assert_eq!(len_before, len_after);
    }

    pub fn match_list_by_context(
        &mut self,
        old_contexts: &mut SVec<NodeHashContext>,
        new_contexts: &mut SVec<NodeHashContext>,
    ) {
        let len_before = old_contexts.len() + new_contexts.len() + self.same.len() * 2;

        // Priority 1: Exact parent context match
        self.match_by_exact_parents(old_contexts, new_contexts);

        let len_after = old_contexts.len() + new_contexts.len() + self.same.len() * 2;
        debug_assert_eq!(len_before, len_after);

        // Priority 2: Matching with partial parents similarity (added/removed parent)
        self.match_by_changed_parents(old_contexts, new_contexts);

        let len_after = old_contexts.len() + new_contexts.len() + self.same.len() * 2;
        debug_assert_eq!(len_before, len_after);
    }

    /// Match by exact parent contexts (same parents)
    pub fn match_by_exact_parents(
        &mut self,
        old_contexts: &mut SVec<NodeHashContext>,
        new_contexts: &mut SVec<NodeHashContext>,
    ) {
        let old_iter = mem::take(old_contexts);

        let mut new_vec = mem::take(new_contexts);

        for old_ctx in old_iter {
            let with_same_context = new_vec.iter().enumerate().find(|(_, new_ctx)| {
                old_ctx.parents_hash == new_ctx.parents_hash
                    && old_ctx.parents_signatures == new_ctx.parents_signatures
            });

            let Some((id, _)) = with_same_context else {
                old_contexts.push(old_ctx);
                continue;
            };
            let new_ctx = new_vec.remove(id);
            self.same.push(DiffEntry {
                old: Some(old_ctx.clone()),
                new: Some(new_ctx.clone()),
            });
        }

        *new_contexts = new_vec;
    }

    // Compare nodes with parents partially equal.
    pub fn match_by_changed_parents(
        &mut self,
        old_contexts: &mut SVec<NodeHashContext>,
        new_contexts: &mut SVec<NodeHashContext>,
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

            new_vec.sort_by_key(|(_, b)| b.num_same_parents(&old_ctx));

            // If more than one candidate context is found, then we cannot uniquely match
            if new_vec.len() > 1 && new_vec[new_vec.len() - 2].1.num_same_parents(&old_ctx) > 0 {
                old_contexts.push(old_ctx);
                continue;
            }

            let (_, new_ctx) = new_vec.pop().unwrap();
            self.same.push(DiffEntry {
                old: Some(old_ctx.clone()),
                new: Some(new_ctx.clone()),
            });
        }
        new_vec.sort_by_key(|(original_order, _)| *original_order);
        *new_contexts = new_vec.into_iter().map(|(_, ctx)| ctx).collect();
    }

    /// Phase 4: Result classification
    pub fn classify_changes(&mut self, old_map: SymbolMap, new_map: SymbolMap) {
        let mut added = Vec::new();
        let mut removed = Vec::new();

        // Process completely unmatched nodes

        for context in old_map.into_values().flatten() {
            removed.push(DiffEntry {
                old: Some(context),
                new: None,
            });
        }

        for context in new_map.into_values().flatten() {
            added.push(DiffEntry {
                old: None,
                new: Some(context),
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
                .filter(|(_, e)| e.signature() == added_entry.signature())
                .map(|(i, _)| i);

            if removed_iter.clone().count() != 1 {
                // either no match or multiple matches - cannot be replaced
                changed.push(added_entry);
                continue;
            }

            let removed_index = removed_iter.next().unwrap();

            let removed_entry = removed.remove(removed_index);
            added_entry.old = removed_entry.old;
            replaced.push(added_entry);
        }

        changed.extend(removed.into_iter());
        changed.extend(replaced.into_iter());

        self.changed = changed;
    }
}
