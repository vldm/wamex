use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Display,
};

use anyhow::Result;
use wamex_types::{BumpVersion, ModuleId, map_vec::MiniSet};

use crate::{
    ModuleIdentifier, SplitModuleIdentifier, SplitPointExtractor,
    analysis::{
        self, StaticModuleInfo,
        split_point::{OutputModuleInfo, SharedModuleIdentifier},
        symbols::DiffEntry,
    },
    index::{IdMap, SymbolId},
};

pub type ModuleDeps = BTreeMap<ModuleId, Vec<ModuleId>>;
pub struct ModuleUpdate {
    pub module_id: ModuleId,
    pub force_restart: bool,
}
pub enum IncrementalSplitResult {
    Unchanged,
    UpdatedModules(Vec<ModuleUpdate>),
    FullResplit,
}

pub struct SplitResult {
    pub deps: ModuleDeps,
    pub incremental_result: IncrementalSplitResult,
    _non_exhaustive: (),
}

pub struct IncrementalSplitState {
    // Symbols map for the latest source module.
    last_module_info: StaticModuleInfo,
    last_module_structure: Vec<(SplitModuleIdentifier, OutputModuleInfo)>,
    modules_versions: BTreeMap<SplitModuleIdentifier, BumpVersion>,
    bump_version: BumpVersion,
    // last structure of split modules?.
}
impl IncrementalSplitState {
    pub fn new() -> Self {
        Self {
            last_module_info: StaticModuleInfo::default(),
            last_module_structure: Vec::new(),
            modules_versions: BTreeMap::new(),
            bump_version: BumpVersion::default(),
        }
    }

    pub fn split_incremental(
        &mut self,
        input_wasm: &[u8],
        verbose: bool,
        precise_modification: bool,
        split_point_extractor: SplitPointExtractor,
        mut emit_module_fn: impl FnMut(ModuleId, &[u8]) -> Result<()>,
    ) -> Result<SplitResult> {
        // 1. Analyze new module.
        let module = crate::InputModule::parse(input_wasm)?;
        let info = analysis::ModuleInfo::from_raw_module(module)?;
        let dep_graph = analysis::dep_graph::get_dependencies(&info)?;
        let split_points = analysis::split_point::find_split_points(&info, split_point_extractor)?;
        let mut split_program_info =
            crate::SplitProgramInfo::compute_split_modules(&info, &dep_graph, &split_points)?;

        // one of the possible mode is to merge all shared with main chunks into main module.
        // The other way can be used in incremental build, when main is not changed but we emit "mini-main".
        crate::emit::merge_main_shared(&mut split_program_info);

        // some wbg functions need to be moved to main before splitting.
        let wbg_fns =
            crate::emit::hoist_wbg_deps_to_main(&info, &dep_graph, &mut split_program_info);
        if verbose {
            println!("Split points: {split_points:?}");
            println!("Split program info: {split_program_info:?}");
            println!("Dependency graph: {dep_graph:?}");
            println!("Module symbols:");
            info.symbols.print_debug();

            println!("Module split details:");
            for (name, split_deps) in split_program_info.output_modules.iter() {
                split_deps.print(format!("{:?}", name).as_str(), &info, &dep_graph);
            }
        }

        // TODO: use (module_id, exports, imports) list as well to detect changes.
        let module_structure = split_program_info.output_modules.clone();
        let structure_diff = StructureDiffResult::new(
            &self.last_module_structure,
            &self.last_module_info,
            &module_structure,
            &info,
        );
        log::info!("Incremental split analysis result: {}", structure_diff);
        if structure_diff.not_changed() {
            log::info!("No changes detected in split modules.");
            return Ok(SplitResult {
                deps: self.build_deps_map(),
                incremental_result: IncrementalSplitResult::Unchanged,
                _non_exhaustive: (),
            });
        }
        // 4. Update state.
        self.last_module_structure = module_structure;
        self.last_module_info = StaticModuleInfo::new(&info);
        self.bump_version.bump();
        let whitelist = structure_diff.whitelist();

        // 5.  Re-split only changed modules.
        let emit_fn = |identifier: &SplitModuleIdentifier, data: &[u8]| -> Result<()> {
            self.modules_versions
                .insert(identifier.clone(), self.bump_version.clone());

            let module_id = self.last_module_id(identifier);
            emit_module_fn(module_id, data)
        };

        crate::emit::emit_modules(
            &info,
            verbose,
            &split_program_info,
            &wbg_fns,
            precise_modification,
            whitelist.as_ref(),
            emit_fn,
        )?;

        if let Some(changed_modules) = whitelist {
            Ok(SplitResult {
                deps: self.build_deps_map(),
                incremental_result: IncrementalSplitResult::UpdatedModules(
                    self.build_update_list(changed_modules, structure_diff),
                ),
                _non_exhaustive: (),
            })
        } else {
            Ok(SplitResult {
                deps: self.build_deps_map(),
                incremental_result: IncrementalSplitResult::FullResplit,
                _non_exhaustive: (),
            })
        }
    }

