use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::{Debug, Display},
};

use cranelift_entity::SecondaryMap;
use helpers::*;
use wamex_types::map_vec::MiniSet;

pub use self::{
    helpers::wbg_closures,
    parse::{SplitPointExtractor, find_split_points, find_split_points_legacy},
};
use super::dep_graph::{DepGraph, DepMiniSet, DepSet, NamedGraph, find_reachable_deps};
use crate::{
    analysis::dep_graph::SharedEntry,
    typed::{
        FunctionRef, Module,
        snapshot::{EntitiesSnapshot, FlatEntityRef},
    },
};

pub mod helpers;
pub mod modify;
pub mod parse;
/// Split-point entrypoint pair (import stub + export impl).
///
/// Note: the algorithm that discovers split points lives in `wamex-cli`.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct SplitPoint {
    pub module_name: String,
    pub unique_id: String,
    import_func: FunctionRef,
    export_func: FunctionRef,
}

impl SplitPoint {
    pub fn new(
        module_name: String,
        unique_id: String,
        import_func: FunctionRef,
        export_func: FunctionRef,
    ) -> Self {
        Self {
            module_name,
            unique_id,
            import_func,
            export_func,
        }
    }
    pub fn import_func(&self) -> FunctionRef {
        self.import_func
    }
    pub fn export_func(&self) -> FunctionRef {
        self.export_func
    }
}

/// Content plan for a single emitted module.
#[derive(Default, Clone)]
pub struct OutputModuleInfo {
    pub defined_symbols: BTreeSet<FlatEntityRef>,
    pub imports: MiniSet<FlatEntityRef>,
    pub exports: MiniSet<FlatEntityRef>,
    pub split_points: Vec<SplitPoint>,

    pub dependencies: BTreeMap<SplitModuleIdentifier, MiniSet<FlatEntityRef>>,
}

impl Debug for OutputModuleInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "OutputModuleInfo {{")?;
        Self::fmt_table(f, "defined_symbols", self.defined_symbols.iter())?;
        Self::fmt_table(f, "imports", self.imports.iter())?;
        Self::fmt_table(f, "exports", self.exports.iter())?;
        Self::fmt_map(f, "dependencies", self.dependencies.iter())?;
        writeln!(f, "  split_points: {:?},", self.split_points)?;
        write!(f, "}}")
    }
}

impl OutputModuleInfo {
    pub fn need_export(&self, symbol: &FlatEntityRef, input_func_id: FunctionRef) -> bool {
        // if any module linked to current function
        let static_export = self.exports.contains(symbol);
        // Or it is linked indirectly via split points
        let lazy_export = self
            .split_points
            .iter()
            .any(|split_point| split_point.export_func() == input_func_id);
        static_export || lazy_export
    }

    fn fmt_table<T, I>(f: &mut dyn std::fmt::Write, label: &str, items: I) -> std::fmt::Result
    where
        T: Debug,
        I: IntoIterator<Item = T>,
    {
        writeln!(f, "  {label}:")?;
        let rendered: Vec<String> = items.into_iter().map(|item| format!("{item:?}")).collect();
        if rendered.is_empty() {
            writeln!(f, "    <empty>")?;
            return Ok(());
        }

        let width = rendered.iter().map(|item| item.len()).max().unwrap_or(0);
        for chunk in rendered.chunks(10) {
            write!(f, "    ")?;
            for (idx, item) in chunk.iter().enumerate() {
                if idx > 0 {
                    write!(f, " ")?;
                }
                write!(f, "{item:<width$}")?;
            }
            writeln!(f)?;
        }
        Ok(())
    }
    fn fmt_map<ID, T, U, I>(
        f: &mut std::fmt::Formatter<'_>,
        label: &str,
        items: I,
    ) -> std::fmt::Result
    where
        U: Debug,
        ID: Debug,
        I: IntoIterator<Item = (ID, T)>,
        // fmt each item using fmt_table
        T: IntoIterator<Item = U>,
    {
        writeln!(f, "{label}:")?;
        let mut is_empty = true;
        for (id, item) in items {
            Self::fmt_table(f, &format!("{id:?}"), item)?;
            is_empty = false;
        }
        if is_empty {
            writeln!(f, "<empty>")?;
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone)]
pub enum ModuleIdentifier {
    Main,
    Split(String),
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone)]
pub struct SharedModuleIdentifier(pub Vec<ModuleIdentifier>);

impl Display for SharedModuleIdentifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut components = self.0.iter();
        let Some(first) = components.next() else {
            return Ok(());
        };

        write!(f, "{}", first)?;
        components.try_for_each(|n| write!(f, "_{}", n))?;
        Ok(())
    }
}

