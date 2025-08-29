//! This module provides functionality to retrieve canonical ids for functions and data segments.
//!
//! This is important to calculate diffs between recompiliations.
//! Currently algorithm following:
//! - for all data/function symbols relocations are applied with 0 offsets/ids.
//! - hashes for function body/data parts are calculated this is symbol content_hash.
//! - optionally refine hashes using kWL algorithm (see Structure::refine_hashes) - to make it more dependant on children.
//! - the unique id is calculated as a hash of (content_signature, content_hash, [deps_signatures, deps_content_hashes])
//!
//! - During match we first match pairs of (content_signature, content_hash) if there are multiple candidates we trying to match by context (graph parents).

use std::collections::{BTreeMap, BTreeSet};

use graph_utils::Child;

use crate::{
    analysis::{dep_graph::DepNode, ModuleInfo},
    emit::{DataSegment, ModuleEmitState, NamedData, SymbolRelation},
    helpers::RangeExt,
    index::{DataSegmentId, DataSymbolId, Id, IdMap, IdVec, InputFuncId},
    metadata::{
        metadata_ext::SymbolSignatureExt, uniq_symbols::diff::NodeHashContext, Hash,
        SymbolSignature,
    },
};

mod diff;
mod graph_utils;
use anyhow::Result;
pub use diff::DiffResult;

fn apply_empty_relocs(body: &mut [u8], relocations: &[wasmparser::RelocationEntry]) {
    for rel in relocations {
        let reloc_range = rel.relocation_range();
        body[reloc_range].fill(0);
    }
}

/// Returns a list of relocations for a function.
/// Each offset of relocation entry is modified relative to the start of the function body.
fn list_function_relocs(
    module: &ModuleInfo,
    all_relocations: &[wasmparser::RelocationEntry],
    fn_id: InputFuncId,
) -> Vec<wasmparser::RelocationEntry> {
    let defined_id = module.as_defined_function_id(fn_id).unwrap();
    let func_info = &module.source.code.defined_funcs[defined_id];
    let range = func_info.body.range();
    let func_relocs = ModuleEmitState::get_relocations_for_range(all_relocations, &range);
    func_relocs
        .iter()
        .map(|rel| rel.shift_left(range.start))
        .collect()
}

fn get_function_content_hash(
    module: &ModuleInfo,
    all_relocations: &[wasmparser::RelocationEntry],
    fn_id: InputFuncId,
) -> Option<(Hash, Vec<wasmparser::RelocationEntry>)> {
    let defined_fn_id = module.as_defined_function_id(fn_id)?;
    let mut body = module.source.code.section_payload.defined_funcs[defined_fn_id]
        .body
        .as_bytes()
        .to_vec();
    let relocations = list_function_relocs(module, all_relocations, fn_id);
    apply_empty_relocs(&mut body, &relocations);
    Some((Hash::hash_bytes(&body), relocations))
}

fn get_data_chunk<'a>(
    segment_info: &'a DataSegment<'a>,
    idx: DataSymbolId,
    data_symbol: &'a NamedData<'a>,
) -> &'a [u8] {
    match data_symbol.symbol_relation() {
        SymbolRelation::Regular { chunk, .. } => *chunk,
        SymbolRelation::BoundToPrevious { offset, len } => {
            //TODO: hide in DataSegment impl
            let mut iter = segment_info._data_symbols_rev_iter(idx).peekable();
            while let Some(SymbolRelation::BoundToPrevious { .. }) =
                iter.peek().map(|s| s.symbol_relation())
            {
                // skip bound symbols
                iter.next();
            }

            let SymbolRelation::Regular {
                chunk: prev_chunk, ..
            } = iter
                .next()
                .expect("should have previous regular symbol")
                .symbol_relation()
            else {
                panic!("first symbol should be regular");
            };
            let start = prev_chunk.len() - offset;
            let range = start..start + len;
            &prev_chunk[range]
        }
    }
}