    // Build a set of changed modules.
    // It could be directly changed modules
    fn build_update_list(
        &self,
        changed_list: BTreeSet<SplitModuleIdentifier>,
        structure_diff: StructureDiffResult,
    ) -> Vec<ModuleUpdate> {
        changed_list
            .into_iter()
            .map(|id| ModuleUpdate {
                module_id: self.last_module_id(&id),
                force_restart: !structure_diff.only_struct_dep_change(&id),
            })
            .collect()
    }

    fn last_module_id(&self, module: &SplitModuleIdentifier) -> ModuleId {
        ModuleId::new_from_components(
            module.to_string(),
            Some(
                self.modules_versions
                    .get(module)
                    .expect("Module version must be present for last emitted module")
                    .clone(),
            ),
            None,
        )
    }

    fn build_deps_map(&self) -> ModuleDeps {
        let shared_modules = self
            .last_module_structure
            .iter()
            .filter_map(|(id, _)| id.as_shared().cloned())
            .collect::<Vec<_>>();

        let mut deps = BTreeMap::new();
        for (id, _) in &self.last_module_structure {
            if id.is_shared() {
                continue;
            }

            let module_id = self.last_module_id(id);
            let module_deps = id.collect_deps(&shared_modules);
            let prev = deps.insert(
                module_id,
                module_deps
                    .into_iter()
                    .map(|m| self.last_module_id(&SplitModuleIdentifier::Shared(m)))
                    .collect(),
            );
            debug_assert!(prev.is_none());
        }
        deps
    }
}

#[derive(Debug)]
struct StructureDiffResult {
    // Modules with changed symbols
    with_changed_symbols: BTreeSet<SplitModuleIdentifier>,
    // Modules with changed exports (only shared).
    with_changed_exports: BTreeSet<SharedModuleIdentifier>,

    // Modules that has no symbol changes, but depends on changed modules.
    changed_deps: BTreeMap<SplitModuleIdentifier, bool /* child modified */>,
    // Structure of split modules changed
    structure_changed: bool,
}
impl StructureDiffResult {
    fn main_changed(&self) -> bool {
        self.with_changed_symbols
            .contains(&SplitModuleIdentifier::Single(ModuleIdentifier::Main))
    }

    pub fn not_changed(&self) -> bool {
        !self.structure_changed
            && self.with_changed_symbols.is_empty()
            && self.with_changed_exports.is_empty()
            && self.changed_deps.is_empty()
    }
    pub fn need_full_rebuild(&self) -> bool {
        self.structure_changed || self.main_changed()
    }
    fn entrypoint_list(
        module_structure: &[(SplitModuleIdentifier, OutputModuleInfo)],
    ) -> Vec<SplitModuleIdentifier> {
        module_structure
            .iter()
            .filter(|(m, _)| !m.is_shared())
            .map(|(m, _)| m.clone())
            .collect()
    }