impl SharedModuleIdentifier {
    pub fn contains(&self, module: &ModuleIdentifier) -> bool {
        self.0.iter().any(|m| m == module)
    }

    pub fn remove(&mut self, module: &ModuleIdentifier) -> bool {
        let original_len = self.0.len();
        self.0.retain(|m| m != module);
        original_len != self.0.len()
    }

    pub fn includes(&self, other: &SplitModuleIdentifier) -> bool {
        match other {
            SplitModuleIdentifier::Single(name) => self.contains(name),
            SplitModuleIdentifier::Shared(shared) => {
                shared.0.iter().all(|name| self.contains(name))
            }
        }
    }
}

impl PartialEq<SplitModuleIdentifier> for SharedModuleIdentifier {
    fn eq(&self, other: &SplitModuleIdentifier) -> bool {
        match other {
            SplitModuleIdentifier::Single(_) => false,
            SplitModuleIdentifier::Shared(shared) => shared == self,
        }
    }
}

impl<'a> IntoIterator for &'a SharedModuleIdentifier {
    type Item = &'a ModuleIdentifier;
    type IntoIter = std::slice::Iter<'a, ModuleIdentifier>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone)]
pub enum SplitModuleIdentifier {
    Single(ModuleIdentifier),
    Shared(SharedModuleIdentifier),
}

impl Default for SplitModuleIdentifier {
    fn default() -> Self {
        Self::Single(ModuleIdentifier::Main)
    }
}

impl Display for ModuleIdentifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Main => write!(f, "main"),
            Self::Split(name) => write!(f, "{}", name),
        }
    }
}

impl Display for SplitModuleIdentifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Single(name) => Display::fmt(name, f),
            Self::Shared(name) => Display::fmt(name, f),
        }
    }
}

impl SplitModuleIdentifier {
    pub fn as_single(&self) -> Option<&ModuleIdentifier> {
        match self {
            Self::Single(name) => Some(name),
            Self::Shared(_) => None,
        }
    }

    pub fn as_shared(&self) -> Option<&SharedModuleIdentifier> {
        match self {
            Self::Single(_) => None,
            Self::Shared(name) => Some(name),
        }
    }

    pub fn is_shared(&self) -> bool {
        matches!(self, Self::Shared(_))
    }

    pub fn is_main(&self) -> bool {
        matches!(self, Self::Single(ModuleIdentifier::Main))
    }

    pub fn is_part_of(&self, other: &SharedModuleIdentifier) -> bool {
        match self {
            Self::Single(name) => other.contains(name),
            Self::Shared(shared) => shared.0.iter().all(|name| other.contains(name)),
        }
    }

    pub fn collect_deps(
        &self,
        shared_modules: &[SharedModuleIdentifier],
    ) -> Vec<SharedModuleIdentifier> {
        let mut result = Vec::new();
        for shared_module in shared_modules {
            if matches!(&self, SplitModuleIdentifier::Shared(our_module) if shared_module == our_module)
            {
                continue;
            }
            if self.is_part_of(shared_module) {
                result.push(shared_module.clone());
            }
        }
        result
    }
}

#[derive(Debug, Default)]
pub struct SplitProgramInfo {
    pub output_modules: Vec<(SplitModuleIdentifier, OutputModuleInfo)>,
    pub symbol_output_module: SecondaryMap<FlatEntityRef, usize>,
}

