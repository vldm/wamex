use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt::Debug,
    ops::Range,
};

use anyhow::Context;
use wasmparser::RelocationType;

use crate::{analysis, helpers::ShiftRange, index::DataSegmentId, read::InputModule};
use crate::{
    index::{InputFuncId, SymbolIndex},
    read::linking::SymbolType,
};

#[derive(Debug, PartialEq, Eq, Hash, Copy, PartialOrd, Ord, Clone)]
pub enum DepNode {
    Function(InputFuncId),
    DataSymbol(DataSegmentId, SymbolIndex),
}

impl DepNode {
    pub fn as_function(&self) -> Option<InputFuncId> {
        match self {
            DepNode::Function(id) => Some(*id),
            _ => None,
        }
    }
    pub fn as_data_symbol(&self) -> Option<(DataSegmentId, SymbolIndex)> {
        match self {
            DepNode::DataSymbol(segment_id, symbol_index) => Some((*segment_id, *symbol_index)),
            _ => None,
        }
    }
}

pub type DepGraph = HashMap<DepNode, HashSet<DepNode>>;

#[derive(Debug, Clone, Default)]
pub struct ReachabilityGraph {
    pub reachable: HashSet<DepNode>,
    pub parents: DepGraph,
}

#[derive(Debug, Clone)]
pub struct NamedGraph<Id> {
    pub module: Id,
    pub deps: ReachabilityGraph,
}

#[derive(Debug, Clone)]
pub struct SharedEntries<Id> {
    pub module_names: Vec<Id>,
    pub shared_deps: HashSet<DepNode>,
    pub topmost_shared: HashSet<DepNode>,
}

pub trait SymbolTable {
    fn get_symbol_dep_node(&self, symbol_index: SymbolIndex) -> Option<DepNode>;
}

impl<'a> SymbolTable for InputModule<'a> {
    fn get_symbol_dep_node(&self, linking_index: SymbolIndex) -> Option<DepNode> {
        let (idx, sym_type) = self.linking.linking_symbols.original_indexes[linking_index];
        match sym_type {
            SymbolType::Func => Some(DepNode::Function(idx as InputFuncId)),
            SymbolType::DataDefined(segment_id) => {
                Some(DepNode::DataSymbol(segment_id, idx as InputFuncId))
            }
            SymbolType::DataUndefined => {
                log::error!(
                    "{idx} {} is undefined data symbol, dont have any location in data segment, what can we do with it?",
                    self.linking.linking_symbols.undefined_data[idx].name
                );
                None
            }
            _ => {
                //println!("{v:?} is not supported symol dep");
                None
            }
        }
    }
}

pub fn get_dependencies(
    module: &InputModule,
    info: &analysis::ModuleInfo,
) -> anyhow::Result<DepGraph> {
    let mut deps = DepGraph::new();
    let mut add_dep = |a: DepNode, linking_index: u32| {
        if let Some(target) = module.get_symbol_dep_node(linking_index as usize) {
            log::trace!("Adding dirrect deps {a:?} -> {target:?}");
            deps.entry(a).or_default().insert(target);
        };
    };

    if let Some(relocs) = module.relocs.get_section(module.code.section_index) {
        for entry in &relocs.entries {
            let func_index = find_function_containing_range(
                info,
                entry
                    .relocation_range()
                    .shift_right(module.code.starting_offset),
            )
            .with_context(|| format!("Invalid code relocation entry {entry:?}"))?;

            match entry.ty {
                RelocationType::TypeIndexLeb => {
                    continue;
                } //TODO: refine rest?
                _ => {}
            }
            add_dep(DepNode::Function(func_index), entry.index);
        }
    }

    if let Some(relocs) = module.relocs.get_section(module.data.section_index) {
        for entry in &relocs.entries {
            let (segment_index, symbol_index) = find_data_symbol_containing_range(
                info,
                entry
                    .relocation_range()
                    .shift_right(module.data.starting_offset),
            )
            .with_context(|| format!("Invalid data relocation entry {entry:?}"))?;
            add_dep(
                DepNode::DataSymbol(segment_index, symbol_index),
                entry.index,
            );
        }
    }
    Ok(deps)
}

