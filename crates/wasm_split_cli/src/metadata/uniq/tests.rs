use std::collections::BTreeMap;

use crate::metadata::{
    linker_metadata::{DemangledName, SymbolSignature},
    uniq::{GraphNode, NodeInfo, NodeMarker, Structure},
    Hash,
};

fn create_test_function_signature(name: &str) -> SymbolSignature {
    SymbolSignature::Function {
        name: DemangledName {
            name: name.to_string(),
            distinguishing_hash: String::new(),
            anonymous: false,
        },
        lazy: false,
        params: vec![],
        results: vec![],
    }
}

fn create_test_data_signature(name: &str, size: u32) -> SymbolSignature {
    SymbolSignature::Data {
        name: DemangledName {
            name: name.to_string(),
            distinguishing_hash: String::new(),
            anonymous: false,
        },
        size,
    }
}

fn create_test_structure(
    functions: &[(&str, &str)], // (name, hash)
    data: &[(&str, &str, u32)], // (name, hash, size)
) -> Structure {
    let mut nodes = BTreeMap::new();

    for (i, (name, hash)) in functions.iter().enumerate() {
        let graph_node = GraphNode::from_index(i);
        let signature = create_test_function_signature(name);
        nodes.insert(
            graph_node,
            NodeInfo {
                signature,
                hash: Hash::hash_bytes(hash.as_bytes()),
                children: vec![],
                parents: vec![],
            },
        );
    }

    let func_count = functions.len();
    for (i, (name, hash, size)) in data.iter().enumerate() {
        let graph_node = GraphNode::from_index(func_count + i);
        let signature = create_test_data_signature(name, *size);
        nodes.insert(
            graph_node,
            NodeInfo {
                signature,
                hash: Hash::hash_bytes(hash.as_bytes()),
                children: vec![],
                parents: vec![],
            },
        );
    }

    Structure { nodes }
}

fn create_test_structure_with_parents_and_types(
    node_specs: &[(&str, &str, &str, Vec<usize>)], // (name, type, hash, parent_ids)
) -> Structure {
    let mut nodes = BTreeMap::<GraphNode, NodeInfo>::new();

    for (i, (name, node_type, hash, parent_ids)) in node_specs.iter().enumerate() {
        let graph_node = GraphNode::from_index(i);
        let signature = if *node_type == "func" {
            create_test_function_signature(name)
        } else {
            create_test_data_signature(name, 100) // default size
        };

        let parents = parent_ids
            .iter()
            .map(|&id| GraphNode::from_index(id))
            .collect();

        nodes.insert(
            graph_node,
            NodeInfo {
                signature,
                hash: Hash::hash_bytes(hash.as_bytes()),
                children: vec![],
                parents,
            },
        );
        // Add children relationships
        for parent in parent_ids {
            if let Some(parent_node) = nodes.get_mut(&GraphNode::from_index(*parent)) {
                let node_marker = NodeMarker::Lazy {
                    node: graph_node,
                    salt: 0,
                };
                parent_node.children.push(node_marker);
            }
        }
    }
    dbg!(&nodes);

    Structure { nodes }
}

#[test]
fn test_identical_functions() {
    let old_structure = create_test_structure(&[("func1", "hash1"), ("func2", "hash2")], &[]);
    let new_structure = create_test_structure(&[("func1", "hash1"), ("func2", "hash2")], &[]);

    let diff = old_structure.diff(&new_structure);

    assert_eq!(diff.added.len(), 0);
    assert_eq!(diff.removed.len(), 0);
    assert_eq!(diff.same.len(), 2);
}

#[test]
fn test_added_functions() {
    let old_structure = create_test_structure(&[("func1", "hash1")], &[]);
    let new_structure = create_test_structure(&[("func1", "hash1"), ("func2", "hash2")], &[]);

    let diff = old_structure.diff(&new_structure);

    assert_eq!(diff.added.len(), 1);
    assert_eq!(diff.removed.len(), 0);
    assert_eq!(diff.same.len(), 1);
}

#[test]
fn test_removed_functions() {
    let old_structure = create_test_structure(&[("func1", "hash1"), ("func2", "hash2")], &[]);
    let new_structure = create_test_structure(&[("func1", "hash1")], &[]);

    let diff = old_structure.diff(&new_structure);

    assert_eq!(diff.added.len(), 0);
    assert_eq!(diff.removed.len(), 1);
    assert_eq!(diff.same.len(), 1);
}

#[test]
fn test_mixed_changes() {
    let old_structure = create_test_structure(&[("func1", "hash1"), ("func2", "hash2")], &[]);
    let new_structure = create_test_structure(&[("func1", "hash1"), ("func3", "hash3")], &[]);

    let diff = old_structure.diff(&new_structure);

    assert_eq!(diff.added.len(), 1);
    assert_eq!(diff.removed.len(), 1);
    assert_eq!(diff.same.len(), 1);
}

#[test]
fn test_data_symbols() {
    let old_structure =
        create_test_structure(&[], &[("data1", "hash1", 100), ("data2", "hash2", 200)]);
    let new_structure =
        create_test_structure(&[], &[("data1", "hash1", 100), ("data3", "hash3", 300)]);

    let diff = old_structure.diff(&new_structure);

    assert_eq!(diff.added.len(), 1);
    assert_eq!(diff.removed.len(), 1);
    assert_eq!(diff.same.len(), 1);
}