/// Compute the split modules content based on split points and dependency graph.
#[tracing::instrument(skip_all)]
pub fn compute_split_modules(
    info: &Module,
    dep_graph: &DepGraph,
    split_points: &[SplitPoint],
    wbg_descriptors: &MiniSet<FlatEntityRef>,
    merge_shared: bool,
) -> anyhow::Result<SplitProgramInfo> {
    let split_points_by_module = merge_split_points_by_name(split_points);

    let snapshot = EntitiesSnapshot::new_without_types(info);
    let roots = main_roots(info, &snapshot, split_points, wbg_descriptors);

    let main_deps = find_reachable_deps(dep_graph, &roots);

    let mut named_modules = vec![NamedGraph::new(ModuleIdentifier::Main, main_deps.clone())];

    for (module_name, entry_points) in split_points_by_module.iter() {
        let mut roots = DepSet::new();
        for entry_point in entry_points.iter() {
            roots.insert(snapshot.pack_ref(entry_point.export_func()));
        }
        let split_functions = find_reachable_deps(dep_graph, &roots);
        named_modules.push(NamedGraph::new(
            ModuleIdentifier::Split(module_name.clone()),
            split_functions,
        ));
    }

    let mut shared_deps = NamedGraph::calculate_shared_modules(&mut named_modules, dep_graph);

    let mut split_module_contents = Vec::<(SplitModuleIdentifier, OutputModuleInfo)>::new();

    split_module_contents.extend(named_modules.into_iter().map(|named_graph| {
        let imports = named_graph.imports().clone();
        let split_points = split_points_by_module
            .get(&named_graph.module.to_string())
            .cloned()
            .unwrap_or_default();
        let id = SplitModuleIdentifier::Single(named_graph.module);
        (
            id,
            OutputModuleInfo {
                defined_symbols: named_graph.reachable,
                dependencies: BTreeMap::new(),
                imports,
                split_points,
                exports: DepMiniSet::new(),
            },
        )
    }));

    {
        let (main, rest) = split_module_contents.split_at_mut(1);
        if merge_shared {
            merge_shared_with_main(&mut main[0], rest, &mut shared_deps)?;
        }

        process_special_entities(info, &snapshot, &mut main[0], rest, &mut shared_deps)?;
    }

    // add deps for single modules.
    for (single_id, single) in split_module_contents.iter_mut() {
        let dependencies = calculate_deps(&shared_deps, single_id, &single.imports, None);
        single.dependencies = dependencies;
    }

    for (shared_index, shared) in shared_deps.iter().enumerate() {
        let id = SplitModuleIdentifier::Shared(SharedModuleIdentifier(shared.module_names.clone()));
        let dependencies = calculate_deps(&shared_deps, &id, &shared.imports, Some(shared_index));
        split_module_contents.push((
            id,
            OutputModuleInfo {
                defined_symbols: shared.shared_deps.clone(),
                exports: shared.exports.clone(),
                imports: shared.imports.clone(),
                split_points: vec![],
                dependencies,
            },
        ));
    }

    let symbol_output_module = split_module_contents
        .iter()
        .enumerate()
        .flat_map(|(output_index, (_id, info))| {
            info.defined_symbols
                .iter()
                .map(move |symbol| (*symbol, output_index))
        })
        .collect::<SecondaryMap<FlatEntityRef, usize>>();

    let output_modules = split_module_contents.into_iter().collect::<Vec<_>>();

    Ok(SplitProgramInfo {
        output_modules,
        symbol_output_module,
    })
}

/// Module deps calculation:
fn get_deps(
    shared_deps: &[SharedEntry<ModuleIdentifier>],
    id: &SplitModuleIdentifier,
) -> Vec<usize> {
    match id {
        SplitModuleIdentifier::Single(name) => shared_deps
            .iter()
            .enumerate()
            .filter(|(_i, m)| m.module_names.contains(name))
            .map(|(i, _)| i)
            .collect(),
        SplitModuleIdentifier::Shared(shared) => shared_deps
            .iter()
            .enumerate()
            .filter(|(_i, m)| shared.0.iter().all(|name| m.module_names.contains(name)))
            .map(|(i, _)| i)
            .collect::<Vec<_>>(),
    }
}
fn calculate_deps(
    shared_deps: &[SharedEntry<ModuleIdentifier>],
    id: &SplitModuleIdentifier,
    imports: &MiniSet<FlatEntityRef>,
    shared_index: Option<usize>,
) -> BTreeMap<SplitModuleIdentifier, MiniSet<FlatEntityRef>> {
    let dep_modules = get_deps(shared_deps, id);
    dep_modules
        .into_iter()
        .filter(|dep_module_index| shared_index != Some(*dep_module_index))
        .map(|dep_module_index| {
            let dep_module = &shared_deps[dep_module_index];
            let dep_id = SplitModuleIdentifier::Shared(SharedModuleIdentifier(
                dep_module.module_names.clone(),
            ));
            let dep_symbols = dep_module
                .exports
                .iter()
                .filter(|symbol| imports.contains(symbol))
                .cloned()
                .collect();
            (dep_id, dep_symbols)
        })
        .collect()
}

// The indirect_function table is shared between main module and submodules.
// it's layout is:
// [ 0: empty ]
// [ 1..N: functions used in this module ]
// [ N+1..N+M: reserved space for lazy stubs, main module fill it empty, and submodules fill it with stubs ]
// [ N+M+1.. : dynamic allocated entries - used for tables in submodules ]
//
// Example of final layout:
// 1. After main load:
// [0, f1, f2, f3, ..., s1_entry1_uninit, s1_entry2_uninit, s2_entry1_uninit, ...]
// 2. After submodule load:
// [0, f1, f2, f3, ..., s1_entry1,        s1_entry2,       s1_f1, s1_f2, ...]
// 3. If submodule reloaded, the following changes are applied:
// [_, _, _, _, ...,    s1_FIX_entry1,    s1_FIX_entry2,   s1_f1, s1_f2,     s1_FIX_f1, s1_FIX_f2, ...]
// Note that original s1_f1 and s1_f2 are not removed, because other submodules may use them.
// And only after calling linker::unload we can reuse these entries.
#[derive(Debug, Eq, PartialEq, Clone)]
pub struct IndirectFnLayout {
    pub start_dyn: usize,
    pub dyn_fns: Vec<FlatEntityRef>,
    snapshot: EntitiesSnapshot,
}