impl ReachabilityGraph {
    // traverse the dep graph starting from roots and return all reachable nodes
    pub fn find_reachable_deps(deps: &DepGraph, roots: &HashSet<DepNode>) -> ReachabilityGraph {
        let mut queue: VecDeque<DepNode> = roots.iter().copied().collect();
        let mut seen = HashSet::<DepNode>::new();

        let mut parents = DepGraph::new();
        while let Some(node) = queue.pop_front() {
            // println!("queue node: {node:?}");
            seen.insert(node);
            let Some(children) = deps.get(&node) else {
                continue;
            };
            for child in children {
                if seen.contains(&child) {
                    continue;
                }
                parents.entry(*child).or_default().insert(node);
                queue.push_back(*child);
            }
        }
        ReachabilityGraph {
            reachable: seen,
            parents,
        }
    }

    pub fn remove_node(&mut self, node: &DepNode, graph: &DepGraph) {
        let mut queue = VecDeque::from([*node]);

        let mut seen = HashSet::<DepNode>::new();
        while let Some(node) = queue.pop_front() {
            seen.insert(node);

            // Can be unreachable if cycle in graph
            let _reachable = self.reachable.remove(&node);

            if let Some(childs) = graph.get(&node) {
                for child in childs {
                    let child_parent = self.parents.get_mut(&node).expect("Parent should exist");
                    child_parent.remove(child);
                    if child_parent.is_empty() {
                        if seen.contains(child) {
                            continue;
                        }
                        queue.push_back(*child);
                    }
                }
            }
        }
    }

    fn check_unreachable(&self, deps: &HashSet<DepNode>) -> bool {
        for dep in deps {
            if self.reachable.contains(dep) {
                log::warn!("Unreachable dep {dep:?} found in deps");
                return false;
            }
        }
        true
    }
}

impl<Id> NamedGraph<Id> {
    /// Collect list of modules that owns a given dep node
    /// Returns a map of dep node to set of module ids that owns it
    fn collect_visited_by(modules: &[NamedGraph<Id>]) -> HashMap<DepNode, HashSet<usize>> {
        let mut visited_by: HashMap<DepNode, HashSet<usize>> = HashMap::new();
        for (module_id, module) in modules.iter().enumerate() {
            for dep in module.deps.reachable.iter() {
                visited_by.entry(*dep).or_default().insert(module_id);
            }
        }
        visited_by
    }

    /// Remove all entries which all parents are also in shared entries.
    pub fn reduce_shared_entries(
        shared_entries: HashSet<DepNode>,
        parents: &DepGraph,
    ) -> HashSet<DepNode> {
        let mut reduced = HashSet::new();
        for dep in &shared_entries {
            if let Some(parent) = parents.get(dep) {
                if parent.iter().all(|p| shared_entries.contains(p)) {
                    continue; // skip if all parents are also in shared entries
                }
            }
            reduced.insert(*dep);
        }
        reduced
    }

    /// Build a reverse graph from the given dep graph.
    /// parent -> child becomes child -> parent
    ///
    /// This is usefull for finding all parents of a given node.
    pub fn reverse(graph: &DepGraph) -> DepGraph {
        let mut reversed = DepGraph::new();
        for (node, deps) in graph.iter() {
            for dep in deps {
                reversed.entry(*dep).or_default().insert(*node);
            }
        }
        reversed
    }

