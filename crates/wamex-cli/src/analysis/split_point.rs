use std::collections::BTreeMap;

use anyhow::{Context, bail};
use wamex_object::emit::split::{
    ModuleIdentifier, OutputModuleInfo, SharedModuleIdentifier, SplitModuleIdentifier, SplitPoint,
    SplitProgramInfo,
};
use wamex_types::map_vec::MiniSet;

use super::dep_graph::{DepGraph, DepMiniSet, DepSet, NamedGraph, find_reachable_deps};
use crate::{
    SplitPointExtractor, analysis,
    index::{ExportId, IdMap, ImportId, InputFuncId, SymbolId},
};

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

pub(crate) const SPLIT_IMPORT_POSTFIX: &str = "00_import_";
pub(crate) const SPLIT_EXPORT_POSTFIX: &str = "00_export_";

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

    process_imports_or_exports!(SPLIT_IMPORT_POSTFIX, import_map, imports, ImportId);
    process_imports_or_exports!(SPLIT_EXPORT_POSTFIX, export_map, exports, ExportId);
    let mut export_map = export_map;

    let split_points = import_map
        .into_iter()
        .map(|(key, import_id)| -> anyhow::Result<SplitPoint> {
            let export_id = export_map
                .remove(&key)
                .with_context(|| format!("No corresponding export for split import {key:?}"))?;
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
                .with_context(|| {
                    format!(
                        "Expected imported function but received: {:?}",
                        &info.wasm.imports[import_id]
                    )
                })?;

            Ok(SplitPoint {
                module_name: key.0,
                unique_id: key.1,
                import: import_id,
                import_func,
                export: export_id,
                export_func: InputFuncId::from_index(index),
            })
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

pub fn find_split_points_legacy(info: &analysis::ModuleInfo) -> anyhow::Result<Vec<SplitPoint>> {
    find_split_points_with_prefix(info, "__wasm_split_00")
}

pub(crate) const WAMEX_ENTRY_PREFIX: &str = "__wamex_00";

fn find_split_points_wamex(info: &analysis::ModuleInfo) -> anyhow::Result<Vec<SplitPoint>> {
    find_split_points_with_prefix(info, WAMEX_ENTRY_PREFIX)
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

fn is_wasm_bindgen_cast(name: &str) -> bool {
    name == "__wbindgen_describe_closure" || name == "__wbindgen_describe_cast"
}

pub fn wbg_closures(module: &analysis::ModuleInfo, graph: &DepGraph) -> MiniSet<SymbolId> {
    let wbg_fns: std::collections::BTreeSet<_> = module
        .symbols
        .iter()
        .filter(|(_id, sym)| is_wasm_bindgen_cast(&sym.name))
        .map(|(id, _)| id)
        .collect();

    let mut wbg_descriptors = std::collections::BTreeSet::new();
    for id in wbg_fns.iter().cloned() {
        wbg_descriptors.insert(id);
        if let Some(parents) = graph.get_parents(id) {
            for parent in parents {
                debug_assert!(module.symbols.is_function(*parent));
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

pub fn compute_split_modules(
    info: &analysis::ModuleInfo,
    dep_graph: &DepGraph,
    split_points: &[SplitPoint],
    wbg_descriptors: &MiniSet<SymbolId>,
) -> anyhow::Result<SplitProgramInfo> {
    let split_points_by_module = merge_split_points_by_name(split_points);

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

    for (index, (_, import)) in info.wasm.imports.iter().enumerate() {
        let wasmparser::Import {
            ty: wasmparser::TypeRef::Func(_),
            ..
        } = import
        else {
            continue;
        };
        roots.insert(
            info.symbols
                .get_function_symbol(InputFuncId::from_index(index))
                .unwrap(),
        );
    }

    for descriptor in wbg_descriptors {
        roots.insert(*descriptor);
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

    let main_deps = find_reachable_deps(dep_graph, &roots);

    let mut named_modules = vec![NamedGraph::new(ModuleIdentifier::Main, main_deps.clone())];

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

    let shared_deps = NamedGraph::calculate_shared_modules(&mut named_modules, dep_graph);

    let mut split_module_contents = BTreeMap::<SplitModuleIdentifier, OutputModuleInfo>::new();

    split_module_contents.extend(named_modules.into_iter().map(|named_graph| {
        let imports = named_graph.imports().clone();
        let split_points = split_points_by_module
            .get(&named_graph.module.to_string())
            .cloned()
            .unwrap_or_default();

        (
            SplitModuleIdentifier::Single(named_graph.module),
            OutputModuleInfo {
                defined_symbols: named_graph.reachable,
                imports,
                split_points,
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
        .flat_map(|(output_index, (_id, info))| {
            info.defined_symbols
                .iter()
                .map(move |symbol| (*symbol, output_index))
        })
        .collect::<IdMap<SymbolId, usize>>();

    let output_modules = split_module_contents.into_iter().collect::<Vec<_>>();

    Ok(SplitProgramInfo {
        output_modules,
        symbol_output_module,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
