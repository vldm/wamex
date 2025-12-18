use wamex_object::InputObject;

use crate::{
    analysis::{
        dep_graph::{DepGraph, DepSet},
        symbols::{SymbolKind, SymbolRecord},
    },
    index::SymbolId,
};

pub(crate) fn print_deps_inner(
    module_name: &str,
    info: &InputObject,
    reachable: &DepSet,
    graph: &DepGraph,
) {
    let size_fn = |symbol: &SymbolRecord| match symbol.kind {
        SymbolKind::DataDefined { length, .. } => length,
        SymbolKind::Func { input_id } => {
            if let Some(defined_id) = info.as_defined_function_id(input_id) {
                info.wasm_reader.code.defined_funcs[defined_id].body.range().len()
            } else {
                0
            }
        }
        _ => unreachable!(),
    };
    let format_dep = |dep: SymbolId| {
        let symbol = info.symbols.get(dep).expect("dep should be valid");
        let name = crate::helpers::demangle_full(&symbol.name);
        match symbol.kind {
            SymbolKind::Func { input_id } => {
                format!(
                    "{dep} func[{input_id}] <{name:?}> (size={})",
                    size_fn(symbol),
                    input_id = input_id.as_u32()
                )
            }
            SymbolKind::DataDefined {
                segment_id,
                offset,
                length,
                ..
            } => {
                let segment_name = info
                    .wasm_reader
                    .names
                    .data_segments
                    .get(segment_id)
                    .cloned()
                    .unwrap_or_default();
                format!(
                    "{dep} data[{segment_name}({segment_id}):{start}..{end}]  <{name:?}> (size={})",
                    size_fn(symbol),
                    start = offset,
                    segment_id = segment_id.as_u32(),
                    end = offset + length
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
        let symbol = info.symbols.get(*r).expect("dep should be valid");
        total_size += size_fn(symbol);
    }
    println!("SPLIT: ============== {module_name} : total size: {total_size}");
}

/// Format a DepGraph into a human-readable string for snapshot testing
pub fn format_dep_graph(graph: &DepGraph, info: &InputObject) -> String {
    use std::fmt::Write;

    let mut output = String::new();

    let format_symbol = |id: SymbolId| -> String {
        let symbol = info.symbols.get(id).expect("symbol should exist");
        let name = crate::helpers::demangle_full(&symbol.name);
        match symbol.kind {
            SymbolKind::Func { input_id } => {
                format!(
                    "{id:?} func[{input_id}] <{name}>",
                    input_id = input_id.as_u32()
                )
            }
            SymbolKind::DataDefined {
                segment_id,
                offset,
                length,
            } => {
                format!(
                    "{id:?} data[{segment_id}:{offset}+{length}] <{name}>",
                    segment_id = segment_id.as_u32()
                )
            }
            _ => format!("{id:?} <{name}>"),
        }
    };

    let mut nodes: Vec<_> = graph.iter_childs().map(|(id, _)| id).collect();
    nodes.sort();

    for node in nodes {
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

/// Format a SplitProgramInfo into a human-readable string for snapshot testing
pub fn format_split_program_info(
    split_info: &wamex_object::emit::split::SplitProgramInfo,
    info: &InputObject,
) -> String {
    use std::fmt::Write;

    let mut output = String::new();

    let format_symbol = |id: SymbolId| -> String {
        let symbol = info.symbols.get(id).expect("symbol should exist");
        let name = crate::helpers::demangle_full(&symbol.name);
        format!("{id:?} <{name}>")
    };

    writeln!(&mut output, "=== Split Program Structure ===\n").unwrap();

    for (idx, (module_id, module_info)) in split_info.output_modules.iter().enumerate() {
        writeln!(&mut output, "Module #{idx}: {module_id:?}").unwrap();
        writeln!(
            &mut output,
            "  Defined symbols: {}",
            module_info.defined_symbols.len()
        )
        .unwrap();

        let mut symbols: Vec<_> = module_info.defined_symbols.iter().copied().collect();
        symbols.sort();
        for symbol_id in symbols {
            writeln!(&mut output, "    {}", format_symbol(symbol_id)).unwrap();
        }

        if !module_info.imports.is_empty() {
            writeln!(&mut output, "  Imports: {}", module_info.imports.len()).unwrap();
            let mut imports: Vec<_> = module_info.imports.iter().copied().collect();
            imports.sort();
            for import_id in imports {
                writeln!(&mut output, "    {}", format_symbol(import_id)).unwrap();
            }
        }

        if !module_info.exports.is_empty() {
            writeln!(&mut output, "  Exports: {}", module_info.exports.len()).unwrap();
            let mut exports: Vec<_> = module_info.exports.iter().copied().collect();
            exports.sort();
            for export_id in exports {
                writeln!(&mut output, "    {}", format_symbol(export_id)).unwrap();
            }
        }

        if !module_info.split_points.is_empty() {
            writeln!(
                &mut output,
                "  Split points: {}",
                module_info.split_points.len()
            )
            .unwrap();
            for sp in &module_info.split_points {
                writeln!(&mut output, "    {:?}", sp.unique_id).unwrap();
            }
        }

        writeln!(&mut output).unwrap();
    }

    output
}

/// Format a SymbolMap into a human-readable string for snapshot testing
pub fn format_symbol_map(info: &InputObject) -> String {
    use std::fmt::Write;

    let mut output = String::new();

    writeln!(&mut output, "=== Symbol Map ===\n").unwrap();

    let mut symbols: Vec<_> = info.symbols.iter().collect();
    symbols.sort_by_key(|(id, _)| *id);

    for (id, symbol) in symbols {
        let name = crate::helpers::demangle_full(&symbol.name);

        match symbol.kind {
            SymbolKind::Func { input_id } => {
                let size = if let Some(defined_id) = info.as_defined_function_id(input_id) {
                    info.wasm_reader.code.defined_funcs[defined_id].body.range().len()
                } else {
                    0
                };
                writeln!(
                    &mut output,
                    "{id:?} func[{input_id}] <{name}> size={size}",
                    input_id = input_id.as_u32()
                )
                .unwrap();
            }
            SymbolKind::DataDefined {
                segment_id,
                offset,
                length,
            } => {
                let segment_name = info
                    .wasm_reader
                    .names
                    .data_segments
                    .get(segment_id)
                    .map(|s| s.as_ref())
                    .unwrap_or("unknown");
                writeln!(
                    &mut output,
                    "{id:?} data[{segment_name}({segment_id}):{offset}+{length}] <{name}>",
                    segment_id = segment_id.as_u32()
                )
                .unwrap();
            }
            SymbolKind::Global(global_id) => {
                writeln!(
                    &mut output,
                    "{id:?} global[{global_id}] <{name}>",
                    global_id = global_id.as_u32()
                )
                .unwrap();
            }
            SymbolKind::Table(table_id) => {
                writeln!(
                    &mut output,
                    "{id:?} table[{table_id}] <{name}>",
                    table_id = table_id.as_u32()
                )
                .unwrap();
            }
            SymbolKind::Duplicate(original_id) => {
                writeln!(&mut output, "{id:?} duplicate -> {original_id:?} <{name}>").unwrap();
            }
        }

        if !symbol.relocs.is_empty() {
            writeln!(&mut output, "  Relocations: {}", symbol.relocs.len()).unwrap();
            for reloc in &symbol.relocs {
                let target_symbol = info
                    .symbols
                    .get(crate::index::SymbolId::from_index(reloc.index));
                let target_name = target_symbol
                    .map(|s| crate::helpers::demangle_full(&s.name))
                    .unwrap_or_else(|| format!("unknown_{}", reloc.index));
                writeln!(
                    &mut output,
                    "    {:?} @ offset {} -> {target_name}",
                    reloc.ty, reloc.offset
                )
                .unwrap();
            }
        }
    }

    output
}
