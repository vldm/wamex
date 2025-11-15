use std::{
    collections::BTreeMap,
    fmt::{Debug, Display},
};

use anyhow::{anyhow, bail};

use super::dep_graph::DepGraph;
use crate::{
    SplitPointExtractor,
    analysis::{
        self,
        dep_graph::{DepMiniSet, DepSet, NamedGraph, find_reachable_deps},
    },
    index::{ExportId, IdMap, ImportId, InputFuncId, SymbolId},
};

// TODO: impl merge and use it in emit_modules as one of strategies to emit modules.
// The other possible is to emit it as separate chunk and allow linkage.
#[derive(Default, Clone)]
pub struct OutputModuleInfo {
    pub defined_symbols: DepSet,
    // Shared imports that should be imported from other modules.
    pub imports: DepMiniSet,
    pub exports: DepMiniSet,
    // TODO: Instead of split points we need list of what "split-points" we exports, and what we imports
    pub split_points: Vec<SplitPoint>,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct SplitPoint {
    // Name of split function that will be moved to the submodule.
    pub module_name: String,
    // Index in imports[] of the module import function.
    pub import: ImportId,
    // Index in functions[] of the corespoinding import
    pub import_func: InputFuncId,
    // Index in exports[] of the module export function.
    pub export: ExportId,
    // Index in functions[] of corespoiding export
    pub export_func: InputFuncId,
}

fn parser<'a>(name: &'a str, prefix: &str, postfix: &str) -> Option<(&'a str, &'a str)> {
    if !name.starts_with(prefix) {
        return None;
    }
    let name = &name[prefix.len()..];
    let postfix_index = name.find(postfix)?;
    let module_name = &name[..postfix_index];
    let fn_name = &name[postfix_index + postfix.len()..];

    Some((module_name, fn_name))
}

fn find_split_points_with_prefix(
    info: &analysis::ModuleInfo,
    prefix: &str,
) -> anyhow::Result<Vec<SplitPoint>> {
    macro_rules! process_imports_or_exports {
        ($postfix: expr, $map:ident, $member:ident, $id_ty:ty) => {
            let $map = info
                .wasm
                .$member
                .iter()
                .filter_map(|(id, item)| {
                    if let Some((module_name, unique_id)) = parser(&item.name, prefix, $postfix) {
                        Some(((module_name.into(), unique_id.into()), id))
                    } else {
                        None
                    }
                })
                .collect::<BTreeMap<(String, String), $id_ty>>();
        };
    }

    process_imports_or_exports!("00_import_", import_map, imports, ImportId);
    process_imports_or_exports!("00_export_", export_map, exports, ExportId);
    let mut export_map = export_map;

    let split_points = import_map
        .into_iter()
        .map(|(key, import_id)| -> anyhow::Result<SplitPoint> {
            let export_id = export_map.remove(&key).ok_or_else(|| {
                anyhow::anyhow!("No corresponding export for split import {key:?}")
            })?;
            let export = info.wasm.exports[export_id];
            let wasmparser::Export {
                kind: wasmparser::ExternalKind::Func,
                index,
                ..
            } = export
            else {
                bail!("Expected exported function but received: {export:?}");
            };
            let &import_func = info
                .import_info
                .imported_func_map
                .get(import_id)
                .ok_or_else(|| {
                    anyhow!(
                        "Expected imported function but received: {:?}",
                        &info.wasm.imports[import_id]
                    )
                })?;
            Ok(SplitPoint {
                module_name: key.0,
                import: import_id,
                import_func,
                export: export_id,
                export_func: InputFuncId::from_index(index),
            })
        })
        .collect::<anyhow::Result<Vec<SplitPoint>>>()?;

    if let Some((key, _)) = export_map.iter().next() {
        anyhow::bail!(
            "No corresponding import for split export {key:?} hash {key_hash:?}. Maybe split module is defined but not used.",
            key_hash = key.1,
            key = key.0
        );
    }

    Ok(split_points)
}

