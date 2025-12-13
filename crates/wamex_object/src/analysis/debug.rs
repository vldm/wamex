use std::fmt::Debug;

use crate::{
    analysis::{
        self,
        dep_graph::{DepGraph, DepSet},
        split_point::OutputModuleInfo,
        symbols::{SymbolKind, SymbolRecord},
    },
    helpers::debug_fmt_mostly_filled,
    index::SymbolId,
};

pub(crate) fn print_deps_inner(
    module_name: &str,
    info: &analysis::ModuleInfo,
    reachable: &DepSet,
    graph: &DepGraph,
) {
    let size_fn = |symbol: &SymbolRecord| match symbol.kind {
        SymbolKind::DataDefined { length, .. } => length,
        SymbolKind::Func { input_id } => {
            if let Some(defined_id) = info.as_defined_function_id(input_id) {
                info.wasm.code.defined_funcs[defined_id].body.range().len()
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
                    size_fn(symbol)
                )
            }
            SymbolKind::DataDefined {
                segment_id,
                offset,
                length,
                ..
            } => {
                let segment_name = info
                    .wasm
                    .names
                    .data_segments
                    .get(segment_id)
                    .cloned()
                    .unwrap_or_default();
                format!(
                    "{dep} data[{segment_name}({segment_id}):{start}..{end}]  <{name:?}> (size={})",
                    size_fn(symbol),
                    start = offset,
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

impl Debug for OutputModuleInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut defined_symbols = self.defined_symbols.iter().copied().collect::<Vec<_>>();

        let mut imported_symbols = self.imports.iter().copied().collect::<Vec<_>>();

        let mut exported_symbols = self.exports.iter().copied().collect::<Vec<_>>();

        defined_symbols.sort_unstable();
        imported_symbols.sort_unstable();
        exported_symbols.sort_unstable();

        f.debug_struct("OutputModuleInfo")
            .field(
                "imported_symbols",
                &debug_fmt_mostly_filled(&imported_symbols, 3, 7, "...", |a, b| a.next() != *b),
            )
            .field(
                "exported_symbols",
                &debug_fmt_mostly_filled(&exported_symbols, 3, 7, "...", |a, b| a.next() != *b),
            )
            .field(
                "defined_symbols",
                &debug_fmt_mostly_filled(&defined_symbols, 3, 7, "...", |a, b| a.next() != *b),
            )
            .finish()
    }
}

impl OutputModuleInfo {
    pub fn print(&self, module_name: &str, info: &analysis::ModuleInfo, graph: &DepGraph) {
        print_deps_inner(module_name, info, &self.defined_symbols, graph);
    }
}
