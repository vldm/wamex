//! Helpers for split-point analysis and manipulation.
//! - Working with wasm-bindgen descriptors and closures.
//! - calculating main roots
//! - merging shared with main modules
//! - processing special entities (memory, indirect table, stack pointer)

use std::collections::BTreeMap;

use wamex_types::map_vec::MiniSet;

use super::{ModuleIdentifier, OutputModuleInfo, SplitModuleIdentifier, SplitPoint};
use crate::{
    analysis::dep_graph::{DepGraph, DepSet, SharedEntry},
    typed::{
        Module,
        snapshot::{EntitiesSnapshot, FlatEntityRef},
    },
};

fn is_wasm_bindgen_cast(name: &str) -> bool {
    name == "__wbindgen_describe_closure"
        || name == "__wbindgen_describe_cast"
        || name == "__wbindgen_describe"
}

#[tracing::instrument(skip_all)]
pub fn wbg_closures(module: &Module, graph: &DepGraph) -> MiniSet<FlatEntityRef> {
    let mut wbg_closures = std::collections::BTreeSet::new();

    let snapshot = EntitiesSnapshot::new_without_types(module);

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

    info.extra.start_function.iter().for_each(|start_fn| {
        roots.insert(snapshot.pack_ref(*start_fn));
    });

    for index in info.entities_exports() {
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

/// Process special entities (memory, __indirect_function_table)
/// - Mark them as exported in main module.
/// - Add imports to modules that use them.
pub fn process_special_entities(
    info: &Module,
    snapshot: &EntitiesSnapshot,
    (main_id, main): &mut (SplitModuleIdentifier, OutputModuleInfo),
    regular_modules: &mut [(SplitModuleIdentifier, OutputModuleInfo)],
    shared: &mut [SharedEntry<ModuleIdentifier>],
) -> anyhow::Result<()> {
    assert_eq!(
        &*main_id,
        &SplitModuleIdentifier::Single(ModuleIdentifier::Main)
    );
    if info.memories.len() != 1 {
        log::error!(
            "Expected more than one memory in source module, {:?}",
            info.memories
        );
    };

    let mut special_entities = Vec::<(&str, FlatEntityRef)>::new();

    // 1. copy all memories to the list;
    special_entities.extend(
        info.memories
            .iter_all_ids()
            .map(|id| ("memory", snapshot.pack_ref(id))),
    );
    // 2. if indirect table exist - add it to the list as well.
    special_entities.push((
        "indirect_function_table",
        snapshot.pack_ref(info.indirect_function_table.table_id),
    ));

    // 3. add global if it is used by main module (e.g. for stack pointer)
    let global = info
        .find_global_id_by_name("__stack_pointer")
        .map(|id| snapshot.pack_ref(id))
        .expect("__stack_pointer global should be present in the module");

    special_entities.push(("stack_pointer", global));

    // now for special entities - mark them as exported in main module, and add imports to modules that use them:
    for (name, entity) in special_entities {
        log::trace!("Adding export of special entity {name}({entity:?}) to main module.",);
        main.exports.insert(entity);
        main.defined_symbols.insert(entity);

        for (module_id, module_info) in regular_modules.iter_mut() {
            if module_info.imports.contains(&entity) {
                continue;
            }
            log::trace!(
                "Adding import of special entity {name}({entity:?}) to module {module_id:?}",
            );
            module_info.imports.insert(entity);
        }
        for shared_module in shared.iter_mut() {
            if shared_module.imports.contains(&entity) {
                continue;
            }
            log::trace!(
                "Adding import of special entity {name}({entity:?}) to module {module_id:?}",
                module_id = shared_module
                    .module_names
                    .iter()
                    .map(|m| m.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            shared_module.imports.insert(entity);
        }
    }

    Ok(())
}
