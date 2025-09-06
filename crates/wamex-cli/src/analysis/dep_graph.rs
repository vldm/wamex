use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt::Debug,
    ops::Range,
};

use anyhow::Context;
use wasmparser::RelocationType;

use crate::{
    analysis,
    helpers::RangeExt,
    index::{AnySymbolId, DataSegmentId, DataSymbolId, InputFuncId},
    read::{linking::SymbolIndex, InputModule},
};

#[derive(PartialEq, Eq, Hash, Copy, PartialOrd, Ord, Clone)]
pub enum DepNode {
    Function(InputFuncId),
    DataSymbol(DataSegmentId, DataSymbolId),
}
impl Debug for DepNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DepNode::Function(id) => write!(f, "F({id})"),
            DepNode::DataSymbol(segment_id, symbol_index) => {
                write!(f, "D({segment_id}, {symbol_index})")
            }
        }
    }
}

impl DepNode {
    pub fn as_function(&self) -> Option<InputFuncId> {
        match self {
            DepNode::Function(id) => Some(*id),
            _ => None,
        }
    }
    pub fn as_data_symbol(&self) -> Option<(DataSegmentId, DataSymbolId)> {
        match self {
            DepNode::DataSymbol(segment_id, symbol_index) => Some((*segment_id, *symbol_index)),
            _ => None,
        }
    }
}

pub type DepList = HashSet<DepNode>;
#[derive(Clone, Default)]
pub struct DepGraph {
    deps: HashMap<DepNode, DepList>,
}
impl DepGraph {
    pub fn new() -> Self {
        Self {
            deps: HashMap::new(),
        }
    }
    pub fn entry(&mut self, key: DepNode) -> &mut DepList {
        self.deps.entry(key).or_default()
    }
    pub fn get(&self, key: &DepNode) -> Option<&DepList> {
        self.deps.get(key)
    }
    pub fn get_mut(&mut self, key: &DepNode) -> Option<&mut DepList> {
        self.deps.get_mut(key)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&DepNode, &DepList)> {
        self.deps.iter()
    }
    /// Build a reverse graph from the given dep graph.
    /// parent -> child becomes child -> parent
    ///
    /// This is usefull for finding all parents of a given node.
    pub fn reverse(&self) -> DepGraph {
        let mut reversed = DepGraph::new();
        for (node, deps) in self.iter() {
            for dep in deps {
                reversed.entry(*dep).insert(*node);
            }
        }
        reversed
    }
}
impl From<HashMap<DepNode, DepList>> for DepGraph {
    fn from(deps: HashMap<DepNode, DepList>) -> Self {
        Self { deps }
    }
}
impl Debug for DepGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (node, deps) in self.iter() {
            if deps.is_empty() {
                writeln!(f, "{node:?} -> <no deps>")?;
                continue;
            }
            write!(f, "{node:?} -> ")?;
            let mut deps = deps.iter();
            if let Some(dep) = deps.next() {
                write!(f, "{dep:?}")?;
            }

            for dep in deps {
                write!(f, " & {dep:?}")?;
            }
            writeln!(f)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct ReachabilityGraph {
    pub reachable: DepList,
    pub parents: DepGraph,
}

#[derive(Debug, Clone)]
pub struct NamedGraph<Id> {
    pub module: Id,
    pub deps: ReachabilityGraph,

    /// This field is hidden, because it is output parameter of `calculate_shared_modules`
    linked_nodes: HashSet<DepNode>,
}

impl<Id> NamedGraph<Id> {
    pub fn new(module: Id, deps: ReachabilityGraph) -> Self {
        Self {
            module,
            deps,
            linked_nodes: HashSet::new(),
        }
    }
    pub fn linked_nodes(&self) -> &HashSet<DepNode> {
        &self.linked_nodes
    }
}

#[derive(Debug, Clone)]
pub struct SharedEntries<Id> {
    pub module_names: Vec<Id>,
    pub shared_deps: HashSet<DepNode>,
    pub linked_nodes: HashSet<DepNode>,
}

pub trait SymbolTable {
    fn get_symbol_dep_node(&self, symbol_index: AnySymbolId) -> Option<DepNode>;
}

impl<'a> SymbolTable for InputModule<'a> {
    fn get_symbol_dep_node(&self, linking_index: AnySymbolId) -> Option<DepNode> {
        let symbol_index = self.linking.linking_symbols.original_indexes[linking_index];
        match symbol_index {
            SymbolIndex::Func(idx) => Some(DepNode::Function(idx)),
            SymbolIndex::DataDefined(segment_id, idx) => Some(DepNode::DataSymbol(segment_id, idx)),
            SymbolIndex::DataUndefined(undefined_idx) => {
                log::error!(
                    "{undefined_idx} {} is undefined data symbol, dont have any location in data segment, what can we do with it?",
                    self.linking.linking_symbols.undefined_data[undefined_idx].name
                );
                None
            }
            _ => {
                // log::error!("{symbol_index:?} is not supported symol dep");
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
            log::trace!("Adding direct deps {a:?} -> {target:?}");
            deps.entry(a).insert(target);
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

            if seen.contains(&node) {
                continue;
            }

            seen.insert(node);
            let Some(children) = deps.get(&node) else {
                continue;
            };
            for child in children {
                parents.entry(*child).insert(node);
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

        let mut removed = HashSet::<DepNode>::new();
        while let Some(node) = queue.pop_front() {
            if removed.contains(&node) {
                continue;
            }
            removed.insert(node);

            // Can be unreachable if cycle in graph
            let _reachable = self.reachable.remove(&node);

            if let Some(children) = graph.get(&node) {
                for child in children {
                    let child_parent = self.parents.get_mut(&child).expect("Parent should exist");
                    child_parent.remove(&node);
                    if child_parent.is_empty() {
                        queue.push_back(*child);
                    }
                }
            }
        }
    }

    #[cfg(test)]
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
    /// This will clean-up tree of shared entries and leave only top-most entries.
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

        for m in modules.iter() {
            debug_assert!(
                m.linked_nodes.is_empty(),
                "Linked nodes is output parameter"
            );
        }

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
        let mut result = Vec::new();

        for (module_ids, shared_deps) in shared_entries {
            let mut module_names = Vec::new();
            let mut shared_linked_points = HashSet::new();

            for module_id in module_ids {
                let module = &mut modules[module_id];
                let module_parents = &module.deps.parents;

                let top_shared_deps =
                    Self::reduce_shared_entries(shared_deps.clone(), module_parents);
                module.linked_nodes.extend(top_shared_deps.clone());
                shared_linked_points.extend(top_shared_deps);
                module_names.push(module.module.clone());
            }
            result.push(SharedEntries {
                module_names,
                shared_deps,
                linked_nodes: shared_linked_points,
            });
        }

        // For each dep -> child if child is not found in deps: add it to linked_nodes
        // for module in

        result.sort_by(|left, right| left.module_names.cmp(&right.module_names));
        result
    }
}

fn find_function_containing_range(
    info: &analysis::ModuleInfo,
    range: Range<usize>,
) -> anyhow::Result<InputFuncId> {
    info.find_function_id_containing_range(range)
}

fn find_data_symbol_containing_range(
    info: &analysis::ModuleInfo,
    range: Range<usize>,
) -> anyhow::Result<(DataSegmentId, DataSymbolId)> {
    let sym = &info.find_data_symbol_containing_range(range)?;
    Ok((sym.segment_index, sym.symbol_index))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashSet, VecDeque},
        fs::File,
        io::Write,
    };

    use lazy_static::lazy_static;
    use testing::tests::function;

