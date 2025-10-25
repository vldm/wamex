use anyhow::anyhow;

use crate::analysis::dep_graph::{DepGraph, DepList};

mod dot_parser;

/// Parses dependencies from a string in the format of a DOT like graph.
pub fn parse_deps(input: &str) -> anyhow::Result<DepGraph> {
    let val =
        dot_parser::parse_deps(input).map_err(|e| anyhow!("Failed to parse dependencies: {e}"))?;
    let mut graph = DepGraph::new();
    for (parent, childs) in val.1 {
        for child in childs {
            graph.insert_child(parent.clone(), child);
        }
    }
    Ok(graph)
}

/// Returns a set of unique nodes from the input string.
///
/// The input string should be in format of `&` separated list of nodes.
/// For example: `D(5, 6) & D(8, 9) & F(4) & F(7)`.
pub fn uniq_nodes(input: &str) -> anyhow::Result<DepList> {
    let (_input, deps) =
        dot_parser::parse_list(input).map_err(|e| anyhow!("Failed to parse list: {e}"))?;

    let mut nodes = DepList::new();
    for value in deps {
        nodes.insert(value);
    }
    Ok(nodes)
}
