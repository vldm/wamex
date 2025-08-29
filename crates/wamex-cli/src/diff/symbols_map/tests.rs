use std::collections::BTreeMap;

use wamex_metadata::{DemangledName, SymbolSignature};

use crate::{
    diff::symbols_map::{GraphNode, NodeInfo, NodeMarker, Structure},
    helpers::Hash,
};

fn create_test_function_signature(name: &str) -> SymbolSignature {
    SymbolSignature::Function {
        name: DemangledName::new(name, false),
        lazy: false,
        params: vec![],
        results: vec![],
    }
}

fn create_test_data_signature(name: &str, size: u32) -> SymbolSignature {
    SymbolSignature::Data {
        name: DemangledName::new(name, true),
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
                body_hash: Hash::hash_bytes(hash.as_bytes()),
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
                body_hash: Hash::hash_bytes(hash.as_bytes()),
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
                body_hash: Hash::hash_bytes(hash.as_bytes()),
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

    diff.debug();
    assert_eq!(diff.added().count(), 0);
    assert_eq!(diff.removed().count(), 0);
    assert_eq!(diff.same().count(), 2);
}

#[test]
fn test_identical_data() {
    let old_structure =
        create_test_structure(&[], &[(".Ldata1", "hash1", 100), ("data2", "hash2", 200)]);
    let new_structure = create_test_structure(
        &[],
        &[(".Ldata1_name", "hash1", 100), ("data2", "hash2", 200)],
    ); // name for anonymous data is not important

    assert_eq!(
        old_structure.nodes[&GraphNode::from_index(1)].signature(),
        new_structure.nodes[&GraphNode::from_index(1)].signature()
    );
    let diff = old_structure.diff(&new_structure);

    diff.debug();
    assert_eq!(diff.added().count(), 0);
    assert_eq!(diff.removed().count(), 0);
    assert_eq!(diff.same().count(), 2);
}

#[test]
fn test_name_of_non_anonymous_data_changed() {
    let old_structure =
        create_test_structure(&[], &[(".Ldata1", "hash1", 100), ("data1", "hash2", 200)]);
    let new_structure = create_test_structure(
        &[],
        &[(".Ldata1_name", "hash1", 100), ("data2", "hash2", 200)],
    ); // name for anonymous data is not important

    assert_ne!(
        old_structure.nodes[&GraphNode::from_index(1)].signature(),
        new_structure.nodes[&GraphNode::from_index(1)].signature()
    );
    let diff = old_structure.diff(&new_structure);

    diff.debug();
    assert_eq!(diff.added().count(), 1);
    assert_eq!(diff.removed().count(), 1);
    assert_eq!(diff.same().count(), 1);
}

#[test]
fn test_added_functions() {
    let old_structure = create_test_structure(&[("func1", "hash1")], &[]);
    let new_structure = create_test_structure(&[("func1", "hash1"), ("func2", "hash2")], &[]);

    let diff = old_structure.diff(&new_structure);

    diff.debug();

    assert_eq!(diff.added().count(), 1);
    assert_eq!(diff.removed().count(), 0);
    assert_eq!(diff.same().count(), 1);
}

#[test]
fn test_removed_functions() {
    let old_structure = create_test_structure(&[("func1", "hash1"), ("func2", "hash2")], &[]);
    let new_structure = create_test_structure(&[("func1", "hash1")], &[]);

    let diff = old_structure.diff(&new_structure);

    assert_eq!(diff.added().count(), 0);
    assert_eq!(diff.removed().count(), 1);
    assert_eq!(diff.same().count(), 1);
}

#[test]
fn test_mixed_changes() {
    let old_structure = create_test_structure(&[("func1", "hash1"), ("func2", "hash2")], &[]);
    let new_structure = create_test_structure(&[("func1", "hash1"), ("func3", "hash3")], &[]);

    let diff = old_structure.diff(&new_structure);

    assert_eq!(diff.added().count(), 1);
    assert_eq!(diff.removed().count(), 1);
    assert_eq!(diff.same().count(), 1);
}

#[test]
fn test_data_symbols() {
    let old_structure =
        create_test_structure(&[], &[("data1", "hash1", 100), ("data2", "hash2", 200)]);
    let new_structure =
        create_test_structure(&[], &[("data1", "hash1", 100), ("data3", "hash3", 300)]);

    let diff = old_structure.diff(&new_structure);

    assert_eq!(diff.added().count(), 1);
    assert_eq!(diff.removed().count(), 1);
    assert_eq!(diff.same().count(), 1);
}

#[test]
fn test_data_symbol_conflicts_same_hash_different_parents() {
    let old_structure = create_test_structure_with_parents_and_types(&[
        ("parent1", "func", "parent_hash1", vec![]),
        ("parent2", "func", "parent_hash2", vec![]),
        ("data_symbol", "data", "same_hash", vec![0]), // parent1
    ]);

    let new_structure = create_test_structure_with_parents_and_types(&[
        ("parent1", "func", "parent_hash1", vec![]),
        ("parent2", "func", "parent_hash2", vec![]),
        ("data_symbol", "data", "same_hash", vec![1]), // parent2
    ]);

    let diff = old_structure.diff(&new_structure);

    diff.debug();

    // Parent1 -> because it doesn't refer to child anymore
    // Parent2 -> because it refer to a child (which wasn't a case before)
    // Child   -> Is same by content, but has different context (parent changed) - so it would be treated as different only if it conflict with another child
    assert_eq!(diff.replaced().count(), 2);
    assert_eq!(diff.added().count(), 0);
    assert_eq!(diff.removed().count(), 0);
    assert_eq!(diff.same().count(), 1);
}

#[test]
fn test_function_signature_hash_collision_matched_by_context() {
    let old_structure = create_test_structure_with_parents_and_types(&[
        ("parent_a", "func", "parent_hash_a", vec![]),
        ("overloaded_func", "func", "collision_hash", vec![0]), // under parent_a
        ("parent_b", "func", "parent_hash_b", vec![]),
    ]);

    let new_structure = create_test_structure_with_parents_and_types(&[
        ("parent_a", "func", "parent_hash_a", vec![]),
        ("overloaded_func", "func", "collision_hash", vec![0]), // under parent_a (matched)
        ("parent_b", "func", "parent_hash_b", vec![]),
        ("overloaded_func", "func", "collision_hash", vec![2]), // under parent_b (new)
    ]);

    let diff = old_structure.diff(&new_structure);

    diff.debug();
    assert_eq!(diff.replaced().count(), 1); // parent_b was replaced
    assert_eq!(diff.added().count(), 1); // new overloaded_func under parent_b
    assert_eq!(diff.removed().count(), 0);
    assert_eq!(diff.same().count(), 2); // exact: parent_a, fuzzy: overloaded_func under parent_a
}

#[test]
fn test_mixed_function_data_diff() {
    let old_structure = create_test_structure(&[("func1", "hash1")], &[("data1", "dhash1", 100)]);
    let new_structure = create_test_structure(&[("func2", "hash2")], &[("data2", "dhash2", 200)]);

    let diff = old_structure.diff(&new_structure);

    assert_eq!(diff.added().count(), 2); // func2 + data2
    assert_eq!(diff.removed().count(), 2); // func1 + data1
    assert_eq!(diff.same().count(), 0);

    // Verify that we have both function and data in added/removed
    let data_added = diff
        .added()
        .filter(|entry| matches!(entry.signature(), SymbolSignature::Data { .. }))
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
    assert_eq!(diff.replaced().count(), 1);
    assert_eq!(diff.added().count(), 0);
    assert_eq!(diff.removed().count(), 0);
    assert_eq!(diff.same().count(), 1);
}

#[test]
fn test_snapshot_and_recover_no_diff() {
    let structure = create_test_structure(&[("func1", "hash1")], &[("data1", "dhash1", 100)]);

    let snapshot = structure.snapshot();
    let recovered = Structure::recover_from_snapshot(&snapshot).unwrap();

    assert_eq!(structure, recovered);
}
