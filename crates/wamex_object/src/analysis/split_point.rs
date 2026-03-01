use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::{Debug, Display},
};

use anyhow::Context;
use cranelift_entity::SecondaryMap;
use wamex_types::map_vec::MiniSet;

use super::dep_graph::{DepGraph, DepMiniSet, DepSet, NamedGraph, find_reachable_deps};
use crate::{
    analysis::dep_graph::SharedEntry,
    typed::{
        FunctionRef, Module,
        common_index::{EntitiesSnapshot, EntityKind, FlatEntityRef},
    },
};

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum SplitPointExtractor {
    /// Use regexp and `_wasm_split_` prefix to identify split points.
    Legacy,
    /// Use `__wamex_` prefix and `.start_with` instead of regexp.
    Wamex,
}

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

    fn fmt_table<'a, T, I>(f: &mut dyn std::fmt::Write, label: &str, items: I) -> std::fmt::Result
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
    fn fmt_map<'a, ID, T, U, I>(
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
            Self::fmt_table(f, &format!("{id:?}"), item.into_iter())?;
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

pub(crate) fn parser<'a>(name: &'a str, prefix: &str, postfix: &str) -> Option<(&'a str, &'a str)> {
    if !name.starts_with(prefix) {
        return None;
    }
    let name = &name[prefix.len()..];
    let postfix_index = name.find(postfix)?;
    let module_name = &name[..postfix_index];
    let fn_name = &name[postfix_index + postfix.len()..];

    Some((module_name, fn_name))
}

