use crate::{
    analysis::dep_graph::{DepGraph, DepSet},
    typed::{
        Module,
        common_index::{EntityKind, ErasedEntityRef, FlatEntityRef},
    },
};

pub(crate) fn print_deps_inner(
    module_name: &str,
    info: &Module,
    reachable: &DepSet,
    graph: &DepGraph,
) {
    let size_fn = |entity: &EntityKind| match entity {
        EntityKind::DataSymbol(d) => info.data.get(*d).unwrap().data.len(),
        EntityKind::Function(input_id) => {
            if let Some(defined) = info.functions.items.get_entity(*input_id).to_defined() {
                defined.body.len()
            } else {
                0
            }
        }
        _ => unreachable!(),
    };
    let format_dep = |dep: FlatEntityRef| {
        let symbol_kind = graph.snapshot().unpack_ref(dep);

        let name = info
            .get_name(symbol_kind)
            .map(crate::helpers::demangle_full)
            .unwrap_or("<unknown>".to_string());
        match symbol_kind {
            EntityKind::Function(input_id) => {
                format!(
                    "{dep} func[{input_id}] <{name:?}> (size={})",
                    size_fn(&symbol_kind),
                    input_id = input_id.as_u32()
                )
            }
            EntityKind::DataSymbol(data_ref) => {
                let data = info.data.get(data_ref).unwrap();
                format!(
                    "{dep} data[{segment_id}:{start}..{end}]  <{name:?}> (size={})",
                    size_fn(&symbol_kind),
                    start = data.original_offset,
                    segment_id = info.data[data_ref].segment_id.as_u32(),
                    end = data.original_offset + data.data.len()
                )
            }
            _ => unreachable!(),
        }
    };

    println!("SPLIT: ============== {module_name}");
    for (node, children) in graph.iter_childs() {
        if !reachable.contains(&node) {
            continue;
        }

        println!("---{}---", format_dep(node));
        for parent in graph.get_parents(node).into_iter().flatten() {
            println!("<=={} (parent)", format_dep(*parent));
        }
        println!("-------------");
        for child in children {
            println!("==>{}", format_dep(*child));
        }
    }

    let mut total_size: usize = 0;
    for r in reachable.iter() {
        let symbol = graph.snapshot().unpack_ref(*r);
        total_size += size_fn(&symbol);
    }
    println!("SPLIT: ============== {module_name} : total size: {total_size}");
}

/// Format a DepGraph into a human-readable string for snapshot testing
pub fn format_dep_graph(graph: &DepGraph, info: &Module) -> String {
    use std::fmt::Write;

    let mut output = String::new();
    let size_fn = |entity: &EntityKind| match entity {
        EntityKind::DataSymbol(d) => info.data.get(*d).unwrap().data.len(),
        EntityKind::Function(input_id) => {
            if let Some(defined) = info.functions.items.get_entity(*input_id).to_defined() {
                defined.body.len()
            } else {
                0
            }
        }
        _ => unreachable!(),
    };
    let format_symbol = |dep: FlatEntityRef| {
        let symbol_kind = graph.snapshot().unpack_ref(dep);

        let name = info.get_name(symbol_kind).unwrap();
        let name = crate::helpers::demangle_full(&name);
        match symbol_kind {
            EntityKind::Function(input_id) => {
                format!(
                    "{dep} func[{input_id}] <{name:?}> (size={})",
                    size_fn(&symbol_kind),
                    input_id = input_id.as_u32()
                )
            }
            EntityKind::DataSymbol(data_ref) => {
                let data = info.data.get(data_ref).unwrap();
                format!(
                    "{dep} data[{segment_id}:{start}..{end}]  <{name:?}> (size={})",
                    size_fn(&symbol_kind),
                    start = data.original_offset,
                    segment_id = info.data[data_ref].segment_id.as_u32(),
                    end = data.original_offset + data.data.len()
                )
            }
            _ => format!("{dep} <{name}>"),
        }
    };

    for (node, _) in graph.iter_childs() {
        if let Some(children) = graph.get_children(node) {
            writeln!(&mut output, "{}", format_symbol(node)).unwrap();

            if let Some(parents) = graph.get_parents(node) {
                let parent_list: Vec<_> = parents.iter().copied().collect();
                for parent in parent_list {
                    writeln!(&mut output, "  <- {}", format_symbol(parent)).unwrap();
                }
            }

            let child_list: Vec<_> = children.iter().copied().collect();
            for child in child_list {
                writeln!(&mut output, "  -> {}", format_symbol(child)).unwrap();
            }
            writeln!(&mut output).unwrap();
        }
    }

    output
}
