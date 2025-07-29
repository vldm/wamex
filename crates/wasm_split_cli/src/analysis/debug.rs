use std::collections::{HashMap, HashSet};
use std::fmt::Debug;

use crate::analysis;
use crate::analysis::dep_graph::{DepGraph, DepNode, ReachabilityGraph};
use crate::analysis::split_point::OutputModuleInfo;
use crate::helpers::debug_fmt_mostly_filled;
use crate::index::DefinedFuncId;

impl ReachabilityGraph {
    pub fn print(&self, module_name: &str, info: &analysis::ModuleInfo) {
        Self::print_deps_inner(module_name, info, &self.reachable, &self.parents);
    }
    pub(crate) fn print_deps_inner(
        module_name: &str,
        info: &analysis::ModuleInfo,
        reachable: &HashSet<DepNode>,
        parents: &DepGraph,
    ) {
        let size_fn = |dep: &DepNode| match dep {
            DepNode::Function(index) => {
                let size = index
                    .as_raw_index()
                    .checked_sub(info.import_funcs_info.imported_funcs.len())
                    .map(|defined_index| {
                        info.source.code.section_payload.defined_funcs
                            [DefinedFuncId::from_index(defined_index)]
                        .body
                        .range()
                        .len()
                    })
                    .unwrap_or_default();
                size
            }
            DepNode::DataSymbol(segment, idx) => {
                info.source.linking.linking_symbols.data_in_segments[*segment][*idx].size as usize
            }
        };

        let format_dep = |dep: &DepNode| match dep {
            DepNode::Function(index) => {
                let name = info.source.names.functions.get(*index);
                format!("func[{index}] <{name:?}> (size={})", size_fn(dep))
            }
            DepNode::DataSymbol(segment, idx) => {
                let symbol = info
                    .source
                    .linking
                    .get_data_in_segment(*segment, *idx)
                    .expect("indexes should be valid")
                    .name;
                let segment_name = info.source.names.data_segments[*segment];
                format!(
                    "data[{segment}:{idx}] {segment_name}<{symbol:?}> (size={})",
                    size_fn(dep)
                )
            }
        };
        let mut rev_tree = HashMap::new();
        for child in reachable.iter() {
            for parent in parents
                .get(child)
                .into_iter()
                .flatten()
                .filter(|p| reachable.contains(p))
            // important when parents is full tree
            {
                rev_tree.entry(parent).or_insert_with(Vec::new).push(child);
            }
        }

        println!("SPLIT: ============== {module_name}");
        for (parent, children) in rev_tree.iter() {
            println!("{}", format_dep(parent));

            for child in children {
                println!("==>{}", format_dep(child));
            }
        }

        let mut total_size: usize = 0;
        for r in reachable.iter() {
            total_size += size_fn(r);
        }
        println!("SPLIT: ============== {module_name}  : total size: {total_size}");
    }
}

impl Debug for OutputModuleInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut included_fns = self
            .included_symbols
            .iter()
            .filter_map(|dep| match dep {
                DepNode::Function(func_id) => Some(*func_id),
                _ => None,
            })
            .collect::<Vec<_>>();
        included_fns.sort_unstable();

        let mut included_datas = self
            .included_symbols
            .iter()
            .filter_map(|dep| match dep {
                DepNode::DataSymbol(segment, data_id) => Some((*segment, *data_id)),
                _ => None,
            })
            .collect::<Vec<_>>();
        included_datas.sort_unstable();

        let mut shared_fns = self
            .link_symbols
            .iter()
            .filter_map(|dep| match dep {
                DepNode::Function(func_id) => Some(*func_id),
                _ => None,
            })
            .collect::<Vec<_>>();
        shared_fns.sort_unstable();

        let mut shared_datas = self
            .link_symbols
            .iter()
            .filter_map(|dep| match dep {
                DepNode::DataSymbol(segment, data_id) => Some((*segment, *data_id)),
                _ => None,
            })
            .collect::<Vec<_>>();
        shared_datas.sort_unstable();

        f.debug_struct("OutputModuleInfo")
            .field(
                "shared_fns",
                &debug_fmt_mostly_filled(&shared_fns, 4, 15, "...", |a, b| a.next() != *b),
            )
            .field(
                "shared_datas",
                &debug_fmt_mostly_filled(&shared_datas, 3, 7, "...", |a, b| a.1.next() != b.1),
            )
            .field(
                "included_fns",
                &debug_fmt_mostly_filled(&included_fns, 4, 15, "...", |a, b| a.next() != *b),
            )
            .field(
                "included_datas",
                &debug_fmt_mostly_filled(&included_datas, 3, 7, "...", |a, b| a.1.next() != b.1),
            )
            .field("split_points", &self.split_points)
            .finish()
    }
}

impl OutputModuleInfo {
    pub fn print(&self, module_name: &str, info: &analysis::ModuleInfo, graph: &DepGraph) {
        let parents = crate::analysis::dep_graph::NamedGraph::<()>::reverse(graph);
        ReachabilityGraph::print_deps_inner(module_name, info, &self.included_symbols, &parents);
    }
}
