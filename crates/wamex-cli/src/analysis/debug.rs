use std::fmt::Debug;

use crate::{
    analysis::{
        self,
        dep_graph::{DepGraph, DepList, DepNode},
        split_point::OutputModuleInfo,
    },
    helpers::debug_fmt_mostly_filled,
    index::DefinedFuncId,
};

pub(crate) fn print_deps_inner(
    module_name: &str,
    info: &analysis::ModuleInfo,
    reachable: &DepList,
    graph: &DepGraph,
) {
    let size_fn = |dep: &DepNode| match dep {
        DepNode::Function(index) => {
            let size = index
                .as_raw_index()
                .checked_sub(info.import_info.imported_funcs.len())
                .map(|defined_index| {
                    info.wasm.code.section_payload.defined_funcs
                        [DefinedFuncId::from_index(defined_index)]
                    .body
                    .range()
                    .len()
                })
                .unwrap_or_default();
            size
        }
        DepNode::DataSymbol(segment, idx) => {
            info.wasm.linking.linking_symbols.data_in_segments[*segment][*idx].size as usize
        }
    };

    let format_dep = |dep: &DepNode| match dep {
        DepNode::Function(index) => {
            let name = info
                .wasm
                .names
                .functions
                .get(*index)
                .map(|n| crate::helpers::demangle_full(n));
            format!("func[{index}] <{name:?}> (size={})", size_fn(dep))
        }
        DepNode::DataSymbol(segment, idx) => {
            let symbol = crate::helpers::demangle_full(
                info.wasm
                    .linking
                    .get_data_in_segment(*segment, *idx)
                    .expect("indexes should be valid")
                    .name,
            );
            let segment_name = info.wasm.names.data_segments[*segment];
            format!(
                "data[{segment}:{idx}] {segment_name}<{symbol:?}> (size={})",
                size_fn(dep)
            )
        }
    };

    println!("SPLIT: ============== {module_name}");
    for (node, children) in graph.iter_childs() {
        if !reachable.contains(&node) {
            continue;
        }

        println!("---{}---", format_dep(node));
        for parent in graph.get_parents(node).into_iter().flatten() {
            println!("<=={} (parent)", format_dep(parent));
        }
        println!("-------------");
        for child in children {
            println!("==>{}", format_dep(child));
        }
    }

    let mut total_size: usize = 0;
    for r in reachable.iter() {
        total_size += size_fn(r);
    }
    println!("SPLIT: ============== {module_name} : total size: {total_size}");
}

impl Debug for OutputModuleInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut included_fns = self
            .defined_symbols
            .iter()
            .filter_map(|dep| match dep {
                DepNode::Function(func_id) => Some(*func_id),
                _ => None,
            })
            .collect::<Vec<_>>();
        included_fns.sort_unstable();

        let mut included_datas = self
            .defined_symbols
            .iter()
            .filter_map(|dep| match dep {
                DepNode::DataSymbol(segment, data_id) => Some((*segment, *data_id)),
                _ => None,
            })
            .collect::<Vec<_>>();
        included_datas.sort_unstable();

        let mut imported_fns = self
            .imports
            .iter()
            .filter_map(|dep| match dep {
                DepNode::Function(func_id) => Some(*func_id),
                _ => None,
            })
            .collect::<Vec<_>>();

        let mut imported_datas = self
            .imports
            .iter()
            .filter_map(|dep| match dep {
                DepNode::DataSymbol(segment, data_id) => Some((*segment, *data_id)),
                _ => None,
            })
            .collect::<Vec<_>>();

        let mut exported_fns = self
            .exports
            .iter()
            .filter_map(|dep| match dep {
                DepNode::Function(func_id) => Some(*func_id),
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut exported_datas = self
            .exports
            .iter()
            .filter_map(|dep| match dep {
                DepNode::DataSymbol(segment, data_id) => Some((*segment, *data_id)),
                _ => None,
            })
            .collect::<Vec<_>>();

        imported_fns.sort_unstable();
        imported_datas.sort_unstable();
        exported_fns.sort_unstable();
        exported_datas.sort_unstable();

        f.debug_struct("OutputModuleInfo")
            .field(
                "imported_fns",
                &debug_fmt_mostly_filled(&imported_fns, 4, 15, "...", |a, b| a.next() != *b),
            )
            .field(
                "imported_datas",
                &debug_fmt_mostly_filled(&imported_datas, 3, 7, "...", |a, b| a.1.next() != b.1),
            )
            .field(
                "exported_fns",
                &debug_fmt_mostly_filled(&exported_fns, 4, 15, "...", |a, b| a.next() != *b),
            )
            .field(
                "exported_datas",
                &debug_fmt_mostly_filled(&exported_datas, 3, 7, "...", |a, b| a.1.next() != b.1),
            )
            .field(
                "included_fns",
                &debug_fmt_mostly_filled(&included_fns, 4, 15, "...", |a, b| a.next() != *b),
            )
            .field(
                "included_datas",
                &debug_fmt_mostly_filled(&included_datas, 3, 7, "...", |a, b| a.1.next() != b.1),
            )
            .finish()
    }
}

impl OutputModuleInfo {
    pub fn print(&self, module_name: &str, info: &analysis::ModuleInfo, graph: &DepGraph) {
        print_deps_inner(module_name, info, &self.defined_symbols, &graph);
    }
}
