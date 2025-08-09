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
    index::{DataSegmentId, DataSymbolId},
    read::InputModule,
};
use crate::{
    index::{InputFuncId, SymbolId},
    read::linking::SymbolIndex,
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
    fn get_symbol_dep_node(&self, symbol_index: SymbolId) -> Option<DepNode>;
}

impl<'a> SymbolTable for InputModule<'a> {
    fn get_symbol_dep_node(&self, linking_index: SymbolId) -> Option<DepNode> {
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
            seen.insert(node);
            let Some(children) = deps.get(&node) else {
                continue;
            };
            for child in children {
                if seen.contains(&child) {
                    continue;
                }
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

        let mut seen = HashSet::<DepNode>::new();
        while let Some(node) = queue.pop_front() {
            seen.insert(node);

            // Can be unreachable if cycle in graph
            let _reachable = self.reachable.remove(&node);

            if let Some(childs) = graph.get(&node) {
                for child in childs {
                    let child_parent = self.parents.get_mut(&child).expect("Parent should exist");
                    child_parent.remove(&node);
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

    use testing::tests::function;

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
            shared_entries[1].shared_deps, // F(7) and childs are stored in [m1,m2,m3] shared deps
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