    /// Calculate shared entries between modules.
    /// Returns a vector of SharedEntries, each containing a list of module names and shared dependencies.
    /// Shared dependencies are those that are reachable from multiple modules.
    pub fn calculate_shared_modules(
        modules: &mut [NamedGraph<Id>],
        graph: &DepGraph,
    ) -> Vec<SharedEntries<Id>>
    where
        Id: Clone + Ord,
    {
        let mut shared_entries: HashMap<Vec<usize>, HashSet<DepNode>> = HashMap::new();

        let parents = Self::reverse(graph);
        let visited_by = Self::collect_visited_by(modules);

        for (dep, owner_modules) in visited_by {
            if owner_modules.len() > 1 {
                for module_id in &owner_modules {
                    let module = &mut modules[*module_id];

                    module.deps.remove_node(&dep, graph);
                }
                let mut owner_modules: Vec<usize> = owner_modules.into_iter().collect();
                owner_modules.sort_unstable();
                shared_entries.entry(owner_modules).or_default().insert(dep);
            }
        }
        let mut res = shared_entries
            .into_iter()
            .map(|(module_names, shared_deps)| {
                let reduced_shared_deps =
                    Self::reduce_shared_entries(shared_deps.clone(), &parents);
                SharedEntries {
                    module_names: module_names
                        .into_iter()
                        .map(|id| modules[id].module.clone())
                        .collect(),
                    shared_deps,
                    topmost_shared: reduced_shared_deps,
                }
            })
            .collect::<Vec<_>>();
        res.sort_by(|left, right| left.module_names.cmp(&right.module_names));
        res
    }
}

fn find_function_containing_range(
    info: &analysis::ModuleInfo,
    range: Range<usize>,
) -> anyhow::Result<usize> {
    info.find_function_id_containing_range(range)
}

