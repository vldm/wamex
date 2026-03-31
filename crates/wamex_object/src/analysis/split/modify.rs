//!
//! A glue between split-points and actual emitting.
//!
//! Contain a code required to convert SplitProgramInfo into copy plan + modifications.
//!

use anyhow::Result;
use cranelift_entity::{EntityRef, PrimaryMap};
use wasmparser::FuncType;

use crate::{
    SVec,
    analysis::{SplitPoint, SplitProgramInfo},
    emit::{
        modify::{EntityModifier, Merge},
        plan::{
            AddressingMode, CopySpec, EmitContext, GotInfo, ImportSpec, OutputModuleCopyPlan,
            PlannedGotInfo,
        },
        relocation::EntityLocation,
    },
    index::Temp,
    typed::{EntityKind, ExportNames, FileId, FileLoader, FunctionRef, ImportedEntity},
};

impl SplitProgramInfo {
    #[tracing::instrument(skip_all, name = "Copy entities")]
    pub fn into_emit_context<'src>(&self, input_files: &'src FileLoader) -> EmitContext<'src> {
        let snapshot = input_files.get_snapshot();
        let outputs = self
            .output_modules
            .iter()
            .map(|(id, info)| {
                let entities = info
                    .defined_symbols
                    .iter()
                    .map(|sym| {
                        (
                            *sym,
                            if info.exports.contains(sym) {
                                let loc = snapshot.unpack_ref(*sym);
                                let module = &input_files.get_file(loc.file_id).module;
                                let name = module.get_name(loc.entity).to_string();
                                CopySpec::wamex_export(name)
                            } else {
                                CopySpec::AsIs
                            },
                        )
                    })
                    .collect();

                // Add imports with original names
                let mut imports = PrimaryMap::new();
                for imported in &info.imports {
                    let loc = snapshot.unpack_ref(*imported);
                    let module = &input_files.get_file(loc.file_id).module;
                    let name = module.get_name(loc.entity).to_string();
                    let Some(ty) = module.get_type(loc.entity) else {
                        continue;
                    };
                    imports.push(ImportSpec::wamex_import(name, ty, *imported));
                }

                let addressing = if id.is_main() {
                    AddressingMode::Static
                } else {
                    let our_got = GotInfo {
                        memory_base: imports.push(ImportSpec::memory_base("")),
                        table_base: imports.push(ImportSpec::table_base("")),
                    };
                    let deps = info
                        .dependencies
                        .keys()
                        .map(|id| {
                            let idx = self
                                .output_modules
                                .iter()
                                .position(|(module_id, _)| module_id == id)
                                .expect("Module from dependencies should be in output modules");
                            let file_id = FileId::from_u32(idx as u32);
                            let got_info = GotInfo {
                                memory_base: imports.push(ImportSpec::memory_base(&id.to_string())),
                                table_base: imports.push(ImportSpec::table_base(&id.to_string())),
                            };
                            (file_id, got_info)
                        })
                        .collect();

                    let planned_deps = PlannedGotInfo { our_got, deps };
                    AddressingMode::GotRelative(planned_deps)
                };

                (
                    id.to_string(),
                    OutputModuleCopyPlan {
                        entities,
                        imports,
                        addressing,
                    },
                )
            })
            .collect();

        EmitContext::new_plan(
            input_files,
            outputs,
            self.symbol_output_module
                .iter()
                .map(|(sym, module_idx)| (sym, FileId::new(*module_idx)))
                .collect(),
        )
    }

    /// Specialized `EmitCtx::copy_entities` impl, that do copying in two steps:
    ///
    /// 1. Process main module - calculate `IndirectFnLayout`
    /// 2. Process rest of modules, with `IndirectFnLayout` as shared state.
    ///
    #[tracing::instrument(skip_all, name = "Copy entities")]
    pub fn copy_entities<'src, M>(
        ctx: &mut EmitContext<'src>,
        entity_modifier_setup: M::SetupData,
    ) -> Result<()>
    where
        M: EntityModifier<'src>,
        M::SetupData: Clone,
    {
        let input_files = ctx.input_files;
        let snapshot = &ctx.snapshot;

        let mut outputs = PrimaryMap::new();

        let main_file_id = FileId::from_u32(0);
        // Process main module first.
        let (name, main_module_plan) = &ctx.output_plans[main_file_id];
        debug_assert_eq!(name, "main");

        // Copy indirect fn layout from main_module.
        let main_output = main_module_plan.copy_entities::<M>(
            input_files,
            snapshot,
            entity_modifier_setup.clone(),
        )?;

        log::error!("fix indirect table len calculation");
        let len = main_output
            .module
            .indirect_function_table
            .items
            .last_key()
            .unwrap_or_default()
            .as_u32()
            + 1;

        // 1. add modifier that will replace SplitPoint imports to `call_indirect <exported fn>`
        // 2. add <export fn> to `OutputEntitiesResolver` and module as `external` entity.
        // 3. at relocation process this entity.

        // main_output
        //     .module
        //     .extra
        //     .mem_layout
        //     .outputs
        //     .push(main_output);

        for (_file_id, (_name, plan)) in ctx.output_plans.iter().skip(1) {
            let output =
                plan.copy_entities::<M>(input_files, snapshot, entity_modifier_setup.clone())?;
            outputs.push(output);
        }
        ctx.output_modules = outputs;
        Ok(())
    }
}