fn parse_entries<'i, I: 'i, Id>(
    prefix: &str,
    postfix: &str,
    collection: I,
) -> BTreeMap<(String, String), Id>
where
    I: Iterator<Item = (Id, &'i str)>,
{
    collection
        .filter_map(|(id, name)| {
            if let Some((module_name, unique_id)) = parser(name, prefix, postfix) {
                Some(((module_name.into(), unique_id.into()), id))
            } else {
                None
            }
        })
        .collect()
}

pub(crate) const SPLIT_IMPORT_POSTFIX: &str = "00_import_";
pub(crate) const SPLIT_EXPORT_POSTFIX: &str = "00_export_";

fn find_split_points_with_prefix(info: &Module, prefix: &str) -> anyhow::Result<Vec<SplitPoint>> {
    let import_map = parse_entries(
        prefix,
        SPLIT_IMPORT_POSTFIX,
        info.functions
            .imports_iter()
            .map(|(i, import)| (i, &*import.name)),
    );
    let mut export_map = parse_entries(
        prefix,
        SPLIT_EXPORT_POSTFIX,
        info.functions
            .exports
            .iter()
            .map(|e| (e.entity_index, &*e.name)),
    );

    let split_points = import_map
        .into_iter()
        .map(|(key, import_func)| -> anyhow::Result<SplitPoint> {
            let export_func = export_map
                .remove(&key)
                .with_context(|| format!("No corresponding export for split import {key:?}"))?;
            Ok(SplitPoint::new(key.0, key.1, import_func, export_func))
        })
        .collect::<anyhow::Result<Vec<SplitPoint>>>()?;

    if let Some((key, _)) = export_map.iter().next() {
        log::error!(
            "No corresponding import for split export {key:?} hash {key_hash:?}. Maybe split module is defined but not used.",
            key_hash = key.1,
            key = key.0
        );
    }

    Ok(split_points)
}

pub fn find_split_points_legacy(info: &Module) -> anyhow::Result<Vec<SplitPoint>> {
    find_split_points_with_prefix(info, "__wasm_split_00")
}

pub(crate) const WAMEX_ENTRY_PREFIX: &str = "__wamex_00";

fn find_split_points_wamex(info: &Module) -> anyhow::Result<Vec<SplitPoint>> {
    find_split_points_with_prefix(info, WAMEX_ENTRY_PREFIX)
}

pub fn find_split_points(
    info: &Module,
    split_point_type: SplitPointExtractor,
) -> anyhow::Result<Vec<SplitPoint>> {
    match split_point_type {
        SplitPointExtractor::Legacy => find_split_points_legacy(info),
        SplitPointExtractor::Wamex => find_split_points_wamex(info),
    }
}

fn is_wasm_bindgen_cast(name: &str) -> bool {
    name == "__wbindgen_describe_closure"
        || name == "__wbindgen_describe_cast"
        || name == "__wbindgen_describe"
}

pub fn wbg_closures(module: &Module, graph: &DepGraph) -> MiniSet<FlatEntityRef> {
    let mut wbg_closures = std::collections::BTreeSet::new();

    let snapshot = EntitiesSnapshot::new(module);

    let mut wbg_all = std::collections::BTreeSet::new();
    for (id, import) in module.functions.imports_iter() {
        // find all wbg fns
        if import.name != "__wbindgen_placeholder__" {
            continue;
        }
        let id = snapshot.pack_ref(id);
        wbg_all.insert(id);
        if is_wasm_bindgen_cast(&import.name) {
            wbg_closures.insert(id);
        }
    }

    let mut wbg_descriptors = wbg_all;
    for id in wbg_closures.iter().cloned() {
        if let Some(parents) = graph.get_parents(id) {
            for parent in parents {
                debug_assert!(snapshot.unpack_ref(*parent).is_function());
                wbg_descriptors.insert(*parent);
            }
        }
    }

    wbg_descriptors.into_iter().collect()
}

pub fn merge_split_points_by_name(
    split_points: &[SplitPoint],
) -> BTreeMap<String, Vec<SplitPoint>> {
    let mut result = BTreeMap::<String, Vec<SplitPoint>>::new();
    for split_point in split_points {
        result
            .entry(split_point.module_name.clone())
            .or_default()
            .push(split_point.clone());
    }
    for results in result.values_mut() {
        results.sort_by_key(|sp| sp.unique_id.clone());
    }
    result
}

pub fn main_roots(
    info: &Module,
    snapshot: &EntitiesSnapshot,
    split_points: &[SplitPoint],
    wbg_descriptors: &MiniSet<FlatEntityRef>,
) -> DepSet {
    let mut roots: DepSet = DepSet::new();

    info.start_functions.iter().for_each(|start_fn| {
        roots.insert(snapshot.pack_ref(*start_fn));
    });
    for export in info.functions.exports.iter() {
        let index = export.entity_index;
        roots.insert(snapshot.pack_ref(index));
    }

    // Include all imports - to make sure that nothing will be removed
    for (index, _) in info.functions.imports_iter() {
        roots.insert(snapshot.pack_ref(index));
    }

    // After adding imports/exports - remove those that are corresponding to split points
    for split_point in split_points.iter() {
        roots.remove(&snapshot.pack_ref(split_point.export_func()));

        // remove import fn as well - because it might be used by other module, and not used by main,
        // let find_reachable_deps do the job.
        roots.remove(&snapshot.pack_ref(split_point.import_func()));
    }

    // Add wasm-bindgen descriptors - to make sure that they will be emited into main module.
    for descriptor in wbg_descriptors {
        roots.insert(*descriptor);
    }

    roots
}

// // Merge modules that shared with main module into main itself.
// pub fn merge_main_shared(program_info: &mut SplitProgramInfo) {
//     let (shared_with_main, mut other): (Vec<_>, Vec<_>) =
//         std::mem::take(&mut program_info.output_modules)
//             .into_iter()
//             .partition(|(id, _)| {
//                 if let SplitModuleIdentifier::Shared(shared_with) = id {
//                     shared_with.contains(&ModuleIdentifier::Main)
//                 } else {
//                     false
//                 }
//             });

//     // split iter at 3 parts: before main, main, after main
//     let (left_to_main, main_module, right_to_main) = {
//         let main_module_index = other
//             .iter()
//             .enumerate()
//             .find(|(_, (id, _))| *id == MAIN_ID)
//             .expect("Main module not found")
//             .0;
//         let (before, main_and_next) = other.split_at_mut(main_module_index);
//         let (main_module, after) = main_and_next.split_at_mut(1);
//         let main_module = &mut main_module[0].1;
//         (before, main_module, after)
//     };

//     // check import in all remain modules except main
//     let is_imported_by_other = |node: &SymbolId| {
//         left_to_main
//             .iter()
//             .chain(right_to_main.iter())
//             .any(|(_, mod_state)| mod_state.imports.contains(node))
//             || right_to_main
//                 .iter()
//                 .any(|(_, mod_state)| mod_state.imports.contains(node))
//     };

//     #[cfg(debug_assertions)]
//     let mut check_imports = vec![];

//     for (id, mut shared_module) in shared_with_main {
//         debug_assert!(shared_module.split_points.is_empty());

//         for node in &shared_module.exports {
//             // it was exported in shared module, so on main side it had been imported.
//             // remove from main link symbols.
//             if !main_module.imports.remove(node) {
//                 log::trace!(
//                     "Shared module symbol not found in main: {node:?}. It probably was removed in other shared entry."
//                 );
//             }
//             // This was imported not only by main, so export is needed.
//             if is_imported_by_other(node) {
//                 main_module.exports.insert(*node);
//             }
//         }

//         // imported modules should already be in main
//         #[cfg(debug_assertions)]
//         for node in &shared_module.imports {
//             check_imports.push(*node);
//         }
//         log::trace!(
//             "extending main defined symbols with shared ({id:?}): {:?}",
//             shared_module.defined_symbols
//         );

//         main_module
//             .defined_symbols
//             .extend(std::mem::take(&mut shared_module.defined_symbols));
//     }

//     debug_assert!(main_module.imports.is_empty());
//     #[cfg(debug_assertions)]
//     for node in check_imports {
//         assert!(
//             main_module.defined_symbols.contains(&node),
//             "Shared module import not found in main defined symbols: {node:?}"
//         );
//     }

//     program_info.output_modules = std::mem::take(&mut other);
// }

// Merge shared modules with main module.
pub fn merge_shared_with_main(
    (main_id, main): &mut (SplitModuleIdentifier, OutputModuleInfo),
    regular_modules: &[(SplitModuleIdentifier, OutputModuleInfo)],
    shared: &mut Vec<SharedEntry<ModuleIdentifier>>,
) -> anyhow::Result<()> {
    assert_eq!(
        &*main_id,
        &SplitModuleIdentifier::Single(ModuleIdentifier::Main)
    );

    let mut shared_with_main = Vec::new();
    let mut other_shared = Vec::new();

    for shared_module in shared.drain(..) {
        if shared_module.module_names.contains(&ModuleIdentifier::Main) {
            shared_with_main.push(shared_module);
        } else {
            other_shared.push(shared_module);
        }
    }

    // check import in all remain modules except main
    let is_imported_by_other = |node: &FlatEntityRef| {
        regular_modules
            .iter()
            .any(|(_, mod_state)| mod_state.imports.contains(node))
    };
    #[cfg(debug_assertions)]
    let mut check_imports = vec![];

    for shared_module in shared_with_main {
        for node in &shared_module.exports {
            // it was exported in shared module, so on main side it had been imported.
            // remove from main link symbols.
            if !main.imports.remove(node) {
                log::trace!(
                    "Shared module symbol not found in main: {node:?}. It probably was removed in other shared entry."
                );
            }
            // This was imported not only by main, so export is needed.
            if is_imported_by_other(node) {
                main.exports.insert(*node);
            }
        }

        // imported modules should already be in main
        #[cfg(debug_assertions)]
        for node in &shared_module.imports {
            check_imports.push(*node);
        }

        log::trace!(
            "extending main defined symbols with shared ({}): {:?}",
            shared_module
                .module_names
                .iter()
                .map(|m| m.to_string())
                .collect::<Vec<_>>()
                .join(","),
            shared_module.shared_deps
        );

        main.defined_symbols.extend(shared_module.shared_deps);
    }

    assert!(
        main.imports.is_empty(),
        "BUG: After merging shared modules, main still contain imports."
    );
    #[cfg(debug_assertions)]
    for node in check_imports {
        assert!(
            main.defined_symbols.contains(&node),
            "Shared module import not found in main defined symbols: {node:?}"
        );
    }

    shared.extend(other_shared);

    Ok(())
}
/// Compute the split modules content based on split points and dependency graph.
pub fn compute_split_modules(
    info: &Module,
    dep_graph: &DepGraph,
    split_points: &[SplitPoint],
    wbg_descriptors: &MiniSet<FlatEntityRef>,
    merge_shared: bool,
) -> anyhow::Result<SplitProgramInfo> {
    let split_points_by_module = merge_split_points_by_name(split_points);

    let snapshot = EntitiesSnapshot::new(info);
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
        let dependencies = calculate_deps(&shared_deps, &id, &imports, None);
        (
            id,
            OutputModuleInfo {
                defined_symbols: named_graph.reachable,
                dependencies,
                imports,
                split_points,
                exports: DepMiniSet::new(),
            },
        )
    }));

    if merge_shared {
        let (main, rest) = split_module_contents.split_at_mut(1);
        merge_shared_with_main(&mut main[0], rest, &mut shared_deps)?;
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
        .filter(|dep_module_index| shared_index.map_or(true, |si| si != *dep_module_index))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typed::LoadedFile;

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