    use crate::{
        analysis::{
            self,
            dep_graph::{DepGraph, DepNode},
            testing,
        },
        index::Id,
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
                let symbol = module.linking.linking_symbols.data_in_segments[*segment][*idx].name;
                let segment = module.names.data_segments[*segment];
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

        let indirect_fn = info.find_function_id_by_name("indirect_fn").unwrap();

        let deps = dep_graph.get(&DepNode::Function(indirect_fn)).unwrap();
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

        let indirect_fn = info.find_function_id_by_name("indirect_fn").unwrap();
        let reachability_graph = super::ReachabilityGraph::find_reachable_deps(
            &dep_graph,
            &HashSet::from([DepNode::Function(indirect_fn)]),
        );
        reachability_graph.print("indirect_fn", &info);
        // almost same count, but indirect_fn has more deep graph and switchtable
        // indirect_fn -> switchtable -> func1 -> data1
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
            super::NamedGraph::new(
                "module1",
                super::ReachabilityGraph::find_reachable_deps(
                    &graph,
                    &testing::uniq_nodes("F(1)").unwrap(),
                ),
            ),
            super::NamedGraph::new(
                "module2",
                super::ReachabilityGraph::find_reachable_deps(
                    &graph,
                    &testing::uniq_nodes("F(11)").unwrap(),
                ),
            ),
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
            super::NamedGraph::new(
                "module1",
                super::ReachabilityGraph::find_reachable_deps(
                    &graph,
                    &testing::uniq_nodes("F(1)").unwrap(),
                ),
            ),
            super::NamedGraph::new(
                "module2",
                super::ReachabilityGraph::find_reachable_deps(
                    &graph,
                    &testing::uniq_nodes("F(11)").unwrap(),
                ),
            ),
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
        for module in &modules {
            assert_eq!(module.linked_nodes, testing::uniq_nodes("F(4)").unwrap());
        }
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
            super::NamedGraph::new(
                "module1",
                super::ReachabilityGraph::find_reachable_deps(
                    &testing::parse_deps(input).unwrap(),
                    &testing::uniq_nodes("F(1)").unwrap(),
                ),
            ),
            super::NamedGraph::new(
                "module2",
                super::ReachabilityGraph::find_reachable_deps(
                    &testing::parse_deps(input).unwrap(),
                    &testing::uniq_nodes("F(10)").unwrap(),
                ),
            ),
            super::NamedGraph::new(
                "module3",
                super::ReachabilityGraph::find_reachable_deps(
                    &testing::parse_deps(input).unwrap(),
                    &testing::uniq_nodes("F(20)").unwrap(),
                ),
            ),
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
            modules[1].linked_nodes,
            testing::uniq_nodes("F(7)").unwrap()
        );
        assert_eq!(shared_entries[1].module_names, vec!["module1", "module3"]);
        assert_eq!(
            shared_entries[1].shared_deps, // F(7) and children are stored in [m1,m2,m3] shared deps
            testing::uniq_nodes("F(4) & D(5, 6)").unwrap()
        );
        for module in &[&modules[0], &modules[2]] {
            assert_eq!(
                module.linked_nodes,
                testing::uniq_nodes("F(4) & F(7)").unwrap()
            );
        }
    }

