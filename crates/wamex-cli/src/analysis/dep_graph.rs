use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt::Debug,
};

use crate::{
    analysis::{self},
    index::{Id, IdMap, SymbolId},
};

pub type DepSet<T = SymbolId> = BTreeSet<T>;
pub type DepMiniSet<T = SymbolId> = wamex_types::map_vec::MiniSet<T>;

#[derive(Clone, Default)]
struct SymbolStructure {
    pub parents: DepMiniSet,
    pub childs: DepMiniSet,
}
#[derive(Clone, Default)]
pub struct DepGraph {
    // TODO: use VecMap kind of structure for better performance
    nodes: IdMap<SymbolId, SymbolStructure>,
}
impl DepGraph {
    pub fn new() -> Self {
        Self {
            nodes: IdMap::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn insert_child(&mut self, parent: SymbolId, child: SymbolId) {
        let parent_struct = self.nodes.entry(parent).or_insert_with(Default::default);
        parent_struct.childs.insert(child);

        let child_struct = self.nodes.entry(child).or_insert_with(Default::default);
        child_struct.parents.insert(parent);
    }

    pub fn get_children(&self, key: SymbolId) -> Option<&DepMiniSet> {
        self.nodes.get(key).map(|s| &s.childs)
    }
    pub fn get_parents(&self, key: SymbolId) -> Option<&DepMiniSet> {
        self.nodes.get(key).map(|s| &s.parents)
    }
    pub fn iter_childs(&self) -> impl Iterator<Item = (SymbolId, &DepMiniSet)> {
        self.nodes.iter().map(|(k, v)| (k, &v.childs))
    }
}

impl Debug for DepGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (node, deps) in self.iter_childs() {
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

#[derive(Debug, Clone)]
pub struct NamedGraph<Id> {
    pub module: Id,
    pub reachable: DepSet,

    /// This field is hidden, because it is output parameter of `calculate_shared_modules`
    imports: DepMiniSet,
}

impl<Id> NamedGraph<Id> {
    pub fn new(module: Id, reachable: DepSet) -> Self {
        Self {
            module,
            reachable,
            imports: DepMiniSet::new(),
        }
    }
    pub fn imports(&self) -> &DepMiniSet {
        &self.imports
    }
}

#[derive(Debug, Clone)]
pub struct SharedEntries<Id> {
    pub module_names: Vec<Id>,
    pub shared_deps: DepSet,
    pub exports: DepMiniSet,
    pub imports: DepMiniSet,
}

pub fn get_dependencies(info: &analysis::ModuleInfo) -> anyhow::Result<DepGraph> {
    let mut deps = DepGraph::new();

    // TypeIndexLeb relocations index is not in symbol index space.
    let non_type_index = |entry: &&wasmparser::RelocationEntry| {
        use wasmparser::RelocationType;
        !matches!(entry.ty, RelocationType::TypeIndexLeb)
    };
    let is_fn_or_data = |id: &SymbolId| info.symbols.is_function(*id) || info.symbols.is_data(*id);

    for (id, child) in info.symbols.iter() {
        if !is_fn_or_data(&id) {
            continue;
        }
        let childs = DepMiniSet::from_iter(
            child
                .relocs
                .iter()
                .filter(non_type_index)
                .map(|entry| Id::from_index(entry.index))
                .filter_map(|index| info.symbols.as_duplicate_mapped(index).or(Some(index)))
                .filter(is_fn_or_data),
        );

        for child_id in &childs {
            let child_struct = deps.nodes.entry(*child_id).or_insert_with(Default::default);
            child_struct.parents.insert(id);
        }

        deps.nodes.entry(id).or_insert_with(Default::default).childs = childs;
    }

    Ok(deps)
}

// traverse the dep graph starting from roots and return all reachable nodes
pub fn find_reachable_deps(deps: &DepGraph, roots: &DepSet) -> DepSet {
    let mut queue: VecDeque<_> = roots.iter().copied().collect();
    let mut seen = DepSet::new();

    while let Some(node) = queue.pop_front() {
        if !seen.insert(node) {
            continue;
        }

        let Some(children) = deps.get_children(node) else {
            continue;
        };
        for child in children {
            queue.push_back(*child);
        }
    }
    seen
}

impl<Id> NamedGraph<Id> {
    /// Collect list of modules that owns a given dep node
    /// Returns a map of dep node to set of module ids that owns it
    fn collect_visited_by(modules: &[NamedGraph<Id>]) -> BTreeMap<SymbolId, DepSet<usize>> {
        let mut visited_by: BTreeMap<SymbolId, DepSet<usize>> = BTreeMap::new();
        for (module_id, module) in modules.iter().enumerate() {
            for dep in module.reachable.iter() {
                visited_by.entry(*dep).or_default().insert(module_id);
            }
        }
        visited_by
    }

    /// List only shared entries that have parents in module entries.
    /// This will collect nodes that module entries imports from shared entries.
    pub fn reduce_shared_entries(
        shared_entries: &DepSet,
        module_entries: &DepSet,
        graph: &DepGraph,
    ) -> DepSet {
        let mut reduced = DepSet::new();
        for dep in shared_entries {
            if let Some(parent) = graph.get_parents(*dep) {
                if !parent.iter().any(|p| module_entries.contains(p)) {
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
        Id: Clone + Ord + Debug,
    {
        //TODO: use bitset as key instead
        let mut shared_entries: BTreeMap<Vec<usize>, DepSet> = BTreeMap::new();

        for m in modules.iter() {
            debug_assert!(m.imports.is_empty(), "Linked nodes is output parameter");
        }

        let visited_by = Self::collect_visited_by(modules);

        for (dep, owner_modules) in visited_by {
            if owner_modules.len() > 1 {
                for module_id in &owner_modules {
                    let module = &mut modules[*module_id];
                    module.reachable.remove(&dep);
                }
                let mut owner_modules: Vec<usize> = owner_modules.into_iter().collect();
                owner_modules.sort_unstable();
                shared_entries.entry(owner_modules).or_default().insert(dep);
            }
        }

        let mut result = Vec::new();
        for (module_ids, shared_deps) in shared_entries {
            let mut module_names = Vec::new();
            let mut shared_exports = DepMiniSet::new();

            for module_id in module_ids {
                let module = &mut modules[module_id];

                let top_shared_deps =
                    Self::reduce_shared_entries(&shared_deps, &module.reachable, graph);
                // imports
                module.imports.extend(top_shared_deps.clone());
                // exports
                shared_exports.extend_and_resort(top_shared_deps.into_iter());
                module_names.push(module.module.clone());
            }
            result.push(SharedEntries {
                module_names,
                shared_deps,
                exports: shared_exports,
                imports: DepMiniSet::new(),
            });
        }

        // For each dep -> child if child is not found in deps: add it to linked_nodes
        // Imports
        for shared in result.iter_mut() {
            let new_imports = shared
                .shared_deps
                .iter()
                .filter_map(|d| graph.get_children(*d))
                .flatten()
                .filter(|child| !shared.shared_deps.contains(child))
                .copied();

            let mut new_exports = Vec::new();
            for dep in &shared.shared_deps {
                if let Some(parents) = graph.get_parents(*dep) {
                    for parent in parents {
                        if !shared.shared_deps.contains(parent) {
                            new_exports.push(*dep);
                        }
                    }
                }
            }

            shared.imports.extend_and_resort(new_imports);
            shared.exports.extend_and_resort(new_exports);
        }
        // TODO For export imports we can just build a map (dep -> owner) then
        // map((dep-> owner), dep -> (children, parents) ) -> (dep -> imports/exports)

        result.sort_by(|left, right| left.module_names.cmp(&right.module_names));
        result
    }
}

#[cfg(test)]
mod tests {
    use std::{fs::File, io::Write};

    use lazy_static::lazy_static;

    use crate::{
        analysis::{
            self,
            debug::print_deps_inner,
            dep_graph::{DepGraph, DepSet},
            symbols::SymbolKind,
            testing,
        },
        index::{Id, SymbolId},
    };

    trait DepListExt {
        fn check_unreachable(&self, other: &DepSet) -> bool;
        fn print(&self, title: &str, info: &analysis::ModuleInfo, graph: &DepGraph);
    }
    impl DepListExt for DepSet {
        // Check that self list of deps does not contain any nodes from other list
        fn check_unreachable(&self, other: &DepSet) -> bool {
            for dep in other {
                if self.contains(dep) {
                    log::warn!("Unreachable dep {dep:?} found in deps");
                    return false;
                }
            }
            true
        }
        fn print(&self, title: &str, info: &analysis::ModuleInfo, graph: &DepGraph) {
            print_deps_inner(title, info, self, graph);
        }
    }
    // checkout test-data/simple-graph crate at root (just keep wasm in case rustc changes)
    const WASM_FILE: &[u8] = include_bytes!("../../test-data/simple_graph.wasm");

    #[test]
    fn load_dep_graph() {
        let info = analysis::ModuleInfo::from_wasm_bytes(&WASM_FILE).unwrap();
        let dep_graph = super::get_dependencies(&info).unwrap();

        let format_dep = |dep: SymbolId| {
            let symbol = info.symbols.get(dep).unwrap();
            let name = &symbol.name;
            match symbol.kind {
                SymbolKind::Func { input_id } => {
                    format!("func[{input_id}] <{name:?}>")
                }
                SymbolKind::DataDefined {
                    segment_id,
                    offset,
                    length,
                } => {
                    format!("data[{segment_id}:{offset}:{length}] <{name:?}>")
                }
                _ => panic!("unexpected symbol kind"),
            }
        };

        for (node, deps) in dep_graph.iter_childs() {
            println!("node: {node}", node = format_dep(node));
            for dep in deps {
                println!("  =>{dep}", dep = format_dep(*dep));
            }
        }

        let no_inline_fn = info.find_function_id_by_name("no_inline_fn").unwrap();
        let no_inline_fn_sym = info.symbols.get_function_symbol(no_inline_fn).unwrap();

        let deps = dep_graph.get_children(no_inline_fn_sym).unwrap();
        let func_deps: Vec<_> = deps
            .iter()
            .filter(|dep| info.symbols.is_function(**dep))
            .collect();
        let data_deps: Vec<_> = deps
            .iter()
            .filter(|dep| info.symbols.is_data(**dep))
            .collect();

        assert_eq!(func_deps.len(), 3);
        // no_inline_fn is really inline data, but keep method call for "side effect"
        assert_eq!(data_deps.len(), 3);

        let indirect_fn = info.find_function_id_by_name("indirect_fn").unwrap();
        let indirect_fn_sym = info.symbols.get_function_symbol(indirect_fn).unwrap();

        let deps = dep_graph.get_children(indirect_fn_sym).unwrap();
        assert_eq!(deps.len(), 1); // only dep on switchtable
        let switch_table = *deps.iter().next().unwrap();
        assert!(matches!(
            info.symbols.get(switch_table).unwrap().kind,
            SymbolKind::DataDefined { .. }
        ));
        let fns = dep_graph.get_children(switch_table).unwrap();

        assert_eq!(fns.len(), 3);
    }

    #[test]
    fn reachablity_graph() {
        let info = analysis::ModuleInfo::from_wasm_bytes(&WASM_FILE).unwrap();
        let dep_graph = super::get_dependencies(&info).unwrap();

        let no_inline_fn = info.find_function_id_by_name("no_inline_fn").unwrap();
        let no_inline_fn_sym = info.symbols.get_function_symbol(no_inline_fn).unwrap();

        let reachability_graph =
            super::find_reachable_deps(&dep_graph, &DepSet::from_iter([no_inline_fn_sym]));
        // no_inline_fn -> data1
        //              -> data2
        //              -> data3
        //              -> func1 -> data1
        //              -> func2 -> data2
        //              -> func3 -> data3
        reachability_graph.print("no_inline_fn", &info, &dep_graph);
        assert_eq!(reachability_graph.len(), 7); // root +  3 data + 3 funcs

        let indirect_fn = info.find_function_id_by_name("indirect_fn").unwrap();
        let reachability_graph = super::find_reachable_deps(
            &dep_graph,
            &DepSet::from_iter([info.symbols.get_function_symbol(indirect_fn).unwrap()]),
        );
        reachability_graph.print("indirect_fn", &info, &dep_graph);
        // almost same count, but indirect_fn has more deep graph and switchtable
        // indirect_fn -> switchtable -> func1 -> data1
        //                             -> func2 -> data2
        //                             -> func3 -> data3
        assert_eq!(reachability_graph.len(), 8); // root + <switchtable> +  3 data + 3 funcs
    }

    lazy_static! {
        static ref TEST_GRAPH: DepGraph = testing::parse_deps(
            r#"
            1 -> 2 & 4 -> 5 & 7 -> 8
            11 -> 4 & 12
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
                super::find_reachable_deps(&graph, &testing::uniq_nodes("1").unwrap()),
            ),
            super::NamedGraph::new(
                "module2",
                super::find_reachable_deps(&graph, &testing::uniq_nodes("11").unwrap()),
            ),
        ];

        let first_module = &modules[0];
        let first_graph = testing::uniq_nodes("1 & 2 & 4 & 5 & 7 & 8").unwrap();

        assert_eq!(first_module.reachable, first_graph);
        assert!(
            first_module
                .reachable
                .check_unreachable(&testing::uniq_nodes("11 & 12").unwrap())
        );

        let second_module = &modules[1];
        let second_graph = testing::uniq_nodes("11 & 12 & 4 & 5 & 7 & 8").unwrap();
        assert_eq!(second_module.reachable, second_graph);

        assert!(
            second_module
                .reachable
                .check_unreachable(&testing::uniq_nodes("1 & 2 & 3").unwrap())
        );
    }

    #[test]
    fn test_shared_entries() {
        let graph = TEST_GRAPH.clone();
        let mut modules = vec![
            super::NamedGraph::new(
                "module1",
                super::find_reachable_deps(&graph, &testing::uniq_nodes("1").unwrap()),
            ),
            super::NamedGraph::new(
                "module2",
                super::find_reachable_deps(&graph, &testing::uniq_nodes("11").unwrap()),
            ),
        ];

        let shared_entries = super::NamedGraph::calculate_shared_modules(&mut modules, &graph);

        assert_eq!(shared_entries.len(), 1);
        assert_eq!(shared_entries[0].module_names, vec!["module1", "module2"]);
        assert_eq!(
            shared_entries[0].shared_deps,
            testing::uniq_nodes("5 & 8 & 4 & 7").unwrap()
        );

        // Test that top_most_dep contains only 4
        // It is top-most because it doesn't depend on other shared dependencies
        // 7, 5 and 8 are not top-most because 4 -> 5 & 7 and 7 -> 8
        for module in &modules {
            assert_eq!(module.imports, testing::uniq_nodes("4").unwrap());
        }
    }

    #[test]
    fn test_multiple_shared_deps() {
        let input = r#"
        1 -> 102 & 11 & 4 -> 115
        11 -> 112 -> 12 & 4 & 7 -> 118
        10 -> 111 & 7
        20 -> 4 & 7
        "#;
        let mut modules = vec![
            super::NamedGraph::new(
                "module1",
                super::find_reachable_deps(
                    &testing::parse_deps(input).unwrap(),
                    &testing::uniq_nodes("1").unwrap(),
                ),
            ),
            super::NamedGraph::new(
                "module2",
                super::find_reachable_deps(
                    &testing::parse_deps(input).unwrap(),
                    &testing::uniq_nodes("10").unwrap(),
                ),
            ),
            super::NamedGraph::new(
                "module3",
                super::find_reachable_deps(
                    &testing::parse_deps(input).unwrap(),
                    &testing::uniq_nodes("20").unwrap(),
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
            testing::uniq_nodes("118 & 7").unwrap()
        );

        assert_eq!(modules[1].imports, testing::uniq_nodes("7").unwrap());
        assert_eq!(shared_entries[1].module_names, vec!["module1", "module3"]);
        assert_eq!(
            shared_entries[1].shared_deps, // 7 and children are stored in [m1,m2,m3] shared deps
            testing::uniq_nodes("4 & 115").unwrap()
        );
        for module in &[&modules[0], &modules[2]] {
            assert_eq!(module.imports, testing::uniq_nodes("4 & 7").unwrap());
        }
    }

    #[test]
    fn test_recursive_shared_deps() {
        // F4 is parent of F7 which call F4
        let input = r#"
        1 -> 102 & 4 -> 105 & 7 -> 108 & 4
        10 -> 111 -> 4
        "#;
        let mut modules = vec![
            super::NamedGraph::new(
                "module1",
                super::find_reachable_deps(
                    &testing::parse_deps(input).unwrap(),
                    &testing::uniq_nodes("1").unwrap(),
                ),
            ),
            super::NamedGraph::new(
                "module2",
                super::find_reachable_deps(
                    &testing::parse_deps(input).unwrap(),
                    &testing::uniq_nodes("10").unwrap(),
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
            testing::uniq_nodes("4 & 105 & 7 & 108").unwrap()
        );
        for module in &modules {
            assert_eq!(module.imports, testing::uniq_nodes("4").unwrap());
        }
    }

    #[test]
    fn test_shared_deps_reduced() {
        let source = r#"
    899  -> 307 & 912
    1358 -> 1417 & 1418 & 4759
    124  -> 4759
    307  -> 308 & 124
    912  -> 1358
    1    -> 307
            "#;
        let graph = testing::parse_deps(source).unwrap();

        let mut modules = vec![
            super::NamedGraph::new(
                "main",
                super::find_reachable_deps(&graph, &testing::uniq_nodes("1").unwrap()),
            ),
            super::NamedGraph::new(
                "split_string_from_static",
                super::find_reachable_deps(&graph, &testing::uniq_nodes("899").unwrap()),
            ),
        ];

        dbg!(&modules);

        let shared_entries = super::NamedGraph::calculate_shared_modules(&mut modules, &graph);

        dbg!(&shared_entries);
        let node = Id::from_index(1417);

        assert!(modules[1].reachable.contains(&node));

        let roots = [node].into_iter().collect();
        let child = super::find_reachable_deps(&graph, &roots);

        assert!(!child.is_empty());
        // make sure that all childs of specific node (that known to be in module 2) are included or linked
        for node in child {
            dbg!(node);
            let direct_dep = modules[1].reachable.contains(&node);
            let linked_dep = modules[1].imports.contains(&node);

            assert!(direct_dep || linked_dep);
        }
    }

    const INPUT: &str = r#"
    1358 -> 1359 & 1366 & 1417
    4453 -> 4800 & 4759
    912  -> 1358
    4452 -> 4453
    4630 -> 4452 & 4464
    4759 -> 4671
    1417 -> 1418
    1418 -> 4759
    899  -> 912 & 307 & 916
    4671 -> 4663
                "#;
    #[test]
    fn test_deps_in_shared_conflict() {
        assert!(test_deps_in_shared_conflict_impl(INPUT));
    }

    #[test]
    #[ignore = "takes too long time"]
    fn test_reduce_deps_in_shared_conflict() {
        reduce(INPUT, test_deps_in_shared_conflict_impl);
    }

    fn test_deps_in_shared_conflict_impl(source: &str) -> bool {
        let graph = testing::parse_deps(source).unwrap();

        let mut modules = vec![
            super::NamedGraph::new(
                "main",
                super::find_reachable_deps(&graph, &testing::uniq_nodes("4630").unwrap()),
            ),
            super::NamedGraph::new(
                "split_static_str",
                super::find_reachable_deps(&graph, &testing::uniq_nodes("897").unwrap()),
            ),
            super::NamedGraph::new(
                "split_string_from_static",
                super::find_reachable_deps(&graph, &testing::uniq_nodes("899").unwrap()),
            ),
        ];

        let _shared_entries = super::NamedGraph::calculate_shared_modules(&mut modules, &graph);

        let node = Id::from_index(1417);

        if !modules[2].reachable.contains(&node) {
            return true;
        }

        let children = graph.get_children(node).unwrap().clone();
        assert!(!children.is_empty());
        // make sure that all childs of specific node (that known to be in module 2) are included or linked
        for node in children {
            dbg!(node);
            let direct_dep = modules[2].reachable.contains(&node);
            let linked_dep = modules[2].imports.contains(&node);

            if !direct_dep && !linked_dep {
                return false;
            }
        }
        return true;
    }

    #[test]
    fn test_of_shared_dep_of_shared() {
        let source = r#"
    1 ->  2 -> 3 -> 4
    11 -> 12 -> 13 -> 14
    21 -> 22 -> 23 -> 24
    31 -> 32 -> 33 -> 34
    100 -> 101 -> 102
    22 -> 100 & 201
    32 -> 100 & 201
    12 -> 101 & 301
    "#;
        let graph = testing::parse_deps(source).unwrap();

        let mut modules = vec![
            super::NamedGraph::new(
                "mod1",
                super::find_reachable_deps(&graph, &testing::uniq_nodes("1").unwrap()),
            ),
            super::NamedGraph::new(
                "mod2",
                super::find_reachable_deps(&graph, &testing::uniq_nodes("11").unwrap()),
            ),
            super::NamedGraph::new(
                "mod3",
                super::find_reachable_deps(&graph, &testing::uniq_nodes("21").unwrap()),
            ),
            super::NamedGraph::new(
                "mod4",
                super::find_reachable_deps(&graph, &testing::uniq_nodes("31").unwrap()),
            ),
        ];

        // So 1 not share anything
        // 11, 21, 31 are sharing 101
        // But 21 & 31 are sharing also 101 parent 100
        let shared_entries = super::NamedGraph::calculate_shared_modules(&mut modules, &graph);
        assert_eq!(shared_entries.len(), 2);
        let first = &shared_entries[0];
        assert_eq!(
            first.module_names,
            &["mod2".to_string(), "mod3".to_string(), "mod4".to_string()]
        );

        // 101 exported, but 101 and 102 are both defined
        assert!(first.exports.contains(&Id::from_index(101)));
        assert!(first.shared_deps.contains(&Id::from_index(101)));
        assert!(first.shared_deps.contains(&Id::from_index(102)));

        let second = &shared_entries[1];
        assert_eq!(
            second.module_names,
            &["mod3".to_string(), "mod4".to_string()]
        );

        dbg!(&second);
        // 100 are exported and defined (201 also defined, but not interesting here)
        assert!(!second.imports.contains(&Id::from_index(100)));
        assert!(second.exports.contains(&Id::from_index(100)));
        assert!(second.shared_deps.contains(&Id::from_index(100)));
        // 101 are imported only
        assert!(second.imports.contains(&Id::from_index(101)));
        assert!(!second.exports.contains(&Id::from_index(101)));
        assert!(!second.shared_deps.contains(&Id::from_index(101)));
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
}