#[test]
fn test_usefull_refine_hashes() {
    let mut old_structure = create_test_structure_with_parents_and_types(&[
        ("parent1", "func", "parent_hash1", vec![]),
        ("parent2", "func", "parent_hash2", vec![]),
        ("data_symbol", "data", "same_hash", vec![0]), // parent1
    ]);

    let mut new_structure = create_test_structure_with_parents_and_types(&[
        ("parent1", "func", "parent_hash1", vec![]),
        ("parent2", "func", "parent_hash2", vec![]),
        ("data_symbol", "data", "same_hash", vec![1]), // parent2
    ]);

    let diff = old_structure.diff(&new_structure);

    diff.debug();
    assert_eq!(diff.same.len(), 3);
    // Refining hashes - propagate children content to parents
    old_structure.refine_hashes(1);
    new_structure.refine_hashes(1);

    let diff = old_structure.diff(&new_structure);

    diff.debug();
    assert_eq!(diff.added.len(), 2);
    assert_eq!(diff.removed.len(), 2);
    assert_eq!(diff.same.len(), 1);
}

#[test]
fn test_data_symbol_conflicts_same_hash_different_parents() {
    let mut old_structure = create_test_structure_with_parents_and_types(&[
        ("parent1", "func", "parent_hash1", vec![]),
        ("parent2", "func", "parent_hash2", vec![]),
        ("data_symbol", "data", "same_hash", vec![0]), // parent1
    ]);
    old_structure.refine_hashes(1);

    let mut new_structure = create_test_structure_with_parents_and_types(&[
        ("parent1", "func", "parent_hash1", vec![]),
        ("parent2", "func", "parent_hash2", vec![]),
        ("data_symbol", "data", "same_hash", vec![1]), // parent2
    ]);
    new_structure.refine_hashes(1);

    let diff = old_structure.diff(&new_structure);

    diff.debug();

    // Parent1 -> because it doesn't refer to child anymore
    // Parent2 -> because it refer to a child (which wasn't a case before)
    // Child   -> Is same by content, but has different context (parent changed) - so it would be treated as different only if it conflict with another child
    assert_eq!(diff.added.len(), 2);
    assert_eq!(diff.removed.len(), 2);
    assert_eq!(diff.same.len(), 1);
}

#[test]
fn test_function_signature_hash_collision_matched_by_context() {
    let mut old_structure = create_test_structure_with_parents_and_types(&[
        ("parent_a", "func", "parent_hash_a", vec![]),
        ("overloaded_func", "func", "collision_hash", vec![0]), // under parent_a
        ("parent_b", "func", "parent_hash_b", vec![]),
    ]);

    let mut new_structure = create_test_structure_with_parents_and_types(&[
        ("parent_a", "func", "parent_hash_a", vec![]),
        ("overloaded_func", "func", "collision_hash", vec![0]), // under parent_a (matched)
        ("parent_b", "func", "parent_hash_b", vec![]),
        ("overloaded_func", "func", "collision_hash", vec![2]), // under parent_b (new)
    ]);

    new_structure.refine_hashes(1);
    old_structure.refine_hashes(1);

    let diff = old_structure.diff(&new_structure);

    diff.debug();
    assert_eq!(diff.added.len(), 2); // new overloaded_func under parent_b, but parent_b is also changed
    assert_eq!(diff.removed.len(), 1); // parent_b was removed
    assert_eq!(diff.same.len(), 2); // exact: parent_a, fuzzy: overloaded_func under parent_a
}

#[test]
fn test_mixed_function_data_diff() {
    let old_structure = create_test_structure(&[("func1", "hash1")], &[("data1", "dhash1", 100)]);
    let new_structure = create_test_structure(&[("func2", "hash2")], &[("data2", "dhash2", 200)]);

    let diff = old_structure.diff(&new_structure);

    assert_eq!(diff.added.len(), 2); // func2 + data2
    assert_eq!(diff.removed.len(), 2); // func1 + data1
    assert_eq!(diff.same.len(), 0);

    // Verify that we have both function and data in added/removed
    let data_added = diff
        .added
        .iter()
        .filter(|entry| matches!(entry.signature, SymbolSignature::Data { .. }))
        .count();
    assert_eq!(data_added, 1);
}

#[test]
fn test_parent_signature_changes_affect_context() {
    let old_structure = create_test_structure_with_parents_and_types(&[
        ("parent", "func", "old_parent_hash", vec![]),
        ("child", "func", "child_hash", vec![0]),
    ]);

    let new_structure = create_test_structure_with_parents_and_types(&[
        ("parent", "func", "new_parent_hash", vec![]),
        ("child", "func", "child_hash", vec![0]),
    ]);

    let diff = old_structure.diff(&new_structure);

    diff.debug();
    // Parent hash changed, so it's treated as removed/added
    // Child hash is the same and uniq so it's treated as the same
    assert_eq!(diff.added.len(), 1);
    assert_eq!(diff.removed.len(), 1);
    assert_eq!(diff.same.len(), 1);
}