    #[test]
    fn test_recursive_shared_deps() {
        // F4 is parent of F7 which call F4
        let input = r#"
        F(1) -> D(2, 3) & F(4) -> D(5, 6) & F(7) -> D(8, 9) & F(4)
        F(10) -> D(11, 12) -> F(4)
        "#;
        let mut modules = vec![
            super::NamedGraph::new(
                "module1",
                super::ReachabilityGraph::find_reachable_deps(
                    &testing::parse_deps(input).unwrap(),
                    &testing::uniq_nodes("F(1)").unwrap(),
                ),
            ),
            super::NamedGraph::new(
                "module2",
                super::ReachabilityGraph::find_reachable_deps(
                    &testing::parse_deps(input).unwrap(),
                    &testing::uniq_nodes("F(10)").unwrap(),
                ),
            ),
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
        for module in &modules {
            assert_eq!(module.linked_nodes, testing::uniq_nodes("F(4)").unwrap());
        }
    }

    #[test]
    fn test_cascade_remove() {
        let graph = testing::parse_deps(
            r#"
            F(1) -> F(2) & F(3) -> F(4) -> F(6) & F(7)
            F(2) -> F(5) & F(6)
            "#,
        )
        .unwrap();

        let mut reachability_graph = super::ReachabilityGraph::find_reachable_deps(
            &graph,
            &testing::uniq_nodes("F(1)").unwrap(),
        );
        assert_eq!(
            reachability_graph.reachable,
            testing::uniq_nodes("F(1) & F(2) & F(3) & F(4) & F(5) & F(6) & F(7)").unwrap()
        );

        // this remove f(4) and child f(7), f(6) is still reachable by f(2)
        reachability_graph.remove_node(&function(4), &graph);

        assert_eq!(
            reachability_graph.reachable,
            testing::uniq_nodes("F(1) & F(2) & F(3) & F(5) & F(6)").unwrap()
        );
        reachability_graph.remove_node(&function(2), &graph);
        assert_eq!(
            reachability_graph.reachable,
            testing::uniq_nodes("F(1) & F(3)").unwrap()
        );
    }

    #[test]
    fn test_shared_deps_reduced() {
        let source = r#"
F(899) ->  F(307) & F(912)
F(1358) -> F(1417) -> F(1418) -> F(4759)
F(124) -> F(4759)
F(307) -> F(308) & F(124)
F(912) -> F(1358)
F(1) -> F(307)
        "#;
        let graph = testing::parse_deps(source).unwrap();

        let mut modules = vec![
            super::NamedGraph::new(
                "main",
                super::ReachabilityGraph::find_reachable_deps(
                    &graph,
                    &testing::uniq_nodes("F(1)").unwrap(),
                ),
            ),
            super::NamedGraph::new(
                "split_string_from_static",
                super::ReachabilityGraph::find_reachable_deps(
                    &graph,
                    &testing::uniq_nodes("F(899)").unwrap(),
                ),
            ),
        ];

        dbg!(&modules);

        let shared_entries = super::NamedGraph::calculate_shared_modules(&mut modules, &graph);

        dbg!(&shared_entries);
        let node = DepNode::Function(Id::from_index(1417));

        assert!(modules[1].deps.reachable.contains(&node));

        let roots = [node].into_iter().collect();
        let child = super::ReachabilityGraph::find_reachable_deps(&graph, &roots);

        assert!(!child.reachable.is_empty());
        // make sure that all childs of specific node (that known to be in module 2) are included or linked
        for node in child.reachable {
            dbg!(node);
            let direct_dep = modules[1].deps.reachable.contains(&node);
            let linked_dep = modules[1].linked_nodes.contains(&node);

            assert!(direct_dep || linked_dep);
        }
    }

    const INPUT: &str = r#"
F(1358) -> F(1359) & F(1366) & F(1417)
F(4453) -> F(4800) & F(4759)
F(912) -> F(1358)
F(4452) -> F(4453)
F(4630) -> F(4452) & F(4464)
F(4759) -> F(4671)
F(1417) -> F(1418)
F(1418) -> F(4759)
F(899) -> F(912) & F(307) & F(916)
F(4671) -> F(4663)
            "#;
    #[test]
    fn test_deps_in_shared_conflict() {
        assert!(test_deps_in_shared_conflict_impl(INPUT));
    }

    #[test]
    fn test_reduce_deps_in_shared_conflict() {
        reduce(INPUT, test_deps_in_shared_conflict_impl);
    }

    fn test_deps_in_shared_conflict_impl(source: &str) -> bool {
        let graph = testing::parse_deps(source).unwrap();

        let mut modules = vec![
            super::NamedGraph::new(
                "main",
                super::ReachabilityGraph::find_reachable_deps(
                    &graph,
                    &testing::uniq_nodes("F(3484) & F(2252) & F(2903) & F(3721) & F(4229) & F(3810) & F(1954) & F(2415) & F(1558) & F(1788) & F(3730) & F(3480) & F(4446) & F(4830) & F(1644) & F(2901) & F(2492) & F(1771) & F(1969) & F(2307) & F(2669) & F(3524) & F(1672) & F(1568) & F(2880) & F(2703) & F(2788) & F(3511) & F(1618) & F(2730) & F(1925) & F(2474) & F(2644) & F(2266) & F(3694) & F(4283) & F(4313) & F(4425) & F(3504) & F(2337) & F(1919) & F(3659) & F(1732) & F(2697) & F(3597) & F(1736) & F(1667) & F(2254) & F(3806) & F(1673) & F(1863) & F(3906) & F(4088) & F(2235) & F(1817) & F(2536) & F(3006) & F(1794) & F(3653) & F(1565) & F(2561) & F(2103) & F(2175) & F(2417) & F(2161) & F(2946) & F(3596) & F(3748) & F(2812) & F(3796) & F(4053) & F(2153) & F(4359) & F(1666) & F(2188) & F(1974) & F(1871) & F(3904) & F(2598) & F(2690) & F(2771) & F(4853) & F(4228) & F(3582) & F(1814) & F(2274) & F(1845) & F(3666) & F(1546) & F(1985) & F(2681) & F(1772) & F(2160) & F(2942) & F(3739) & F(1972) & F(4031) & F(1943) & F(3814) & F(1674) & F(3952) & F(1700) & F(2811) & F(1792) & F(1895) & F(1802) & F(2215) & F(1903) & F(1575) & F(4832) & F(4884) & F(1681) & F(2704) & F(2805) & F(3509) & F(2757) & F(2272) & F(3788) & F(4847) & F(3669) & F(3851) & F(4039) & F(2652) & F(3928) & F(1791) & F(2234) & F(1716) & F(3609) & F(3711) & F(2622) & F(2029) & F(2202) & F(2278) & F(3498) & F(2094) & F(3544) & F(3981) & F(1710) & F(2346) & F(1994) & F(2521) & F(3888) & F(2496) & F(1708) & F(2894) & F(2043) & F(3949) & F(2705) & F(3634) & F(3861) & F(4350) & F(2817) & F(2368) & F(3478) & F(1844) & F(4375) & F(2801) & F(3926) & F(3108) & F(3490) & F(3139) & F(2567) & F(4004) & F(2088) & F(1984) & F(2955) & F(4436) & F(1769) & F(1628) & F(1835) & F(1876) & F(2724) & F(3513) & F(4620) & F(2484) & F(4064) & F(1650) & F(3881) & F(3935) & F(4284) & F(4294) & F(4840) & F(2454) & F(3795) & F(1703) & F(2196) & F(2869) & F(2384) & F(3750) & F(3131) & F(2270) & F(1977) & F(2219) & F(4063) & F(4389) & F(1551) & F(3741) & F(4339) & F(4399) & F(2786) & F(1945) & F(4287) & F(2407) & F(4870) & F(3464) & F(3012) & F(2717) & F(3109) & F(2370) & F(4060) & F(2983) & F(2589) & F(2620) & F(2693) & F(1995) & F(1734) & F(3956) & F(2957) & F(3036) & F(4429) & F(4435) & F(3552) & F(1567) & F(2108) & F(2350) & F(3572) & F(1868) & F(2873) & F(3129) & F(2289) & F(3058) & F(1676) & F(2563) & F(2585) & F(2134) & F(2827) & F(3007) & F(3708) & F(2294) & F(2009) & F(2944) & F(2958) & F(3132) & F(2745) & F(3822) & F(2343) & F(3964) & F(1590) & F(1797) & F(2649) & F(4073) & F(1951) & F(3516) & F(2677) & F(3500) & F(2139) & F(2142) & F(3614) & F(2031) & F(2639) & F(3051) & F(3059) & F(3746) & F(2340) & F(2814) & F(2006) & F(3753) & F(4885) & F(2897) & F(2748) & F(2898) & F(4421) & F(3660) & F(3995) & F(3640) & F(4005) & F(1852) & F(1699) & F(1726) & F(2769) & F(1936) & F(1678) & F(1923) & F(4295) & F(4080) & F(2972) & F(4322) & F(3000) & F(2112) & F(3967) & F(3502) & F(1689) & F(2948) & F(2862) & F(2832) & F(2809) & F(4851) & F(3499) & F(1545) & F(2162) & F(3549) & F(2179) & F(3045) & F(2145) & F(2978) & F(2550) & F(2518) & F(2632) & F(2654) & F(1711) & F(1544) & F(4034) & F(4844) & F(2570) & F(2530) & F(2565) & F(4036) & F(1640) & F(2102) & F(2063) & F(2165) & F(1721) & F(1879) & F(2688) & F(4875) & F(3030) & F(3497) & F(4390) & F(1864) & F(4392) & F(2879) & F(1789) & F(3688) & F(2635) & F(4335) & F(4887) & F(3541) & F(3630) & F(2070) & F(1612) & F(2095) & F(2457) & F(1747) & F(4077) & F(3125) & F(2240) & F(2876) & F(2035) & F(4076) & F(2740) & F(2022) & F(3117) & F(2309) & F(1757) & F(3987) & F(3646) & F(3908) & F(2723) & F(2098) & F(3595) & F(3747) & F(2471) & F(2568) & F(2829) & F(3563) & F(4859) & F(1865) & F(3133) & F(3829) & F(2373) & F(2670) & F(4891) & F(3570) & F(3718) & F(1742) & F(2634) & F(1572) & F(2382) & F(2937) & F(3738) & F(3801) & F(3958) & F(4026) & F(1573) & F(3092) & F(4293) & F(540) & F(2804) & F(1651) & F(2727) & F(3600) & F(2965) & F(1598) & F(2831) & F(2374) & F(3905) & F(2933) & F(2475) & F(1921) & F(3764) & F(2296) & F(3975) & F(3593) & F(3961) & F(4041) & F(1576) & F(3752) & F(2875) & F(2871) & F(1642) & F(2970) & F(3862) & F(2610) & F(1781) & F(4856) & F(2456) & F(2143) & F(2163) & F(4045) & F(4071) & F(2265) & F(2038) & F(3529) & F(3707) & F(3841) & F(2335) & F(3038) & F(2136) & F(3635) & F(1577) & F(2018) & F(1599) & F(1632) & F(1898) & F(3864) & F(2472) & F(3046) & F(2624) & F(3794) & F(2547) & F(2459) & F(2089) & F(4323) & F(1779) & F(2775) & F(3469) & F(2025) & F(2830) & F(2807) & F(2193) & F(2782) & F(2174) & F(2426) & F(2747) & F(2770) & F(3523) & F(3932) & F(3938) & F(2419) & F(1826) & F(2534) & F(2181) & F(3611) & F(3847) & F(3097) & F(3126) & F(3032) & F(2595) & F(3482) & F(2587) & F(2401) & F(4007) & F(1900) & F(1847) & F(1661) & F(1704) & F(2776) & F(2628) & F(2273) & F(1582) & F(2342) & F(2672) & F(1891) & F(3543) & F(4331) & F(3745) & F(1634) & F(2676) & F(2678) & F(1840) & F(1701) & F(1825) & F(2428) & F(3471) & F(2376) & F(2286) & F(2402) & F(1777) & F(2225) & F(3122) & F(4298) & F(2356) & F(1722) & F(1547) & F(3573) & F(2421) & F(2363) & F(3002) & F(2082) & F(1724) & F(3944) & F(2330) & F(1917) & F(3095) & F(3618) & F(1801) & F(4873) & F(2045) & F(4062) & F(4883) & F(2729) & F(2683) & F(3762) & F(2110) & F(2383) & F(1941) & F(2135) & F(3546) & F(3607) & F(3818) & F(4018) & F(1856) & F(1671) & F(4876) & F(1962) & F(2424) & F(1939) & F(2338) & F(2577) & F(1663) & F(1888) & F(2367) & F(3805) & F(1823) & F(1822) & F(1846) & F(3062) & F(3679) & F(3775) & F(4332) & F(2116) & F(4630) & F(3532) & F(1956) & F(1800) & F(2746) & F(2079) & F(2487) & F(3545) & F(1926) & F(2085) & F(4302) & F(2508) & F(2890) & F(1988) & F(2963) & F(4022) & F(3136) & F(2489) & F(1668) & F(2495) & F(4356) & F(4054) & F(4845) & F(3578) & F(1978) & F(3642) & F(2784) & F(2105) & F(3685) & F(3518) & F(2752) & F(1585) & F(2364) & F(2781) & F(2396) & F(2077) & F(2152) & F(2156) & F(1992) & F(2427) & F(2005) & F(3637) & F(3999) & F(4444) & F(1938) & F(2753) & F(2655) & F(2086) & F(2641) & F(3120) & F(3731) & F(4833) & F(2469) & F(2130) & F(2425) & F(1913) & F(2172) & F(3820) & F(1602) & F(3512) & F(3828) & F(4017) & F(2532) & F(2967) & F(3837) & F(3722) & F(4010) & F(3917) & F(3997) & F(3777) & F(2755) & F(1561) & F(3094) & F(2877) & F(2586) & F(2643) & F(1967) & F(2282) & F(2523) & F(2120) & F(2313) & F(1973) & F(2816) & F(2406) & F(1930) & F(3015) & F(2269) & F(4835) & F(2544) & F(2039) & F(1739) & F(1873) & F(1848) & F(2739) & F(2800) & F(2190) & F(3677) & F(2423) & F(4401) & F(2004) & F(3670) & F(1741) & F(3623) & F(2517) & F(2422) & F(2388) & F(2091) & F(2024) & F(3087) & F(3613) & F(3756) & F(3535) & F(3005) & F(2107) & F(2734) & F(2868) & F(1922) & F(1574) & F(2588) & F(1881) & F(2537) & F(2133) & F(3137) & F(1765) & F(2527) & F(3675) & F(2065) & F(4330) & F(3974) & F(2220) & F(1839) & F(3779) & F(3890) & F(1548) & F(1690) & F(1818) & F(2262) & F(2040) & F(2687) & F(2470) & F(4837) & F(3049) & F(3911) & F(3853) & F(1720) & F(2053) & F(2128) & F(2514) & F(4049) & F(3037) & F(2461) & F(2618) & F(2420) & F(3472) & F(2749) & F(3022) & F(4355) & F(2263) & F(3678) & F(2765) & F(3704) & F(2604) & F(2608) & F(1748) & F(2092) & F(2490) & F(4424) & F(2695) & F(4831) & F(2132) & F(4028) & F(2148) & F(3802) & F(2178) & F(4354) & F(4632) & F(2888) & F(2144) & F(1627) & F(4059) & F(2580) & F(4336) & F(1998) & F(3843) & F(2892) & F(2648) & F(2767) & F(1707) & F(1616) & F(3929) & F(1646) & F(1615) & F(2047) & F(2412) & F(3534) & F(4318) & F(1622) & F(2213) & F(3835) & F(2124) & F(2606) & F(2231) & F(2579) & F(2326) & F(4855) & F(3622) & F(2551) & F(2334) & F(2887) & F(1947) & F(2564) & F(3008) & F(3580) & F(3784) & F(3791) & F(2072) & F(2940) & F(3033) & F(2455) & F(3627) & F(1562) & F(1705) & F(2255) & F(1776) & F(1763) & F(4345) & F(1665) & F(2201) & F(3574) & F(3763) & F(2267) & F(2689) & F(2637) & F(2310) & F(3020) & F(3592) & F(1751) & F(2737) & F(4050) & F(1652) & F(2167) & F(2651) & F(2062) & F(1613) & F(1637) & F(2706) & F(3658) & F(1761) & F(1746) & F(3047) & F(2558) & F(3872) & F(3717) & F(3923) & F(2260) & F(4863) & F(3517) & F(2250) & F(3755) & F(2321) & F(3968) & F(2883) & F(1656) & F(4065) & F(3364) & F(2332) & F(2073) & F(2393) & F(1813) & F(2638) & F(3715) & F(3506) & F(3734) & F(1815) & F(4400) & F(2605) & F(2377) & F(3773) & F(2395) & F(3089) & F(1824) & F(4386) & F(4417) & F(3017) & F(3813) & F(2408) & F(3856) & F(3011) & F(1731) & F(3925) & F(4839) & F(1811) & F(3590) & F(3758) & F(3781) & F(3966) & F(1887) & F(4042) & F(2573) & F(3579) & F(1838) & F(1631) & F(3712) & F(1733) & F(2795) & F(1706) & F(3896) & F(1989) & F(3536) & F(3916) & F(4038) & F(3893) & F(3671) & F(2935) & F(3945) & F(1591) & F(3803) & F(2261) & F(3138) & F(2715) & F(1593) & F(2268) & F(2682) & F(2355) & F(4388) & F(2515) & F(1805) & F(2802) & F(4343) & F(3854) & F(1702) & F(3792) & F(4394) & F(2371) & F(2511) & F(3700) & F(1862) & F(1713) & F(4860) & F(1786) & F(3111) & F(2623) & F(2157) & F(1750) & F(2186) & F(4327) & F(2633) & F(2187) & F(1564) & F(2566) & F(2707) & F(2759) & F(2138) & F(4089) & F(4326) & F(1619) & F(4872) & F(4333) & F(1796) & F(1982) & F(3765) & F(2545) & F(2792) & F(2482) & F(2980) & F(3962) & F(3687) & F(2528) & F(2701) & F(2685) & F(4376) & F(3525) & F(3709) & F(3004) & F(1589) & F(3539) & F(2718) & F(2155) & F(3934) & F(1655) & F(1806) & F(2741) & F(4848) & F(3761) & F(3993) & F(2104) & F(4011) & F(4348) & F(2591) & F(4416) & F(4624) & F(1597) & F(2354) & F(4082) & F(1778) & F(4012) & F(2733) & F(4865) & F(2956) & F(1812) & F(2317) & F(1971) & F(3086) & F(2936) & F(3467) & F(3866) & F(3985) & F(2430) & F(3550) & F(3602) & F(2938) & F(3858) & F(3521) & F(3465) & F(3714) & F(3026) & F(2068) & F(2308) & F(2535) & F(2003) & F(2959) & F(3050) & F(4083) & F(3040) & F(4292) & F(2626) & F(1964) & F(2318) & F(1883) & F(2010) & F(3980) & F(3085) & F(3870) & F(2631) & F(2189) & F(2665) & F(3836) & F(1924) & F(2465) & F(2147) & F(3724) & F(3702) & F(1931) & F(4013) & F(2223) & F(1768) & F(3096) & F(4281) & F(2865) & F(3749) & F(2895) & F(3865) & F(2369) & F(2815) & F(2126) & F(3116) & F(3827) & F(3031) & F(2462) & F(3098) & F(4360) & F(2821) & F(3804) & F(4391) & F(2324) & F(2960) & F(1692) & F(3979) & F(2505) & F(4032) & F(4308) & F(2399) & F(1963) & F(2823) & F(3845) & F(4387) & F(3115) & F(2080) & F(3809) & F(4086) & F(1554) & F(1810) & F(4319) & F(4433) & F(4075) & F(2819) & F(1583) & F(2962) & F(2790) & F(2050) & F(3520) & F(2184) & F(3057) & F(1758) & F(2339) & F(4862) & F(1959) & F(1670) & F(3719) & F(3821) & F(4346) & F(4033) & F(1807) & F(2311) & F(2712) & F(1842) & F(3971) & F(2714) & F(2736) & F(3548) & F(2387) & F(2971) & F(4874) & F(1991) & F(3616) & F(3726) & F(3927) & F(3649) & F(2581) & F(4334) & F(2953) & F(3682) & F(2224) & F(1653) & F(4066) & F(1894) & F(3112) & F(2772) & F(3880) & F(1675) & F(2397) & F(2668) & F(3871) & F(2238) & F(4021) & F(2100) & F(4393) & F(4629) & F(1600) & F(1983) & F(3039) & F(2555) & F(2969) & F(2150) & F(2973) & F(3751) & F(1990) & F(4337) & F(2719) & F(4351) & F(2557) & F(2808) & F(3921) & F(1860) & F(2629) & F(2735) & F(2943) & F(3493) & F(3477) & F(2226) & F(2533) & F(1952) & F(1828) & F(2616) & F(2028) & F(3973) & F(2158) & F(1714) & F(3538) & F(2288) & F(2276) & F(2287) & F(2657) & F(1950) & F(2602) & F(3027) & F(3852) & F(2529) & F(2058) & F(4627) & F(1636) & F(1942) & F(4861) & F(3102) & F(2345) & F(2205) & F(1901) & F(3919) & F(2414) & F(4304) & F(2612) & F(2411) & F(3508) & F(3568) & F(2118) & F(4347) & F(1719) & F(1571) & F(4037) & F(3948) & F(2467) & F(4838) & F(1625) & F(1738) & F(4377) & F(2113) & F(2763) & F(2019) & F(4445) & F(2216) & F(2228) & F(2280) & F(2017) & F(2818) & F(3661) & F(2710) & F(2305) & F(1934) & F(1915) & F(3533) & F(4344) & F(2180) & F(3527) & F(3013) & F(2625) & F(2721) & F(2314) & F(2121) & F(1832) & F(4067) & F(4378) & F(3105) & F(2069) & F(3695) & F(3834) & F(3842) & F(2761) & F(2327) & F(1927) & F(2468) & F(3889) & F(3647) & F(1819) & F(3912) & F(1696) & F(4867) & F(2979) & F(2964) & F(2319) & F(1841) & F(3998) & F(1717) & F(1683) & F(2249) & F(2361) & F(2738) & F(2864) & F(1608) & F(2542) & F(3703) & F(1975) & F(3884) & F(1718) & F(3930) & F(4310) & F(2889) & F(3833) & F(4403) & F(4828) & F(2539) & F(2106) & F(2353) & F(3555) & F(3489) & F(3977) & F(3799) & F(1609) & F(2192) & F(3363) & F(3134) & F(3629) & F(2037) & F(1694) & F(2151) & F(3084) & F(2381) & F(1677) & F(4432) & F(4291) & F(4009) & F(1836) & F(2229) & F(3531) & F(1606) & F(3505) & F(3641) & F(2222) & F(1759) & F(3029) & F(4309) & F(2016) & F(4048) & F(1624) & F(2390) & F(1541) & F(4410) & F(3770) & F(2899) & F(3839) & F(4379) & F(4866) & F(2494) & F(1909) & F(1773) & F(1744) & F(2166) & F(4286) & F(1946) & F(2803) & F(3063) & F(2882) & F(2696) & F(3953) & F(1851) & F(2431) & F(2716) & F(2227) & F(4020) & F(3547) & F(2519) & F(4427) & F(1849) & F(2291) & F(1659) & F(4850) & F(4338) & F(1697) & F(3540) & F(3601) & F(3090) & F(4879) & F(2750) & F(2239) & F(1552) & F(4402) & F(2052) & F(2347) & F(1755) & F(2725) & F(4826) & F(2185) & F(4843) & F(1549) & F(2874) & F(2169) & F(4380) & F(3825) & F(4426) & F(1775) & F(2660) & F(1885) & F(2011) & F(3088) & F(3743) & F(4384) & F(2067) & F(2344) & F(1961) & F(3692) & F(2686) & F(4069) & F(2642) & F(2117) & F(3776) & F(1679) & F(3605) & F(3674) & F(3957) & F(3840) & F(3863) & F(2256) & F(1981) & F(1595) & F(2630) & F(2071) & F(3838) & F(3485) & F(3612) & F(3689) & F(2434) & F(4044) & F(4296) & F(3104) & F(3594) & F(2485) & F(2954) & F(1607) & F(3034) & F(3986) & F(2768) & F(1752) & F(2584) & F(3992) & F(1854) & F(2083) & F(2791) & F(3789) & F(2061) & F(3690) & F(1614) & F(1641) & F(1905) & F(3575) & F(4864) & F(2247) & F(1680) & F(4440) & F(1611) & F(1745) & F(1647) & F(2881) & F(4320) & F(1563) & F(3591) & F(3551) & F(2463) & F(2385) & F(2264) & F(2194) & F(1912) & F(2774) & F(2159) & F(1649) & F(2230) & F(3662) & F(1966) & F(2329) & F(3542) & F(2279) & F(1910) & F(1940) & F(2974) & F(2569) & F(1698) & F(3632) & F(3127) & F(2299) & F(4324) & F(1808) & F(1793) & F(2403) & F(1753) & F(2744) & F(2562) & F(1968) & F(2516) & F(1592) & F(2614) & F(1795) & F(4299) & F(3757) & F(4395) & F(2090) & F(2365) & F(2870) & F(1630) & F(1834) & F(2325) & F(3041) & F(1916) & F(2111) & F(3103) & F(4321) & F(2379) & F(1831) & F(2292) & F(4834) & F(2044) & F(2008) & F(3569) & F(4019) & F(1937) & F(3515) & F(2207) & F(2900) & F(4081) & F(3667) & F(3969) & F(4003) & F(2491) & F(2758) & F(4090) & F(2087) & F(2594) & F(3599) & F(2810) & F(1770) & F(3693) & F(2271) & F(1869) & F(2020) & F(4055) & F(1596) & F(1853) & F(2590) & F(1784) & F(1929) & F(2259) & F(1638) & F(3808) & F(2708) & F(4325) & F(2248) & F(2691) & F(2615) & F(1960) & F(3831) & F(3772) & F(3931) & F(3657) & F(2867) & F(1976) & F(4383) & F(3468) & F(3585) & F(2074) & F(4827) & F(3742) & F(2951) & F(4046) & F(2375) & F(4413) & F(3895) & F(3043) & F(3846) & F(2055) & F(4025) & F(1785) & F(3553) & F(2131) & F(1682) & F(1660) & F(2613) & F(2742) & F(2323) & F(3481) & F(3800) & F(2199) & F(3608) & F(1685) & F(3976) & F(1799) & F(1559) & F(3936) & F(2526) & F(2246) & F(3566) & F(2754) & F(1570) & F(4043) & F(2699) & F(2258) & F(4297) & F(2054) & F(3598) & F(3736) & F(4846) & F(2096) & F(4869) & F(2896) & F(1578) & F(2300) & F(3035) & F(3705) & F(4880) & F(2336) & F(2078) & F(2941) & F(1730) & F(1635) & F(3589) & F(3710) & F(2902) & F(1859) & F(3924) & F(1550) & F(3581) & F(2176) & F(2281) & F(3951) & F(4288) & F(4289) & F(2947) & F(1820) & F(3488) & F(1620) & F(3483) & F(3824) & F(3894) & F(2731) & F(1691) & F(2824) & F(2554) & F(3099) & F(2123) & F(2720) & F(1566) & F(3651) & F(4382) & F(2813) & F(2030) & F(2506) & F(2872) & F(3617) & F(3940) & F(4358) & F(4396) & F(1542) & F(2241) & F(3492) & F(2208) & F(3560) & F(1804) & F(2597) & F(2627) & F(1604) & F(3760) & F(3855) & F(2692) & F(3729) & F(2301) & F(3933) & F(3565) & F(3564) & F(3055) & F(1914) & F(4406) & F(2797) & F(2418) & F(3886) & F(4056) & F(2007) & F(1904) & F(3915) & F(4340) & F(3817) & F(4829) & F(1948) & F(4894) & F(1897) & F(1893) & F(4035) & F(2154) & F(3663) & F(2211) & F(1709) & F(2081) & F(3672) & F(4849) & F(3530) & F(2032) & F(2556) & F(2866) & F(2789) & F(1715) & F(2939) & F(3010) & F(2503) & F(3610) & F(4877) & F(2975) & F(3106) & F(1581) & F(2360) & F(2036) & F(3859) & F(2548) & F(2543) & F(2619) & F(2056) & F(2389) & F(1643) & F(2404) & F(3643) & F(2101) & F(2212) & F(3621) & F(2001) & F(3815) & F(2306) & F(3576) & F(3943) & F(1932) & F(2773) & F(1783) & F(2013) & F(4409) & F(2295) & F(1556) & F(1958) & F(2109) & F(3638) & F(3113) & F(2798) & F(2891) & F(1584) & F(2129) & F(1965) & F(1594) & F(3946) & F(3652) & F(2982) & F(2245) & F(3628) & F(2499) & F(2023) & F(2666) & F(2621) & F(1787) & F(2302) & F(2432) & F(3016) & F(2115) & F(2400) & F(2966) & F(2502) & F(2607) & F(2197) & F(2826) & F(2656) & F(3053) & F(4029) & F(2486) & F(1588) & F(2012) & F(2275) & F(1920) & F(2592) & F(2684) & F(2751) & F(1658) & F(2679) & F(1944) & F(4084) & F(1766) & F(2433) & F(4428) & F(1728) & F(3093) & F(3571) & F(3819) & F(1743) & F(2146) & F(2125) & F(4890) & F(1780) & F(4857) & F(3559) & F(3867) & F(3882) & F(1626) & F(1587) & F(2603) & F(3554) & F(2546) & F(2560) & F(1829) & F(3990) & F(3991) & F(2653) & F(3914) & F(4051) & F(3844) & F(2251) & F(2349) & F(2114) & F(1774) & F(1827) & F(2893) & F(3656) & F(4353) & F(2650) & F(3759) & F(2578) & F(2322) & F(1664) & F(2728) & F(4300) & F(1980) & F(2680) & F(2512) & F(1693) & F(2206) & F(1657) & F(3503) & F(4342) & F(2984) & F(3766) & F(2466) & F(2488) & F(1850) & F(1857) & F(2171) & F(2244) & F(3959) & F(2787) & F(3807) & F(3023) & F(3984) & F(4871) & F(1843) & F(2394) & F(3644) & F(4868) & F(3673) & F(3983) & F(2333) & F(3826) & F(2504) & F(4008) & F(2315) & F(2571) & F(2828) & F(1712) & F(2048) & F(2221) & F(1737) & F(1740) & F(2904) & F(1639) & F(2510) & F(2952) & F(2950) & F(4314) & F(2285) & F(1908) & F(2232) & F(3723) & F(2209) & F(4230) & F(2237) & F(2372) & F(2304) & F(4842) & F(2793) & F(2191) & F(4854) & F(2662) & F(4381) & F(3683) & F(1861) & F(1855) & F(2575) & F(2429) & F(4070) & F(2149) & F(2357) & F(1999) & F(2386) & F(3491) & F(3537) & F(4231) & F(2409) & F(2961) & F(3744) & F(4001) & F(2051) & F(2493) & F(3868) & F(2458) & F(4307) & F(3909) & F(1727) & F(2663) & F(3701) & F(3768) & F(4398) & F(3874) & F(3913) & F(1555) & F(2799) & F(2522) & F(2099) & F(2497) & F(2698) & F(1586) & F(4328) & F(1662) & F(1764) & F(2378) & F(1953) & F(3947) & F(2293) & F(2601) & F(3754) & F(3633) & F(4068) & F(541) & F(3619) & F(2141) & F(2674) & F(3988) & F(4061) & F(1760) & F(2645) & F(2762) & F(2331) & F(2410) & F(1911) & F(1601) & F(2200) & F(3060) & F(3100) & F(2173) & F(3728) & F(2659) & F(3496) & F(3883) & F(1767) & F(3626) & F(3885) & F(4016) & F(2034) & F(1899) & F(2122) & F(1907) & F(2806) & F(2064) & F(2093) & F(1928) & F(1986) & F(3848) & F(3009) & F(2572) & F(2783) & F(2290) & F(3720) & F(4305) & F(1790) & F(2500) & F(3769) & F(3786) & F(2084) & F(4301) & F(4024) & F(1875) & F(2391) & F(2617) & F(2713) & F(2127) & F(4072) & F(4893) & F(1543) & F(3124) & F(2673) & F(2700) & F(2316) & F(2002) & F(2675) & F(2405) & F(3684) & F(3783) & F(3001) & F(2014) & F(2097) & F(4058) & F(2574) & F(2015) & F(3606) & F(1725) & F(2640) & F(3557) & F(3774) & F(4282) & F(1949) & F(2177) & F(1979) & F(3996) & F(2297) & F(2596) & F(1918) & F(2636) & F(3494) & F(3790) & F(2785) & F(3056) & F(3797) & F(3798) & F(2284) & F(3665) & F(3121) & F(3691) & F(3811) & F(3850) & F(4014) & F(2066) & F(1987) & F(3486) & F(3110) & F(2667) & F(2204) & F(2647) & F(2283) & F(2380) & F(1890) & F(3668) & F(2884) & F(3615) & F(2253) & F(1569) & F(1886) & F(2552) & F(3003) & F(3918) & F(1870) & F(3091) & F(1798) & F(2075) & F(3680) & F(3054) & F(3119) & F(2164) & F(3528) & F(1837) & F(1830) & F(1688) & F(3963) & F(3740) & F(2060) & F(3487) & F(2726) & F(3526) & F(3604) & F(2609) & F(4411) & F(2348) & F(2540) & F(2822) & F(1610) & F(4623) & F(3019) & F(3869) & F(3857) & F(1579) & F(4352) & F(3495) & F(3650) & F(2711) & F(2049) & F(2559) & F(1654) & F(3114) & F(3479) & F(4341) & F(1623) & F(2413) & F(3812) & F(4015) & F(2277) & F(1872) & F(3910) & F(1803) & F(2046) & F(1684) & F(4312) & F(4311) & F(1729) & F(4349) & F(2214) & F(4878) & F(2541) & F(3028) & F(1645) & F(1749) & F(1874) & F(2195) & F(3052) & F(2658) & F(2464) & F(4431) & F(3501) & F(1933) & F(2341) & F(2137) & F(2481) & F(3021) & F(3556) & F(3655) & F(2732) & F(3787) & F(3510) & F(3832) & F(3950) & F(3128) & F(2794) & F(3135) & F(2509) & F(3583) & F(4408) & F(2312) & F(2243) & F(1889) & F(2694) & F(1617) & F(4430) & F(4892) & F(2885) & F(3470) & F(3907) & F(4419) & F(2303) & F(3636) & F(2026) & F(3725) & F(2027) & F(3118) & F(3733) & F(2611) & F(3101) & F(1782) & F(1997) & F(3965) & F(2531) & F(1957) & F(4882) & F(4315) & F(1935) & F(2553) & F(3463) & F(1809) & F(3514) & F(3561) & F(1880) & F(2976) & F(2198) & F(2661) & F(3624) & F(2168) & F(2236) & F(3887) & F(1754) & F(2242) & F(1553) & F(2000) & F(2538) & F(2183) & F(3654) & F(1669) & F(3941) & F(2796) & F(1580) & F(2934) & F(3519) & F(2298) & F(4881) & F(1605) & F(2473) & F(3793) & F(2507) & F(3577) & F(2825) & F(3048) & F(1867) & F(2352) & F(3024) & F(4889) & F(1892) & F(4306) & F(2582) & F(3716) & F(1833) & F(2033) & F(1884) & F(2520) & F(3014) & F(1560) & F(2398) & F(3562) & F(3631) & F(2140) & F(3645) & F(3771) & F(3970) & F(4617) & F(4002) & F(2182) & F(3713) & F(3849) & F(1821) & F(1756) & F(2501) & F(1648) & F(2646) & F(1723) & F(3785) & F(1695) & F(2820) & F(2119) & F(4841) & F(4858) & F(2945) & F(3960) & F(2709) & F(2416) & F(3648) & F(3664) & F(1906) & F(3860) & F(3978) & F(4079) & F(4439) & F(3676) & F(2981) & F(2170) & F(2743) & F(2210) & F(2366) & F(1629) & F(4078) & F(4438) & F(2886) & F(4052) & F(2863) & F(2483) & F(2664) & F(3558) & F(2059) & F(4027) & F(2780) & F(2392) & F(4836) & F(3466) & F(3686) & F(2021) & F(2977) & F(1621) & F(4329) & F(4357) & F(2779) & F(2599) & F(3922) & F(1687) & F(2359) & F(3873) & F(1970) & F(3625) & F(3706) & F(4303) & F(1878) & F(4290) & F(3830) & F(3522) & F(2593) & F(2233) & F(2362) & F(2777) & F(4888) & F(3982) & F(4385) & F(3123) & F(4407) & F(3823) & F(2320) & F(1993) & F(4087) & F(2878) & F(2513) & F(2203) & F(1633) & F(4047) & F(3994) & F(1896) & F(3603) & F(1866) & F(3042) & F(2257) & F(1902) & F(3567) & F(3939) & F(1858) & F(2932) & F(3816) & F(1603) & F(2756) & F(3061) & F(2764) & F(3507) & F(1557) & F(4085) & F(2351) & F(4285) & F(3727) & F(4852) & F(4434) & F(4825) & F(2722) & F(3681) & F(1816) & F(3107) & F(2600) & F(2057) & F(4437) & F(4030) & F(2328) & F(2766) & F(1877) & F(3639) & F(2778) & F(2498) & F(3018) & F(1762) & F(1955) & F(2076) & F(4000) & F(4397) & F(2671) & F(2476) & F(1686) & F(3130) & F(2576) & F(2549) & F(1996) & F(3140) & F(2760) & F(2358) & F(3942) & F(2702) & F(4886) & F(2477) & F(1735) & F(3044) & F(2583)").unwrap(),
                ),
            ),
            super::NamedGraph::new(
                "split_static_str",
                super::ReachabilityGraph::find_reachable_deps(
                    &graph,
                    &testing::uniq_nodes("F(897)").unwrap(),
                ),
            ),
            super::NamedGraph::new(
                "split_string_from_static",
                super::ReachabilityGraph::find_reachable_deps(
                    &graph,
                    &testing::uniq_nodes("F(899)").unwrap(),
                ),
            ),
        ];

        let shared_entries = super::NamedGraph::calculate_shared_modules(&mut modules, &graph);

        let node = DepNode::Function(Id::from_index(1417));

        if !modules[2].deps.reachable.contains(&node) {
            return true;
        }

        let children = graph.get(&node).unwrap().clone();
        assert!(!children.is_empty());
        // make sure that all childs of specific node (that known to be in module 2) are included or linked
        for node in children {
            dbg!(node);
            let direct_dep = modules[2].deps.reachable.contains(&node);
            let linked_dep = modules[2].linked_nodes.contains(&node);

            if !direct_dep && !linked_dep {
                return false;
            }
        }
        return true;
    }

    fn reduce(source: &str, test: impl Fn(&str) -> bool) {
        let mut prefix = source.lines().collect::<Vec<_>>();

        let mut suffix = String::new();

        let max_iter = 4000;
        for _ in 0..max_iter {
            let mut new_prefix = prefix.clone();
            let Some(removed) = new_prefix.pop() else {
                break;
            };

            let mut file = File::create("./reduced.txt").unwrap();
            let mut source = new_prefix.join("\n");
            source.push_str(&suffix);

            if test(&source) {
                println!("removed: {}", removed);
                suffix = format!("\n{}{}", removed, suffix);
            } else {
                file.write_all(format!("{}\n", source).as_bytes()).unwrap();
                file.flush().unwrap();
                prefix = new_prefix;
            }
        }

        let mut source = prefix.join("\n");
        source.push_str(&suffix);
        eprintln!("Reduced to:\n{}", source);
    }
    #[test]
    fn test_extracted_from_example() {
        // SPLIT: ============== split_static_str
        // func[29] <Some("_ZN8dlmalloc8dlmalloc17Dlmalloc$LT$A$GT$6malloc17hd2421ed15302c503E")> (size=4811)
        // ==>func[73] <Some("_ZN8dlmalloc8dlmalloc17Dlmalloc$LT$A$GT$12unlink_chunk17h0d0f475ea8907bb5E")> (size=386)
        // ==>data[.bss:4] <"_ZN3std3sys5alloc4wasm8DLMALLOC17h4db028f137b44949E"> (size=452)
        // ==>func[662] <Some("_ZN8dlmalloc8dlmalloc17Dlmalloc$LT$A$GT$18insert_large_chunk17ha51abd6cd7031e06E")> (size=323)
        // func[64] <Some("__wasm_split_00static_str00_export_776d8e51aac0782b17e57d45872c52d7_static_str")> (size=50)
        // ==>data[.rodata:18] <".Lanon.8353eafc0686ece12291357d89b43616.17"> (size=9)
        // ==>func[29] <Some("_ZN8dlmalloc8dlmalloc17Dlmalloc$LT$A$GT$6malloc17hd2421ed15302c503E")> (size=4811)
        // ==>data[.bss:0] <"__rust_no_alloc_shim_is_unstable"> (size=1)
        // func[662] <Some("_ZN8dlmalloc8dlmalloc17Dlmalloc$LT$A$GT$18insert_large_chunk17ha51abd6cd7031e06E")> (size=323)
        // ==>data[.bss:4] <"_ZN3std3sys5alloc4wasm8DLMALLOC17h4db028f137b44949E"> (size=452)
        // SPLIT: ============== split_static_str  : total size: 6032

        // SPLIT: ============== split_string_from_static
        // func[660] <Some("_ZN3std9panicking19begin_panic_handler28_$u7b$$u7b$closure$u7d$$u7d$17h83b3d84f04c7372bE")> (size=186)
        // ==>func[661] <Some("_ZN99_$LT$std..panicking..begin_panic_handler..StaticStrPayload$u20$as$u20$core..panic..PanicPayload$GT$6as_str17h942de68efd2647a4E")> (size=12)
        // ==>func[654] <Some("_ZN4core5panic12PanicPayload6as_str17had35548f12cb95a5E")> (size=9)
        // ==>func[655] <Some("_ZN3std9panicking20rust_panic_with_hook17hb39abb160cd4038cE")> (size=153)
        // func[29] <Some("_ZN8dlmalloc8dlmalloc17Dlmalloc$LT$A$GT$6malloc17hd2421ed15302c503E")> (size=4811)
        // ==>data[.bss:4] <"_ZN3std3sys5alloc4wasm8DLMALLOC17h4db028f137b44949E"> (size=452)
        // ==>func[73] <Some("_ZN8dlmalloc8dlmalloc17Dlmalloc$LT$A$GT$12unlink_chunk17h0d0f475ea8907bb5E")> (size=386)
        // ==>func[662] <Some("_ZN8dlmalloc8dlmalloc17Dlmalloc$LT$A$GT$18insert_large_chunk17ha51abd6cd7031e06E")> (size=323)
        // data[.rodata:26] <".Lanon.b3ea03665832a4ba8902f71b0f46aa8d.4"> (size=8)
        // ==>data[.rodata:97] <".Lanon.b3ea03665832a4ba8902f71b0f46aa8d.3"> (size=17)
        // func[659] <Some("_ZN3std3sys9backtrace26__rust_end_short_backtrace17h8eb99c908c86e40bE")> (size=11)
        // ==>func[660] <Some("_ZN3std9panicking19begin_panic_handler28_$u7b$$u7b$closure$u7d$$u7d$17h83b3d84f04c7372bE")> (size=186)
        // func[662] <Some("_ZN8dlmalloc8dlmalloc17Dlmalloc$LT$A$GT$18insert_large_chunk17ha51abd6cd7031e06E")> (size=323)
        // ==>data[.bss:4] <"_ZN3std3sys5alloc4wasm8DLMALLOC17h4db028f137b44949E"> (size=452)
        // data[.rodata:20] <".Lanon.8353eafc0686ece12291357d89b43616.11"> (size=16)
        // ==>data[.rodata:95] <".Lanon.8353eafc0686ece12291357d89b43616.10"> (size=113)
        // func[655] <Some("_ZN3std9panicking20rust_panic_with_hook17hb39abb160cd4038cE")> (size=153)
        // ==>func[656] <Some("rust_panic")> (size=3)
        // ==>data[.bss:10] <"_ZN3std9panicking4HOOK17h8444376eb767869dE.0"> (size=4)
        // ==>data[.bss:9] <"_ZN3std9panicking11panic_count17LOCAL_PANIC_COUNT28_$u7b$$u7b$closure$u7d$$u7d$3VAL17h66e0a8e292221c9bE.0"> (size=4)
        // ==>data[.bss:8] <"_ZN3std9panicking11panic_count17LOCAL_PANIC_COUNT28_$u7b$$u7b$closure$u7d$$u7d$3VAL17h66e0a8e292221c9bE.1"> (size=1)
        // ==>data[.bss:7] <"_ZN3std9panicking11panic_count18GLOBAL_PANIC_COUNT17hf4d1d45d81f74171E"> (size=4)
        // func[66] <Some("_ZN5alloc7raw_vec12handle_error17hcd6c5f33527353caE")> (size=18)
        // ==>func[77] <Some("_ZN5alloc7raw_vec17capacity_overflow17hbc71c29d4abc75a0E")> (size=67)
        // func[101] <Some("rust_begin_unwind")> (size=56)
        // ==>func[659] <Some("_ZN3std3sys9backtrace26__rust_end_short_backtrace17h8eb99c908c86e40bE")> (size=11)
        // func[77] <Some("_ZN5alloc7raw_vec17capacity_overflow17hbc71c29d4abc75a0E")> (size=67)
        // ==>data[.rodata:26] <".Lanon.b3ea03665832a4ba8902f71b0f46aa8d.4"> (size=8)
        // ==>func[78] <Some("_ZN4core9panicking9panic_fmt17h6f4dae69dcc1a6d2E")> (size=54)
        // func[65] <Some("__wasm_split_00string_from_static00_export_17997317cc392c52ed3bea15880aab65_string_from_static")> (size=128)
        // ==>data[.rodata:20] <".Lanon.8353eafc0686ece12291357d89b43616.11"> (size=16)
        // ==>func[66] <Some("_ZN5alloc7raw_vec12handle_error17hcd6c5f33527353caE")> (size=18)
        // ==>data[.bss:0] <"__rust_no_alloc_shim_is_unstable"> (size=1)
        // ==>func[29] <Some("_ZN8dlmalloc8dlmalloc17Dlmalloc$LT$A$GT$6malloc17hd2421ed15302c503E")> (size=4811)
        // ==>data[.rodata:19] <".Lanon.8353eafc0686ece12291357d89b43616.20"> (size=10)
        // func[78] <Some("_ZN4core9panicking9panic_fmt17h6f4dae69dcc1a6d2E")> (size=54)
        // ==>func[101] <Some("rust_begin_unwind")> (size=56)
        // SPLIT: ============== split_string_from_static  : total size: 6847

        let graph = testing::parse_deps(
            r#"
            F(29) -> F(73) & D(2, 4) & F(662)
            F(64) -> D(0, 18) & F(29) & D(2, 0)
            F(65) -> D(0, 20) & F(66) & D(2, 0) & F(29) & D(0, 19)
            F(66) -> F(77)
            F(77) -> D(0, 26) & F(78)
            F(78) -> F(101)
            F(101) -> F(659)
            F(655) -> F(656) & D(2, 10) & D(2, 9) & D(2, 8)
            F(659) -> F(660)
            F(660) -> F(661) & F(654) & F(655)
            F(662) -> D(2, 4)
            D(0, 26) -> D(0, 97)
            D(0, 20) -> D(0, 95)
            F(1000) -> D(2, 4) & D(0, 1000) & F(73)
            F(2000) -> F(1000)
            "#,
        )
        .unwrap();
        let mut modules = vec![
            super::NamedGraph::new(
                "split_static_str",
                super::ReachabilityGraph::find_reachable_deps(
                    &graph,
                    &testing::uniq_nodes("F(64)").unwrap(),
                ),
            ),
            super::NamedGraph::new(
                "split_string_from_static",
                super::ReachabilityGraph::find_reachable_deps(
                    &graph,
                    &testing::uniq_nodes("F(65)").unwrap(),
                ),
            ),
            super::NamedGraph::new(
                "main",
                super::ReachabilityGraph::find_reachable_deps(
                    &graph,
                    &testing::uniq_nodes("F(2000)").unwrap(),
                ),
            ),
        ];
        let shared_entries = super::NamedGraph::calculate_shared_modules(&mut modules, &graph);

        for module in &[&modules[0], &modules[1]] {
            assert_eq!(
                module.linked_nodes,
                testing::uniq_nodes("F(29) & D(2, 0)").unwrap()
            );
        }

        assert_eq!(shared_entries.len(), 2);
        assert_eq!(
            shared_entries[0].module_names,
            vec!["split_static_str", "split_string_from_static"]
        );

        // F(29) -> F(73) & D(2, 4) & F(662)
        // But F(73) and D(2, 4) are included into shared_entries[1]
        // so we have only F(29) and F(662) from this tree
        // D(2, 0) is separate dependency
        assert_eq!(
            shared_entries[0].shared_deps,
            testing::uniq_nodes("D(2, 0) & F(29) & F(662)").unwrap()
        );
        assert_eq!(
            shared_entries[0].linked_nodes,
            testing::uniq_nodes("F(29) & D(2, 0)").unwrap()
        );

        // For second chunk - all deps are exported
        assert_eq!(
            shared_entries[1].module_names,
            vec!["split_static_str", "split_string_from_static", "main"]
        );

        assert_eq!(
            shared_entries[1].shared_deps,
            testing::uniq_nodes("F(73) & D(2, 4)").unwrap()
        );
        assert_eq!(
            shared_entries[1].linked_nodes,
            testing::uniq_nodes("F(73) & D(2, 4)").unwrap()
        );

        // top-most shared entries should be only F(29) and D(2, 0)
        assert_eq!(
            modules[2].linked_nodes,
            testing::uniq_nodes("F(73) & D(2, 4)").unwrap()
        );
    }
}