///
/// Convert split imports into indirect calls.
///
/// Split point consist of two methods:
/// - exported function - that contain all the implementation.
///   But which is not called dirrecly.
/// - import fn - that is called by dependent modules, but doesn't contain any implementation.
///
/// After split point processing, import fn calls should be replaced
///  by indirect call to entry filled by module initialization routine.
///
pub struct ConvertSplitImports {
    layout: IndirectFnLayout,

    sp_imports_new_refs: Vec<Temp<FunctionRef>>,
}

#[derive(Debug, Default)]
pub struct SplitModifierResult {
    /// Track of new "external" function that should be resolved to external `call_indirect`
    pub split_imports: SVec<(EntityLocation, Temp<FunctionRef>)>,

    /// Track of internal functions which ids should be placed in corresponding `IndirectFnLayout` entry.
    pub split_exports: SVec<(Temp<FunctionRef>, usize)>,
}
impl Merge for SplitModifierResult {
    fn new() -> Self {
        Self::default()
    }
    fn merge(&mut self, other: Self) {
        self.split_imports.extend(other.split_imports);
        self.split_exports.extend(other.split_exports);
    }
}

impl EntityModifier<'_> for ConvertSplitImports {
    type ExtraData = SplitModifierResult;
    type SetupData = IndirectFnLayout;

    fn setup(
        shared: Self::SetupData,
        _plan: &OutputModuleCopyPlan,
        module: &mut crate::typed::ModuleBuilder<'_>,
    ) -> std::result::Result<Self, anyhow::Error>
    where
        Self: Sized,
    {
        let mut sp_imports_new_refs = Vec::new();
        // Reserve new "external" function for each split point.
        for sp in &shared.dyn_fns {
            let import = sp.import_func();
            let new_ref = module.functions.push_external(ImportedEntity {
                module: "_split".into(),
                name: format!("_split_point_{}", import).into(),
                entity_type: FuncType::new([], []),
                export_as: ExportNames::new(),
                renamed_as: None,
            });
            log::info!(
                "Reserved new function for split point import: {:?} -> {:?}",
                import,
                new_ref
            );
            sp_imports_new_refs.push(new_ref);
        }
        Ok(Self {
            layout: shared,
            sp_imports_new_refs,
        })
    }

    fn modify_entity(
        &self,
        source_info: crate::emit::modify::SourceInfo<'_>,
        entity: crate::emit::modify::AnyEntity<'_, '_>,
    ) -> std::result::Result<Self::ExtraData, anyhow::Error> {
        let EntityKind::Function(f) = source_info.source_entity else {
            return Ok(SplitModifierResult::default());
        };

        if let Some(id) = self
            .layout
            .dyn_fns
            .iter()
            .position(|sp| sp.import_func() == f)
        {
            match entity {
                crate::emit::modify::AnyEntity::Function { .. } => {
                    log::info!(
                        "Found split point import function: {:?}",
                        source_info.source_entity
                    );
                    let new_ref = self.sp_imports_new_refs[id];
                    // place to add new function.
                    return Ok(SplitModifierResult {
                        split_imports: SVec::from(vec![(
                            EntityLocation::from_parts(
                                source_info.input_file,
                                source_info.source_entity,
                            ),
                            new_ref,
                        )]),
                        split_exports: SVec::new(),
                    });
                }
                _ => unreachable!("Only function imports should be in split point imports list"),
            }
        } else if let Some(id) = self
            .layout
            .dyn_fns
            .iter()
            .position(|sp| sp.export_func() == f)
        {
            match entity {
                crate::emit::modify::AnyEntity::Function { new_ref, .. } => {
                    log::info!(
                        "Found split point export function: {:?}",
                        source_info.source_entity
                    );
                    return Ok(SplitModifierResult {
                        split_imports: SVec::new(),
                        split_exports: SVec::from(vec![(new_ref, id)]),
                    });
                }
                _ => unreachable!("Only function exports should be in split point exports list"),
            }
        }
        Ok(SplitModifierResult::default())
    }
    fn finish(
        &self,
        module: &mut crate::typed::Module<'_>,
        resolver: &mut crate::emit::relocation::resolver::OutputEntitiesResolver,
        aggregated_data: Self::ExtraData,
    ) -> std::result::Result<(), anyhow::Error> {
        // Add new functions to resolver.
        for (src, output) in aggregated_data.split_imports {
            resolver.add_entity_mapping(src, module.functions.stable_id(output).into());
        }
        // fill indirect function table entries for split point exports.
        todo!();
        Ok(())
    }
}