/// Search for _wasm_split_00<module_name>00_import_<import_id> and
/// _wasm_split_00<module_name>00_export_<export_id> functions
/// and extract them as SplitPoints.
pub fn find_split_points_legacy(info: &analysis::ModuleInfo) -> anyhow::Result<Vec<SplitPoint>> {
    find_split_points_with_prefix(info, "__wasm_split_00")
}
/// Search for __wamex_00<module_name>00_import_<import_id> and
/// __wamex_00<module_name>00_export_<export_id> functions
/// and extract them as SplitPoints.
fn find_split_points_wamex(info: &analysis::ModuleInfo) -> anyhow::Result<Vec<SplitPoint>> {
    find_split_points_with_prefix(info, "__wamex_00")
}

pub fn find_split_points(
    info: &analysis::ModuleInfo,
    split_point_type: SplitPointExtractor,
) -> anyhow::Result<Vec<SplitPoint>> {
    match split_point_type {
        SplitPointExtractor::Legacy => find_split_points_legacy(info),
        SplitPointExtractor::Wamex => find_split_points_wamex(info),
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
    // Return true if a module was removed.
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

    /// Check if this split module identifier includes the given module identifier.
    pub fn is_part_of(&self, other: &SharedModuleIdentifier) -> bool {
        match self {
            Self::Single(name) => other.contains(name),
            Self::Shared(shared) => shared.0.iter().all(|name| other.contains(name)),
        }
    }

    // List all shared modules which name include this split module.
    // This modules are (directly or indirectly) called by the current module.
    // NOTE: This method can return modules that are not directly connected to this module.
    //
    // Example, consider next tree deps:
    // Single(A) -> Shared(A,B)
    // Single(B) -> Shared(A,B)
    // Single(C) -> Shared(B,C)
    // Shared(A,B) -> Shared(A,B,C);
    //
    // This method will return:
    // For Single(A) -> [Shared(A,B), Shared(A,B,C)]
    // For Shared(A,B) -> [Shared(A,B,C)]
    // ...
    pub fn collect_deps(
        &self,
        shared_modules: &[SharedModuleIdentifier],
    ) -> Vec<SharedModuleIdentifier> {
        let mut result = Vec::new();
        for shared_module in shared_modules {
            if matches!(&self, SplitModuleIdentifier::Shared(our_module) if shared_module == our_module)
            {
                continue; // skip self
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
    pub symbol_output_module: IdMap<SymbolId, usize>,
}

impl SplitProgramInfo {
    // TODO: Not sure why imports are used here.
    // Add start_func, exports and imports
    // Filter-out all split points related functions.
    fn get_main_module_roots(info: &analysis::ModuleInfo, split_points: &[SplitPoint]) -> DepSet {
        let mut roots: DepSet = DepSet::new();
        if let Some(id) = info.wasm.code.section_payload.start_func {
            roots.insert(info.symbols.get_function_symbol(id).unwrap());
        }
        for (_id, export) in info.wasm.exports.iter() {
            let wasmparser::Export {
                index,
                kind: wasmparser::ExternalKind::Func,
                ..
            } = export
            else {
                continue;
            };
            roots.insert(
                info.symbols
                    .get_function_symbol(InputFuncId::from_index(*index))
                    .unwrap(),
            );
        }

        for split_point in split_points.iter() {
            roots.remove(
                &info
                    .symbols
                    .get_function_symbol(split_point.export_func)
                    .unwrap(),
            );
            roots.remove(
                &info
                    .symbols
                    .get_function_symbol(split_point.import_func)
                    .unwrap(),
            );
        }
        roots
    }

    pub fn merge_split_points_by_name(
        split_points: &[SplitPoint],
    ) -> BTreeMap<String, Vec<&SplitPoint>> {
        let mut result = BTreeMap::<String, Vec<&SplitPoint>>::new();

        for split_point in split_points {
            result
                .entry(split_point.module_name.clone())
                .or_default()
                .push(split_point);
        }
        result
    }

    pub fn compute_split_modules(
        info: &analysis::ModuleInfo,
        dep_graph: &DepGraph,
        split_points: &[SplitPoint],
    ) -> anyhow::Result<SplitProgramInfo> {
        let split_points_by_module = Self::merge_split_points_by_name(split_points);

        let main_roots = Self::get_main_module_roots(info, split_points);

        // graph root -> dep -> dep
        let main_deps = find_reachable_deps(dep_graph, &main_roots);

        let mut named_modules = vec![NamedGraph::new(ModuleIdentifier::Main, main_deps.clone())];

        // Determine reachable symbols (excluding main module symbols) for each
        // split module. Symbols may be reachable from more than one split module;
        // these symbols will be moved to a separate module.
        for (module_name, entry_points) in split_points_by_module.iter() {
            let mut roots = DepSet::new();
            for entry_point in entry_points.iter() {
                roots.insert(
                    info.symbols
                        .get_function_symbol(entry_point.export_func)
                        .unwrap(),
                );
            }

            let split_functions = find_reachable_deps(dep_graph, &roots);

            named_modules.push(NamedGraph::new(
                ModuleIdentifier::Split(module_name.clone()),
                split_functions,
            ));
        }

        // Calculate shared deps.
        let shared_deps = NamedGraph::calculate_shared_modules(&mut named_modules, dep_graph);

        let mut split_module_contents = BTreeMap::<SplitModuleIdentifier, OutputModuleInfo>::new();

        split_module_contents.extend(named_modules.into_iter().map(|named_graph| {
            let imports = named_graph.imports().clone();
            // TODO: Rewrite this
            let split_points = split_points_by_module
                .get(&named_graph.module.to_string())
                .iter()
                .copied()
                .flatten()
                .copied()
                .cloned()
                .collect();
            (
                SplitModuleIdentifier::Single(named_graph.module),
                OutputModuleInfo {
                    defined_symbols: named_graph.reachable,
                    imports,
                    split_points,
                    // Module can only import symbols from shared modules.
                    exports: DepMiniSet::new(),
                },
            )
        }));

        for shared in shared_deps {
            split_module_contents.insert(
                SplitModuleIdentifier::Shared(SharedModuleIdentifier(shared.module_names.clone())),
                OutputModuleInfo {
                    defined_symbols: shared.shared_deps,
                    exports: shared.exports,
                    imports: shared.imports,
                    split_points: vec![],
                },
            );
        }

        let symbol_output_module = split_module_contents
            .iter()
            .enumerate()
            .flat_map(|(output_index, (_, info))| {
                info.defined_symbols
                    .iter()
                    .map(move |symbol| (*symbol, output_index))
            })
            .collect();

        Ok(SplitProgramInfo {
            output_modules: split_module_contents.into_iter().collect(),
            symbol_output_module,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_part_of() {
        let shared = SharedModuleIdentifier(vec![
            ModuleIdentifier::Main,
            ModuleIdentifier::Split("a".into()),
            ModuleIdentifier::Split("c".into()),
        ]);
        let single_main = SplitModuleIdentifier::Single(ModuleIdentifier::Main);
        let single_a = SplitModuleIdentifier::Single(ModuleIdentifier::Split("a".into()));
        let single_b = SplitModuleIdentifier::Single(ModuleIdentifier::Split("b".into()));
        let shared_ab = SplitModuleIdentifier::Shared(SharedModuleIdentifier(vec![
            ModuleIdentifier::Split("a".into()),
            ModuleIdentifier::Split("b".into()),
        ]));
        let single_c = SplitModuleIdentifier::Single(ModuleIdentifier::Split("c".into()));
        let shared_ac = SplitModuleIdentifier::Shared(SharedModuleIdentifier(vec![
            ModuleIdentifier::Split("a".into()),
            ModuleIdentifier::Split("c".into()),
        ]));

        assert!(single_main.is_part_of(&shared));
        assert!(single_a.is_part_of(&shared));
        assert!(single_c.is_part_of(&shared));
        assert!(shared_ac.is_part_of(&shared));
        assert!(!single_b.is_part_of(&shared));
        assert!(!shared_ab.is_part_of(&shared));
    }

    //     #[test]
    //     fn from_example() {
    //         "e_data_9382861128153349529"
    //         "lazy_data_2021541099659736428"

    //          "view_c_view_16564031152823166319_view_d_view_3929403835768869397_view_e_view_16839038780052115883"
    // "e_data_9382861128153349529_lazy_data_2021541099659736428"
    //     }
}