    // There is no direct change of module, and dep structure changed only.
    pub fn only_struct_dep_change(&self, module: &SplitModuleIdentifier) -> bool {
        self.changed_deps.get(module).copied().unwrap_or(false)
    }
    /// List of modules that need to be reemitted.
    pub fn whitelist(&self) -> Option<BTreeSet<SplitModuleIdentifier>> {
        if self.need_full_rebuild() {
            return None;
        }
        let mut whitelist = self.with_changed_symbols.clone();
        whitelist.extend(
            self.with_changed_exports
                .iter()
                .map(|m| SplitModuleIdentifier::Shared(m.clone())),
        );
        whitelist.extend(self.changed_deps.keys().cloned());

        Some(whitelist)
    }

    // Find shared modules with changed exports.
    fn find_changed_sig<SymMapper>(
        old_modules: &[(SplitModuleIdentifier, OutputModuleInfo)],
        new_modules: &[(SplitModuleIdentifier, OutputModuleInfo)],
        sym_mapper: SymMapper,
    ) -> BTreeSet<SharedModuleIdentifier>
    where
        SymMapper: Fn(&SymbolId) -> Option<SymbolId>,
    {
        let old_set: BTreeMap<SplitModuleIdentifier, &OutputModuleInfo> = old_modules
            .iter()
            .map(|(m, info)| (m.clone(), info))
            .collect();
        let mut new_chunks = BTreeSet::new();
        for (module, info) in new_modules {
            let Some(shared) = module.as_shared() else {
                continue;
            };
            if let Some(old_info) = old_set.get(module) {
                let exports_list = old_info
                    .exports
                    .iter()
                    .map(|s| sym_mapper(s))
                    .collect::<Option<MiniSet<SymbolId>>>();
                if let Some(exports_list) = exports_list {
                    if exports_list == info.exports {
                        continue;
                    }
                }
            }
            // If
            // - Module is new ||
            // - no mapping for some exported symbol (deleted) ||
            // - export_list changed
            // mark module as changed.
            new_chunks.insert(shared.clone());
        }
        new_chunks
    }

    // Find modules, that uses shared module.
    // TODO: 1. this is brute-force, 2. we add more deps that needed (not all deps are using shared module).
    fn shared_module_users(
        shared: &SharedModuleIdentifier,
        list: &[(SplitModuleIdentifier, OutputModuleInfo)],
    ) -> Vec<SplitModuleIdentifier> {
        list.iter()
            .filter(|(m, _)| shared.includes(m) && shared != m)
            .map(|(m, _)| m.clone())
            .collect()
    }

