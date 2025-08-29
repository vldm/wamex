use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt::Debug,
};

use anyhow::{anyhow, bail};
use lazy_static::lazy_static;
use regex::Regex;

use super::dep_graph::{DepGraph, DepNode, ReachabilityGraph};
use crate::{
    analysis,
    analysis::dep_graph::NamedGraph,
    index::{ExportId, ImportId, InputFuncId},
    read::InputModule,
};

// TODO: impl merge and use it in emit_modules as one of strategies to emit modules.
// The other possible is to emit it as separate chunk and allow linkage.
#[derive(Default)]
pub struct OutputModuleInfo {
    pub included_symbols: HashSet<DepNode>,
    // Shared imports that should be imported from other modules.
    pub link_symbols: HashSet<DepNode>,
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

/// Search for _wasm_split_00<module_name>00_import_<import_id> and
/// _wasm_split_00<module_name>00_export_<export_id> functions
/// and extract them as SplitPoints.
pub fn find_split_points(
    module: &InputModule,
    info: &analysis::ModuleInfo,
) -> anyhow::Result<Vec<SplitPoint>> {
    macro_rules! process_imports_or_exports {
        ($pattern:expr, $map:ident, $member:ident, $id_ty:ty) => {
            let mut $map = HashMap::<(String, String), $id_ty>::new();
            {
                lazy_static! {
                    static ref PATTERN: Regex = Regex::new($pattern).unwrap();
                }

                for (id, item) in module.$member.iter() {
                    let Some(captures) = PATTERN.captures(&item.name) else {
                        continue;
                    };
                    let (_, [module_name, unique_id]) = captures.extract();
                    $map.insert((module_name.into(), unique_id.into()), id);
                }
            }
        };
    }

    process_imports_or_exports!(
        "__wasm_split_00(.*)00_import_([0-9a-f]{32})",
        import_map,
        imports,
        ImportId
    );
    process_imports_or_exports!(
        "__wasm_split_00(.*)00_export_([0-9a-f]{32})",
        export_map,
        exports,
        ExportId
    );

    let split_points = import_map
        .drain()
        .map(|(key, import_id)| -> anyhow::Result<SplitPoint> {
            let export_id = export_map.remove(&key).ok_or_else(|| {
                anyhow::anyhow!("No corresponding export for split import {key:?}")
            })?;
            let export = module.exports[export_id];
            let wasmparser::Export {
                kind: wasmparser::ExternalKind::Func,
                index,
                ..
            } = export
            else {
                bail!("Expected exported function but received: {export:?}");
            };
            let &import_func = info
                .import_funcs_info
                .imported_func_map
                .get(import_id)
                .ok_or_else(|| {
                    anyhow!(
                        "Expected imported function but received: {:?}",
                        &module.imports[import_id]
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

    for (key, _) in export_map.iter() {
        anyhow::bail!("No corresponding import for split export {key:?}");
    }

    Ok(split_points)
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone)]
pub enum ModuleIdentifier {
    Main,
    Split(String),
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone)]
pub enum SplitModuleIdentifier {
    Single(ModuleIdentifier),
    Shared(Vec<ModuleIdentifier>),
}

impl ModuleIdentifier {
    pub fn name(&self) -> &str {
        match self {
            Self::Main => "main",
            Self::Split(name) => name,
        }
    }
}
impl SplitModuleIdentifier {
    pub fn name(&self) -> String {
        match self {
            Self::Single(name) => name.name().to_string(),
            Self::Shared(names) => names.iter().fold(String::new(), |mut acc, n| {
                if !acc.is_empty() {
                    acc.push('_');
                }
                acc + &n.name()
            }),
        }
    }
    pub fn as_single(&self) -> Option<&ModuleIdentifier> {
        match self {
            Self::Single(name) => Some(name),
            Self::Shared(_) => None,
        }
    }
    pub fn is_shared(&self) -> bool {
        matches!(self, Self::Shared(_))
    }
    pub fn is_main(&self) -> bool {
        matches!(self, Self::Single(ModuleIdentifier::Main))
    }
}

#[derive(Debug, Default)]
pub struct SplitProgramInfo {
    pub output_modules: Vec<(SplitModuleIdentifier, OutputModuleInfo)>,
    pub shared_nodes: HashSet<DepNode>,
    pub symbol_output_module: HashMap<DepNode, usize>,
}

impl SplitProgramInfo {
    // TODO: Not sure why imports are used here.
    // Add start_func, exports and imports
    // Filter-out all split points related functions.
    fn get_main_module_roots(
        info: &analysis::ModuleInfo,
        split_points: &[SplitPoint],
    ) -> HashSet<DepNode> {
        let mut roots: HashSet<DepNode> = HashSet::new();
        if let Some(id) = info.source.code.section_payload.start_func {
            roots.insert(DepNode::Function(id));
        }
        for (_id, export) in info.source.exports.iter() {
            let wasmparser::Export {
                index,
                kind: wasmparser::ExternalKind::Func,
                ..
            } = export
            else {
                continue;
            };
            roots.insert(DepNode::Function(InputFuncId::from_index(*index)));
        }

        for split_point in split_points.iter() {
            roots.remove(&DepNode::Function(split_point.export_func));
            roots.remove(&DepNode::Function(split_point.import_func.into()));
        }
        roots
    }

    pub fn merge_split_points_by_name(
        split_points: &[SplitPoint],
    ) -> HashMap<String, Vec<&SplitPoint>> {
        let mut result = HashMap::<String, Vec<&SplitPoint>>::new();

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
        let split_points_by_module = Self::merge_split_points_by_name(&split_points[..]);

        let main_roots = Self::get_main_module_roots(info, &split_points);

        // graph root -> dep -> dep
        let main_deps = ReachabilityGraph::find_reachable_deps(dep_graph, &main_roots);

        let mut named_modules = vec![NamedGraph::new(ModuleIdentifier::Main, main_deps.clone())];

        // Determine reachable symbols (excluding main module symbols) for each
        // split module. Symbols may be reachable from more than one split module;
        // these symbols will be moved to a separate module.
        for (module_name, entry_points) in split_points_by_module.iter() {
            let mut roots = HashSet::<DepNode>::new();
            for entry_point in entry_points.iter() {
                roots.insert(DepNode::Function(entry_point.export_func));
            }

            let split_functions = ReachabilityGraph::find_reachable_deps(dep_graph, &roots);
            named_modules.push(NamedGraph::new(
                ModuleIdentifier::Split(module_name.clone()),
                split_functions,
            ));
        }

        // Calculate shared deps.
        let shared_deps = NamedGraph::calculate_shared_modules(&mut named_modules, dep_graph);

        let mut split_module_contents = BTreeMap::<SplitModuleIdentifier, OutputModuleInfo>::new();

        split_module_contents.extend(named_modules.into_iter().map(|named_graph| {
            let link_symbols = named_graph.linked_nodes().clone();
            // TODO: Rewrite this
            let split_points = split_points_by_module
                .get(named_graph.module.name())
                .iter()
                .copied()
                .flatten()
                .copied()
                .cloned()
                .collect();
            (
                SplitModuleIdentifier::Single(named_graph.module),
                OutputModuleInfo {
                    included_symbols: named_graph.deps.reachable,
                    link_symbols,
                    split_points,
                },
            )
        }));

        let mut all_links = HashSet::new();

        for shared in shared_deps {
            for module in shared.module_names.iter() {
                let split_module = split_module_contents
                    .get_mut(&SplitModuleIdentifier::Single(module.clone()))
                    .unwrap();
                all_links.extend(split_module.link_symbols.iter().cloned());
            }

            split_module_contents.insert(
                SplitModuleIdentifier::Shared(shared.module_names.clone()),
                OutputModuleInfo {
                    included_symbols: shared.shared_deps,
                    link_symbols: shared.linked_nodes,
                    split_points: vec![],
                },
            );
        }

        let symbol_output_module = split_module_contents
            .iter()
            .enumerate()
            .flat_map(|(output_index, (_, info))| {
                info.included_symbols
                    .iter()
                    .map(move |symbol| (symbol.clone(), output_index))
            })
            .collect();

        Ok(SplitProgramInfo {
            output_modules: split_module_contents.into_iter().collect(),
            shared_nodes: all_links,
            symbol_output_module,
        })
    }
}