impl IndirectFnLayout {
    pub fn new(
        main_indirect_table_len: usize,
        split_points: &[SplitPoint],
        snapshot: EntitiesSnapshot,
    ) -> Self {
        let start_dyn = main_indirect_table_len;
        let dyn_fns = split_points
            .iter()
            .map(|sp| sp.export_func())
            .map(|func| snapshot.pack_ref(func))
            .collect();
        Self {
            start_dyn,
            dyn_fns,
            snapshot,
        }
    }
    /// Return place reserved for given split point in the flat indirect functions list.
    pub fn get_split_point_index(&self, split_point: &SplitPoint) -> usize {
        let id = self.snapshot.pack_ref(split_point.export_func());
        self.start_dyn
            + self
                .dyn_fns
                .iter()
                .position(|f| *f == id)
                .expect("Split point export function not found in indirect functions layout")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        analysis::parse::{SplitPointExtractor, find_split_points},
        typed::LoadedFile,
    };

    #[test]
    fn test_is_part_of() {
        let a = SplitModuleIdentifier::Single(ModuleIdentifier::Split("a".into()));
        let b = SplitModuleIdentifier::Single(ModuleIdentifier::Split("b".into()));
        let c = SplitModuleIdentifier::Single(ModuleIdentifier::Split("c".into()));

        let shared = SharedModuleIdentifier(vec![
            ModuleIdentifier::Split("a".into()),
            ModuleIdentifier::Split("b".into()),
            ModuleIdentifier::Split("c".into()),
        ]);

        assert!(a.is_part_of(&shared));
        assert!(b.is_part_of(&shared));
        assert!(c.is_part_of(&shared));

        let ab = SplitModuleIdentifier::Shared(SharedModuleIdentifier(vec![
            ModuleIdentifier::Split("a".into()),
            ModuleIdentifier::Split("b".into()),
        ]));
        assert!(ab.is_part_of(&shared));
    }

    #[test]
    fn test_snapshot_split_structure() {
        let _ = env_logger::Builder::new()
            .filter(None, log::LevelFilter::Debug)
            .parse_env("RUST_LOG")
            .try_init();

        // Test with simple_graph.wasm
        let wasm_bytes = crate::testfiles::SIMPLE_GRAPH;
        test_snapshot_split_structure_for_file("simple_graph.wasm", wasm_bytes);

        // Test with example.wasm
        let wasm_bytes = crate::testfiles::EXAMPLE_WASM;
        test_snapshot_split_structure_for_file("example.wasm", wasm_bytes);

        // Too big for snapshot testing.
        // // Test with lazy_routes.wasm
        // let wasm_bytes = crate::testfiles::LAZY_ROUTES;
        // test_snapshot_split_structure_for_file("lazy_routes.wasm", wasm_bytes);
    }

    fn test_snapshot_split_structure_for_file(name: &str, wasm_bytes: &[u8]) {
        let info = LoadedFile::from_wasm_bytes(wasm_bytes).expect("Failed to parse wasm file");

        // todo: snapshot entities.

        let dep_graph = crate::analysis::dep_graph::get_dependencies(&info)
            .expect("Failed to get dependencies");
        let split_points = find_split_points(&info.module, SplitPointExtractor::Legacy)
            .expect("Failed to find split points");

        let wbg_descriptors = wbg_closures(&info.module, &dep_graph);

        // Snapshot the dependency graph
        let dep_graph_output = crate::analysis::debug::format_dep_graph(&dep_graph, &info.module);
        insta::assert_snapshot!(format!("{} - dep_graph", name), dep_graph_output);

        // Compute and snapshot the split program info
        let split_info = compute_split_modules(
            &info.module,
            &dep_graph,
            &split_points,
            &wbg_descriptors,
            false,
        )
        .expect("Failed to compute split modules");

        let split_info_output =
            crate::analysis::debug::format_split_program_info(&split_info, &info.module);
        insta::assert_snapshot!(format!("{} - split_program_info", name), split_info_output);

        // Compute and snapshot the split program info
        let split_info = compute_split_modules(
            &info.module,
            &dep_graph,
            &split_points,
            &wbg_descriptors,
            true,
        )
        .expect("Failed to compute split modules");

        let split_info_output =
            crate::analysis::debug::format_split_program_info(&split_info, &info.module);
        insta::assert_snapshot!(
            format!("{} - split_program_info - merged", name),
            split_info_output
        );
    }
}