    // Find module from module list that uses some of changed shared modules.
    fn collect_changed_users<'a>(
        changed_shared: impl Iterator<Item = &'a SharedModuleIdentifier>,
        new_module_structure: &[(SplitModuleIdentifier, OutputModuleInfo)],
    ) -> BTreeSet<SplitModuleIdentifier> {
        let mut changed_deps = BTreeSet::new();
        for shared in changed_shared {
            let users = Self::shared_module_users(shared, new_module_structure);
            for user in users {
                changed_deps.insert(user);
            }
        }
        changed_deps
    }

    pub fn new(
        old_module_structure: &[(SplitModuleIdentifier, OutputModuleInfo)],
        old_module_info: &StaticModuleInfo,
        new_module_structure: &[(SplitModuleIdentifier, OutputModuleInfo)],
        new_module_info: &analysis::ModuleInfo<'_>,
    ) -> Self {
        let old_entrypoits = Self::entrypoint_list(old_module_structure);
        let new_entrypoints = Self::entrypoint_list(new_module_structure);
        // List of entrypoitns is defined in indirect function table and must be stable between runs.
        // TODO: we should not only check module list - but also check their entrypoint functions.
        let rebuild_full = old_entrypoits != new_entrypoints;

        let mut debug_module_changed_info = BTreeMap::new();

        // Diff with last_module_info.
        if rebuild_full {
            log::info!("Split module structure changed, performing full rebuild.");
            return Self {
                with_changed_symbols: BTreeSet::new(),
                with_changed_exports: BTreeSet::new(),
                changed_deps: BTreeMap::new(),
                structure_changed: true,
            };
        }

        // Build symbol -> module map.
        let mut symbol_map = IdMap::<SymbolId, SplitModuleIdentifier>::new();
        for (module_id, split_module) in new_module_structure.iter() {
            for symbol in &split_module.defined_symbols {
                symbol_map.insert(*symbol, module_id.clone());
            }
        }

        // iterate over changed symbols - and mark modules as changed.
        let differ = analysis::symbols::Differ::new(old_module_info, new_module_info);
        let sym_map = differ.symbol_map();
        let diff_result = differ.build_diff(&sym_map);
        let changed_syms = diff_result
            .all_changes()
            .filter_map(|entry| match entry {
                DiffEntry::Added { right } | DiffEntry::Replaced { right, .. } => Some(right),
                _ => None,
            })
            .copied()
            .collect::<Vec<_>>();

        // Identify changed split points.
        let mut with_changed_symbols = BTreeSet::new();
        for sym in changed_syms {
            if let Some(module_id) = symbol_map.get(sym) {
                with_changed_symbols.insert(module_id.clone());
                debug_module_changed_info
                    .entry(module_id.clone())
                    .or_insert_with(Vec::new)
                    .push(sym);
            }
        }

        let mut with_changed_exports =
            Self::find_changed_sig(old_module_structure, new_module_structure, |sym| {
                differ.symbol_map().map(*sym)
            });

        let changed_symbols_users = Self::collect_changed_users(
            with_changed_symbols.iter().filter_map(|m| match m {
                SplitModuleIdentifier::Shared(shared) => Some(shared),
                _ => None,
            }),
            new_module_structure,
        )
        .into_iter()
        .map(|m| (m, true)); // child has changed symbols

        let changed_exports_users =
            Self::collect_changed_users(with_changed_exports.iter(), new_module_structure)
                .into_iter()
                .map(|m| (m, false)); // child is just restructured

        let mut changed_deps = std::iter::chain(changed_symbols_users, changed_exports_users)
            .collect::<BTreeMap<_, _>>();

        // Remove duplicates
        with_changed_exports
            .retain(|m| !with_changed_symbols.contains(&SplitModuleIdentifier::Shared(m.clone())));

        changed_deps.retain(|m, _| {
            !with_changed_symbols.contains(m)
                && m.as_shared()
                    .map(|s| !with_changed_exports.contains(s))
                    .unwrap_or(true)
        });

        // TODO: assert list of imports remain unchanged (for unchanged modules).
        // TODO: if imports changed - mark module as changed.
        let this = Self {
            with_changed_symbols,
            with_changed_exports,
            structure_changed: rebuild_full,
            changed_deps,
        };
        // If main changed - perform full rebuild.
        if this.main_changed() {
            log::info!("Main module changed, performing full rebuild.");
        }

        if !debug_module_changed_info.is_empty() {
            log::info!("Changed modules and symbols:");
            for (module, symbols) in debug_module_changed_info.iter() {
                log::info!("  Module {:?} changed symbols:", module);
                for sym in symbols {
                    let symbol = new_module_info.symbols.get(*sym).unwrap();
                    log::info!(
                        "    Symbol {:?} <{}>",
                        sym,
                        crate::helpers::demangle_full(&symbol.name)
                    );
                }
            }
        }

        this
    }
}

impl Display for StructureDiffResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.not_changed() {
            return write!(f, "No changes detected");
        }
        // print in format [module1, module2, ...]
        if !self.with_changed_symbols.is_empty() {
            let with_changed_symbols = self
                .with_changed_symbols
                .iter()
                .map(|m| m.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            write!(f, " Modules with changed symbols: {}", with_changed_symbols)?;
        }
        if !self.with_changed_exports.is_empty() {
            let with_changed_exports = self
                .with_changed_exports
                .iter()
                .map(|m| m.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            write!(f, " Modules with changed exports: {}", with_changed_exports)?;
        }

        if !self.changed_deps.is_empty() {
            let changed_deps = self
                .changed_deps
                .iter()
                .map(|(m, v)| format!("{m}, soft_reload: {v}"))
                .collect::<Vec<_>>()
                .join(", ");
            write!(f, " Changed dependent modules: {}", changed_deps)?;
        }
        Ok(())
    }
}
