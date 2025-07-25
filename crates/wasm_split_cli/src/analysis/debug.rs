use std::collections::HashSet;
use std::fmt::Debug;

use crate::analysis;
use crate::analysis::dep_graph::{DepNode, ReachabilityGraph};
use crate::analysis::split_point::OutputModuleInfo;
use crate::helpers::debug_fmt_mostly_filled;
use crate::read::InputModule;

impl ReachabilityGraph {
    pub fn print(&self, module_name: &str, info: &analysis::ModuleInfo) {
        Self::print_deps_inner(module_name, info, &self.reachable);
    }
    pub(crate) fn print_deps_inner(
        module_name: &str,
        info: &analysis::ModuleInfo,
        reachable: &HashSet<DepNode>,
    ) {
        let format_dep = |dep: &DepNode| match dep {
            DepNode::Function(index) => {
                let name = info.source.names.functions.get(*index);
                format!("func[{index}] <{name:?}>")
            }
            DepNode::DataSymbol(segment, idx) => {
                let symbol = info
                    .source
                    .linking
                    .get_data_in_segment(*segment, *idx)
                    .expect("indexes should be valid")
                    .name;
                let segment = info.source.names.data_segments[segment];
                format!("data[{segment}:{idx}] <{symbol:?}>")
            }
        };

        println!("SPLIT: ============== {module_name}");
        let mut total_size: usize = 0;
        for dep in reachable.iter() {
            let size = match dep {
                DepNode::Function(index) => {
                    let size = index
                        .checked_sub(info.import_funcs_info.imported_funcs.len())
                        .map(|defined_index| {
                            info.source.code.section_payload.defined_funcs[defined_index]
                                .body
                                .range()
                                .len()
                        })
                        .unwrap_or_default();
                    size
                }
                DepNode::DataSymbol(segment, idx) => {
                    info.source.linking.linking_symbols.data_in_segments[*segment][*idx].size
                        as usize
                }
            };

            total_size += size;

            println!("   {} size={size:?}", format_dep(dep));
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
                &debug_fmt_mostly_filled(&shared_fns, 4, 15, "...", |a, b| a + 1 != *b),
            )
            .field(
                "shared_datas",
                &debug_fmt_mostly_filled(&shared_datas, 3, 7, "...", |a, b| a.1 + 1 != b.1),
            )
            .field(
                "included_fns",
                &debug_fmt_mostly_filled(&included_fns, 4, 15, "...", |a, b| a + 1 != *b),
            )
            .field(
                "included_datas",
                &debug_fmt_mostly_filled(&included_datas, 3, 7, "...", |a, b| a.1 + 1 != b.1),
            )
            .field("split_points", &self.split_points)
            .finish()
    }
}

impl OutputModuleInfo {
    pub fn print(&self, module_name: &str, info: &analysis::ModuleInfo) {
        ReachabilityGraph::print_deps_inner(module_name, info, &self.included_symbols);
    }
}