fn find_data_symbol_containing_range(
    info: &analysis::ModuleInfo,
    range: Range<usize>,
) -> anyhow::Result<(usize, usize)> {
    let sym = &info.find_data_symbol_containing_range(range)?;
    Ok((sym.segment_index, sym.symbol_index))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use lazy_static::lazy_static;

    use crate::{
        analysis::{
            self,
            dep_graph::{DepGraph, DepNode},
            testing,
        },
        read,
    };

    // checkout test-data/simple-graph crate at root (just keep wasm in case rustc changes)
    const WASM_FILE: &[u8] = include_bytes!("../../test-data/simple_graph.wasm");

    #[test]
    fn load_dep_graph() {
        let module = read::InputModule::parse(&WASM_FILE).unwrap();
        let info = analysis::ModuleInfo::new(&module).unwrap();
        let dep_graph = super::get_dependencies(&module, &info).unwrap();

        let format_dep = |dep: &DepNode| match dep {
            DepNode::Function(index) => {
                let name = module.names.functions.get(*index);
                format!("func[{index}] <{name:?}>")
            }
            DepNode::DataSymbol(segment, idx) => {
                let symbol = module.linking.linking_symbols.data_in_segments[segment][*idx].name;
                let segment = module.names.data_segments[segment];
                format!("data[{segment}:{idx}] <{symbol:?}>")
            }
        };

        for (node, deps) in dep_graph.iter() {
            println!("node: {node}", node = format_dep(node));
            for dep in deps {
                println!("  =>{dep}", dep = format_dep(dep));
            }
        }

        let no_inline_fn = info.find_function_id_by_name("no_inline_fn").unwrap();

        let deps = dep_graph.get(&DepNode::Function(no_inline_fn)).unwrap();
        let func_deps: Vec<_> = deps
            .iter()
            .filter(|dep| matches!(dep, DepNode::Function(_)))
            .collect();
        let data_deps: Vec<_> = deps
            .iter()
            .filter(|dep| matches!(dep, DepNode::DataSymbol(..)))
            .collect();

        assert_eq!(func_deps.len(), 3);
        // no_inline_fn is really inline data, but keep method call for "side effect"
        assert_eq!(data_deps.len(), 3);

        let indirrect_fn = info.find_function_id_by_name("indirrect_fn").unwrap();

        let deps = dep_graph.get(&DepNode::Function(indirrect_fn)).unwrap();
        assert_eq!(deps.len(), 1); // only dep on switchtable
        let swith_table = deps.iter().next().unwrap();
        assert!(matches!(swith_table, DepNode::DataSymbol(..)));
        let fns = dep_graph.get(swith_table).unwrap();

        assert_eq!(fns.len(), 3);
    }

    #[test]
    fn reachablity_graph() {
        let module = read::InputModule::parse(&WASM_FILE).unwrap();
        let info = analysis::ModuleInfo::new(&module).unwrap();
        let dep_graph = super::get_dependencies(&module, &info).unwrap();

        let no_inline_fn = info.find_function_id_by_name("no_inline_fn").unwrap();

        let reachability_graph = super::ReachabilityGraph::find_reachable_deps(
            &dep_graph,
            &HashSet::from([DepNode::Function(no_inline_fn)]),
        );
        // no_inline_fn -> data1
        //              -> data2
        //              -> data3
        //              -> func1 -> data1
        //              -> func2 -> data2
        //              -> func3 -> data3
        reachability_graph.print("no_inline_fn", &info);
        assert_eq!(reachability_graph.reachable.len(), 7); // root +  3 data + 3 funcs

        let indirrect_fn = info.find_function_id_by_name("indirrect_fn").unwrap();
        let reachability_graph = super::ReachabilityGraph::find_reachable_deps(
            &dep_graph,
            &HashSet::from([DepNode::Function(indirrect_fn)]),
        );
        reachability_graph.print("indirrect_fn", &info);
        // almost same count, but indirrect_fn has more deep graph and switchtable
        // indirrect_fn -> switchtable -> func1 -> data1
        //                             -> func2 -> data2
        //                             -> func3 -> data3
        assert_eq!(reachability_graph.reachable.len(), 8); // root + <switchtable> +  3 data + 3 funcs
    }

    lazy_static! {
        static ref TEST_GRAPH: DepGraph = testing::parse_deps(
            r#"
            F(1) -> D(2, 3) & F(4) -> D(5, 6) & F(7) -> D(8, 9)
            F(11) -> F(4) & F(12)
            "#
        )
        .unwrap();
    }
    #[test]
    fn test_unique_nodes() {
        let graph = TEST_GRAPH.clone();
        let modules = vec![
            super::NamedGraph {
                module: "module1",
                deps: super::ReachabilityGraph::find_reachable_deps(
                    &graph,
                    &testing::uniq_nodes("F(1)").unwrap(),
                ),
            },
            super::NamedGraph {
                module: "module2",
                deps: super::ReachabilityGraph::find_reachable_deps(
                    &graph,
                    &testing::uniq_nodes("F(11)").unwrap(),
                ),
            },
        ];

        let first_module = &modules[0];
        let first_graph =
            testing::uniq_nodes("F(1) & D(2, 3) & F(4) & D(5, 6) & F(7) & D(8, 9)").unwrap();

        assert_eq!(first_module.deps.reachable, first_graph);
        assert!(first_module
            .deps
            .check_unreachable(&testing::uniq_nodes("F(11) & F(12)").unwrap()));

        let second_module = &modules[1];
        let second_graph =
            testing::uniq_nodes("F(11) & F(12) & F(4) & D(5, 6) & F(7) & D(8, 9)").unwrap();
        assert_eq!(second_module.deps.reachable, second_graph);

        assert!(second_module
            .deps
            .check_unreachable(&testing::uniq_nodes("F(1) & D(2, 3)").unwrap()));
    }

    #[test]
    fn test_shared_entries() {
        let graph = TEST_GRAPH.clone();
        let mut modules = vec![
            super::NamedGraph {
                module: "module1",
                deps: super::ReachabilityGraph::find_reachable_deps(
                    &graph,
                    &testing::uniq_nodes("F(1)").unwrap(),
                ),
            },
            super::NamedGraph {
                module: "module2",
                deps: super::ReachabilityGraph::find_reachable_deps(
                    &graph,
                    &testing::uniq_nodes("F(11)").unwrap(),
                ),
            },
        ];

        let shared_entries = super::NamedGraph::calculate_shared_modules(&mut modules, &graph);

        assert_eq!(shared_entries.len(), 1);
        assert_eq!(shared_entries[0].module_names, vec!["module1", "module2"]);
        assert_eq!(
            shared_entries[0].shared_deps,
            testing::uniq_nodes("D(5, 6) & D(8, 9) & F(4) & F(7)").unwrap()
        );

        // Test that top_most_dep contains only F(4)
        // It is top-most because it doesn't depend on other shared dependencies
        // F(7), D(5,6) and D(8,9) are not top-most because F(4) -> D(5,6) & F(7)and F(7) -> D(8,9)
        assert_eq!(
            shared_entries[0].topmost_shared,
            testing::uniq_nodes("F(4)").unwrap()
        );
    }

    #[test]
    fn test_multiple_shared_deps() {
        let input = r#"
        F(1) -> D(2, 3) & F(11) & F(4) -> D(5, 6) 
        F(11) -> D(12, 13) -> F(12) & F(4) & F(7) -> D(8, 9)
        F(10) -> D(11, 12) & F(7)
        F(20) -> F(4) & F(7)
        "#;
        let mut modules = vec![
            super::NamedGraph {
                module: "module1",
                deps: super::ReachabilityGraph::find_reachable_deps(
                    &testing::parse_deps(input).unwrap(),
                    &testing::uniq_nodes("F(1)").unwrap(),
                ),
            },
            super::NamedGraph {
                module: "module2",
                deps: super::ReachabilityGraph::find_reachable_deps(
                    &testing::parse_deps(input).unwrap(),
                    &testing::uniq_nodes("F(10)").unwrap(),
                ),
            },
            super::NamedGraph {
                module: "module3",
                deps: super::ReachabilityGraph::find_reachable_deps(
                    &testing::parse_deps(input).unwrap(),
                    &testing::uniq_nodes("F(20)").unwrap(),
                ),
            },
        ];

        let shared_entries = super::NamedGraph::calculate_shared_modules(
            &mut modules,
            &testing::parse_deps(input).unwrap(),
        );

        assert_eq!(shared_entries.len(), 2);
        assert_eq!(
            shared_entries[0].module_names,
            vec!["module1", "module2", "module3"]
        );
        assert_eq!(
            shared_entries[0].shared_deps,
            testing::uniq_nodes("D(8, 9) & F(7)").unwrap()
        );
        assert_eq!(
            shared_entries[0].topmost_shared,
            testing::uniq_nodes("F(7)").unwrap()
        );
        assert_eq!(shared_entries[1].module_names, vec!["module1", "module3"]);
        assert_eq!(
            shared_entries[1].shared_deps, // F(7) and childs are stored in [m1,m2,m3] shared deps
            testing::uniq_nodes("F(4) & D(5, 6)").unwrap()
        );
        assert_eq!(
            shared_entries[1].topmost_shared,
            testing::uniq_nodes("F(4)").unwrap()
        );
    }

    #[test]
    fn test_recursive_shared_deps() {
        // F4 is parent of F7 which call F4
        let input = r#"
        F(1) -> D(2, 3) & F(4) -> D(5, 6) & F(7) -> D(8, 9) & F(4)
        F(10) -> D(11, 12) -> F(4)
        "#;
        let mut modules = vec![
            super::NamedGraph {
                module: "module1",
                deps: super::ReachabilityGraph::find_reachable_deps(
                    &testing::parse_deps(input).unwrap(),
                    &testing::uniq_nodes("F(1)").unwrap(),
                ),
            },
            super::NamedGraph {
                module: "module2",
                deps: super::ReachabilityGraph::find_reachable_deps(
                    &testing::parse_deps(input).unwrap(),
                    &testing::uniq_nodes("F(10)").unwrap(),
                ),
            },
        ];

        let shared_entries = super::NamedGraph::calculate_shared_modules(
            &mut modules,
            &testing::parse_deps(input).unwrap(),
        );

        assert_eq!(shared_entries.len(), 1);
        assert_eq!(shared_entries[0].module_names, vec!["module1", "module2"]);
        assert_eq!(
            shared_entries[0].shared_deps,
            testing::uniq_nodes("F(4) & D(5, 6) & F(7) & D(8, 9)").unwrap()
        );
        assert_eq!(
            shared_entries[0].topmost_shared,
            testing::uniq_nodes("F(4)").unwrap()
        );
    }
}