/// The indirect_function table is shared between main module and submodules.
/// it's layout is:
/// [ 0: empty ]
/// [ 1..N: functions used in this module ]
/// [ N+1..N+M: reserved space for lazy stubs, main module fill it empty, and submodules fill it with stubs ]
/// [ N+M+1.. : dynamic allocated entries - used for tables in submodules ]
///
/// Example of final layout:
///  1. After main load:
///     [0, f1, f2, f3, ..., s1_entry1_uninit, s1_entry2_uninit, s2_entry1_uninit, ...]
///  2. After submodule load:
///     [0, f1, f2, f3, ..., s1_entry1,        s1_entry2,       s1_f1, s1_f2, ...]
///  3. If submodule reloaded, the following changes are applied:
///     [_, _, _, _, ...,    s1_FIX_entry1,    s1_FIX_entry2,   s1_f1, s1_f2,     s1_FIX_f1, s1_FIX_f2, ...]
///  
///  Note that original s1_f1 and s1_f2 are not removed, because other submodules may still use them.
///  And only after calling linker::unload we can reuse these entries.
#[derive(Debug, Eq, PartialEq, Clone)]
pub struct IndirectFnLayout {
    pub start_dyn: usize,
    pub dyn_fns: Vec<SplitPoint>,
}

// impl IndirectFnLayout {
//     pub fn new_raw(
//         module: &Module,
//         split_points: &[SplitPoint],
//         snapshot: EntitiesSnapshot,
//     ) -> Self {
//         let start_dyn = module.indirect_function_table;
//         let dyn_fns = split_points
//             .iter()
//             .map(|sp| sp.export_func())
//             .map(|func| snapshot.pack_ref(func))
//             .collect();
//         Self {
//             start_dyn,
//             dyn_fns,
//             snapshot,
//         }
//     }
//     /// Return place reserved for given split point in the flat indirect functions list.
//     pub fn get_split_point_index(&self, split_point: &SplitPoint) -> usize {
//         let id = self.snapshot.pack_ref(split_point.export_func());
//         self.start_dyn
//             + self
//                 .dyn_fns
//                 .iter()
//                 .position(|f| *f == id)
//                 .expect("Split point export function not found in indirect functions layout")
//     }
// }