fn get_data_content_hash(
    data_segments: &IdVec<DataSegment<'_>>,
    segment_id: DataSegmentId,
    idx: DataSymbolId,
) -> (Hash, Vec<wasmparser::RelocationEntry>) {
    let data_segment = data_segments
        .get(segment_id)
        .expect("data segment should exist");
    let data_symbol = data_segment
        .get_data_symbol(idx)
        .expect("data symbol should exist");

    let relocs = data_symbol.relocations();
    let mut data = get_data_chunk(data_segment, idx, data_symbol).to_vec();
    apply_empty_relocs(&mut data, relocs);
    let hash = Hash::hash_bytes(&data);

    (hash, relocs.to_vec())
}

impl_standalone_index! {
    GraphNode(_GraphNode)
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub(crate) enum NodeMarker {
    Lazy { node: GraphNode, salt: u32 },
    Static(Hash),
}

impl NodeMarker {
    fn new_from_child(module_mapper: &BTreeMap<DepNode, GraphNode>, child: &Child) -> NodeMarker {
        match child {
            Child::DataSymbol { id, offset } => {
                let dep_node = DepNode::DataSymbol(id.0, id.1);
                let node = *module_mapper
                    .get(&dep_node)
                    .expect("child node should be in the registry");
                NodeMarker::Lazy {
                    node,
                    salt: *offset,
                }
            }
            Child::Function(func_id) => {
                let dep_node = DepNode::Function(*func_id);
                let node = *module_mapper
                    .get(&dep_node)
                    .expect("child node should be in the registry");
                NodeMarker::Lazy { node, salt: 0 }
            }
            Child::OtherReloc(other_reloc) => NodeMarker::Static(Hash::from_hashable(&other_reloc)),
        }
    }
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct NodeInfo<Child = NodeMarker> {
    /// Consistent body hash (filtered out symbols)
    pub body_hash: Hash,
    pub signature: SymbolSignature,
    pub children: Vec<Child>,
    // Parents are not used in hash calculation, but used later for differentiation
    // of symbols with same content hash.
    pub parents: Vec<GraphNode>,
}
impl NodeInfo {
    pub fn body_hash(&self) -> Hash {
        self.body_hash
    }
    pub fn signature(&self) -> &SymbolSignature {
        &self.signature
    }
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct Structure {
    pub nodes: BTreeMap<GraphNode, NodeInfo>,
}
impl Structure {
    pub fn snapshot(&self) -> crate::metadata::Snapshot {
        let symbols = self
            .nodes
            .iter()
            .map(|(_node_id, info)| {
                let content_hash = info.body_hash();
                let signature = info.signature().clone();
                (signature, content_hash)
            })
            .collect::<Vec<_>>();

        let deps = self
            .nodes
            .iter()
            .filter_map(|(id, info)| {
                let children = info.children.iter().filter_map(|c| match c {
                    NodeMarker::Lazy { node, salt: _ } => Some(node.as_raw_index()),
                    NodeMarker::Static(_) => {
                        // todo: add static childs as well?
                        None
                    }
                });

                let children = children.collect::<BTreeSet<_>>();
                if children.is_empty() {
                    None
                } else {
                    Some((id.as_raw_index(), children))
                }
            })
            .collect();

        crate::metadata::Snapshot { symbols, deps }
    }

    /// Diff this structure against another structure to find added, removed, and same nodes
    pub fn diff(&self, other: &Structure) -> DiffResult {
        // Phase 1: Build identity maps
        let mut old_identity_map = NodeHashContext::build_symbol_map(&self);
        let mut new_identity_map = NodeHashContext::build_symbol_map(&other);

        // Debug print duplicate nodes (usually only anonymous data symbols, but can be some hash collisions)
        NodeHashContext::trace_dups("old", &old_identity_map);
        NodeHashContext::trace_dups("new", &new_identity_map);

        let mut result = DiffResult::new();
        // Phase 2: Find exact matches and clean maps simultaneously &&
        // Phase 3: Signature-based matching using context and order
        result.extract_matches(&mut old_identity_map, &mut new_identity_map);

        // Phase 4: Classify remaining as different types of changes
        result.classify_changes(old_identity_map, new_identity_map);
        result
    }

    pub fn recover_from_snapshot(snapshot: &crate::metadata::Snapshot) -> Result<Self> {
        let mut nodes = snapshot
            .symbols
            .iter()
            .enumerate()
            .map(|(id, (signature, content_hash))| {
                let node = GraphNode::from_index(id);
                let children_ids = snapshot
                    .deps
                    .get(&id)
                    .into_iter()
                    .flatten()
                    .map(|child_id| {
                        NodeMarker::Lazy {
                            node: GraphNode::from_index(*child_id),
                            salt: 0, // TODO: handle Salt and non-lazy children
                        }
                    });
                let info = NodeInfo {
                    signature: signature.clone(),
                    body_hash: *content_hash,
                    children: children_ids.collect(),
                    parents: Vec::new(),
                };
                Ok((node, info))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        //TODO: populate parents
        Ok(Structure { nodes })
    }
}

pub struct ModuleStructure {
    pub structure: Structure,
    pub module_nodes: IdMap<GraphNode, DepNode>,
}
impl ModuleStructure {
    pub fn new_from_module(
        module: &ModuleInfo,
        all_relocations: &[wasmparser::RelocationEntry],
        data_segments: &IdVec<DataSegment<'_>>,
    ) -> Self {
        let mut module_nodes = IdMap::new();
        let mut structure_nodes = BTreeMap::new();

        let data_iter = module
            .data_symbols
            .iter()
            .map(|symbol| DepNode::DataSymbol(symbol.segment_index, symbol.symbol_index));
        let func_iter = module.function_id_iter().map(|id| DepNode::Function(id));

        // 1st pass: collect all children
        for (i, node) in func_iter.chain(data_iter).enumerate() {
            let id = Id::from_index(i);
            module_nodes.insert(id, node.clone());

            let (content_hash, children) = match node {
                DepNode::Function(id) => {
                    get_function_content_hash(module, all_relocations, id).unwrap_or_default()
                }
                DepNode::DataSymbol(segment, idx) => {
                    get_data_content_hash(&data_segments, segment, idx)
                }
            };
            let signature = SymbolSignature::from_node(module, &node);

            let node_info = NodeInfo::<Child> {
                signature,
                body_hash: content_hash,
                children: children
                    .iter()
                    .map(|rel| Child::from_relocation(module, rel))
                    .collect(),
                parents: Vec::new(),
            };
            structure_nodes.insert(id, node_info);
        }

        // Map from Module dep (DepNode) to serializable GraphNode (plain index)
        let graph_to_struct_id = module_nodes
            .iter()
            .map(|(id, node)| (node.clone(), id))
            .collect();

        let mut parents_map = BTreeMap::<GraphNode, Vec<GraphNode>>::new();

        // 2nd pass: convert children to NodeMarkers
        // collect parents

        let mut structure_nodes: BTreeMap<GraphNode, NodeInfo<NodeMarker>> = structure_nodes
            .into_iter()
            .map(|(id, info)| {
                let children: Vec<NodeMarker> = info
                    .children
                    .into_iter()
                    .map(|c| NodeMarker::new_from_child(&graph_to_struct_id, &c))
                    .collect();

                let parents = children.iter().filter_map(|c| match c {
                    NodeMarker::Lazy { node, salt: _ } => Some((*node, id)),
                    NodeMarker::Static(_) => None,
                });
                // extend parents map
                for (child, parent) in parents {
                    parents_map.entry(child).or_default().push(parent);
                }

                (
                    id,
                    NodeInfo {
                        children,
                        // TODO: fill name signature, etc
                        parents: Vec::new(),
                        body_hash: info.body_hash,
                        signature: info.signature,
                    },
                )
            })
            .collect();
        // 3rd pass: fill parents
        for (node, parents) in parents_map {
            if let Some(info) = structure_nodes.get_mut(&node) {
                debug_assert!(
                    info.parents.is_empty(),
                    "parents should be filled only once"
                );
                info.parents = parents;
            }
        }

        Self {
            module_nodes,
            structure: Structure {
                nodes: structure_nodes,
            },
        }
    }
}

#[cfg(test)]
mod tests;
