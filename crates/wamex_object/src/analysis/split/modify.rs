//!
//! A glue between split-points and actual emitting.
//!
//! Contain a code required to convert SplitProgramInfo into copy plan + modifications.
//!

use anyhow::Result;
use cranelift_entity::{EntityRef, SecondaryMap};
use smallvec::{SmallVec, smallvec};
use wasmparser::FuncType;

use crate::{
    SVec,
    analysis::{SplitModuleIdentifier, SplitModuleInfo, SplitPoint, SplitProgramInfo},
    emit::{
        modify::{AbsToGot, Blacklist, IsSet, abs_to_got},
        plan::{AnyEntity, DyLinkInfo, EmitContext, GotInfo, Merge, OutputModule, OutputPlan},
        relocation::EntityLocation,
    },
    index::Temp,
    layouts::{
        ElementInTable, ElementItemId, ElementKind, ElementSegmentSpec, SegmentPlacement,
        VirtualSpaceId,
    },
    typed::{
        EntityBody, EntityType, ExportNames, FileId, FileLoader, FunctionRef, GlobalRef,
        ImportOrDefined, ImportedEntity, Module, TableRef, TempEntityKind,
        snapshot::{self, FlatEntityRef, MultiSnapshot},
    },
};

#[derive(Debug, Clone)]
enum ExtraExport {
    AddExport(String),
    Nothing,
}

enum GotConverter<F>
where
    Blacklist<F>: IsSet,
{
    Static,
    NotInitted {
        blacklist: Blacklist<F>,
        main_layout: IndirectFnLayout,
        // List of exported split points, that should be placed in indirect function table and exported as is.
        sp_exports: Vec<FlatEntityRef>,
    },
    Initted {
        converter: AbsToGot<F>,
        main_layout: IndirectFnLayout,
        // List of exported split points, that should be placed in indirect function table and exported as is.
        sp_exports: Vec<FlatEntityRef>,
    },
}

#[derive(Debug)]
struct TrampolineCalculated {
    // External fn that was created
    external_ref: Temp<FunctionRef>,
    // Index that should be used in resolving of this trampoline.
    sp_index: usize,
}

#[derive(Debug, Clone)]
struct TrampolineDeclared {
    flat_entity: FlatEntityRef,
    src_location: EntityLocation,
    src_type: EntityType,

    // Name of trampoline, used for debug purposes.
    name: String,
    // Extra export info, that should be applied to created trampoline.
    extra: ExtraExport,
}

#[derive(Debug, Clone, Copy)]
struct TrampolineCreated {
    // Function ref of created trampoline
    defined_func: Temp<FunctionRef>,
    src_ref: FlatEntityRef,
}

enum TrampolinesState {
    Declared {
        // List of external used split points, that should be called as `call_indirect` instructions
        // (so we need to create trampolines for them and link with original `FlatEntityRef` for relocations).
        trampoline_fns: Vec<TrampolineDeclared>,
    },
    Created {
        // Trampolines was created, but need final layout to calculate their offsets.
        trampoline_fns: SVec<TrampolineCreated>,
    },
}

#[derive(Debug)]
struct Export {
    // Reference to fn that was created for exported split point function.
    defined_ref: Temp<FunctionRef>,
    // Index that should be used in resolving of this export.
    sp_index: usize,
}

struct SplitModulePlan<F>
where
    Blacklist<F>: IsSet,
{
    // list of defined entities that should be copied as is from input modules
    entities: Vec<(FlatEntityRef, ExtraExport)>,

    // List of entities from other modules that should be emitted as
    // import entry. (Data entity should be placed as `external`).
    static_imports: Vec<(EntityLocation, (EntityType, FileId, String))>,

    //
    // setup results
    //
    ///
    /// Marker for main module, that allows enforcing got-relative addressing for other modules.
    ///
    is_main: GotConverter<F>,

    // List of external used split points, that should be called as `call_indirect` instructions
    // (so we need to create trampolines for them and link with original `FlatEntityRef` for relocations).
    trampoline_fns: TrampolinesState,

    /// Track of newly created entities that have reference to input modules
    tmp_refs: Vec<(EntityLocation, TempEntityKind)>,
}

struct SplitModuleResult {
    abs_to_got_res: abs_to_got::Artifact,
    /// Track of internal functions which ids should be placed in corresponding `IndirectFnLayout` entry.
    sp_defined_exports: SVec<Export>,
}
impl Merge for SplitModuleResult {
    fn new() -> Self {
        Self {
            abs_to_got_res: abs_to_got::Artifact::new(),
            sp_defined_exports: SVec::new(),
        }
    }
    fn merge(&mut self, other: Self) {
        self.abs_to_got_res.merge(other.abs_to_got_res);
        self.sp_defined_exports.extend(other.sp_defined_exports);
    }
}

impl SplitModulePlan<fn(FlatEntityRef) -> bool> {
    pub fn new_main(
        snapshot: &snapshot::MultiSnapshot,
        input_files: &FileLoader,
        info: &SplitModuleInfo,
        dep_info: &SecondaryMap<FlatEntityRef, usize>,
        split_points: Vec<SplitPoint>,
    ) -> Self {
        assert!(
            info.split_points.is_empty(),
            "Main module should not have split points, but has {} split points",
            info.split_points.len()
        );
        Self::new_inner(
            snapshot,
            input_files,
            info,
            GotConverter::Static,
            dep_info,
            |r| split_points.iter().any(|sp| sp.import_func() == r),
        )
    }
}

impl<F> SplitModulePlan<F>
where
    Blacklist<F>: IsSet,
{
    pub fn new_submodule(
        snapshot: &snapshot::MultiSnapshot,
        input_files: &FileLoader,
        (id, info): &(SplitModuleIdentifier, SplitModuleInfo),
        dep_info: &SecondaryMap<FlatEntityRef, usize>,
        split_points: Vec<SplitPoint>,
        blacklist: Blacklist<F>,
        static_layout: IndirectFnLayout,
    ) -> Self {
        let dynamic = if !info.split_points.is_empty() {
            "dynamic"
        } else {
            "static"
        };
        log::debug!(
            "Creating plan for {dynamic} module {id} with {} split points",
            info.split_points.len()
        );
        Self::new_inner(
            snapshot,
            input_files,
            info,
            GotConverter::NotInitted {
                sp_exports: info
                    .split_points
                    .iter()
                    .map(|sp| sp.export_func())
                    .collect(),
                blacklist,
                main_layout: static_layout,
            },
            dep_info,
            |r| split_points.iter().any(|sp| sp.import_func() == r),
        )
    }
    fn new_inner<U>(
        snapshot: &snapshot::MultiSnapshot,
        input_files: &FileLoader,

        info: &SplitModuleInfo,
        got_converter: GotConverter<F>,
        dep_info: &SecondaryMap<FlatEntityRef, usize>,
        is_split_point_import: U,
    ) -> Self
    where
        U: Fn(FlatEntityRef) -> bool,
    {
        let mut dyn_import_fns = Vec::new();
        let mut entities = Vec::new();
        for &defined in &info.defined_symbols {
            let extra = if info.exports.contains(&defined) {
                let loc = snapshot.unpack_ref(defined);
                let module = &input_files.get_file(loc.file_id).module;
                let name = module.get_name(loc.entity).to_string();
                ExtraExport::AddExport(name)
            } else {
                ExtraExport::Nothing
            };

            if is_split_point_import(defined) {
                let loc = snapshot.unpack_ref(defined);
                let module = &input_files.get_file(loc.file_id).module;
                let name = module.get_name(loc.entity).to_string();
                let entity_ty = module
                    .get_type(loc.entity)
                    .expect("Entity should have type")
                    .clone();
                dyn_import_fns.push(TrampolineDeclared {
                    flat_entity: defined,
                    name,
                    src_location: loc,
                    src_type: entity_ty,
                    extra,
                });
            } else {
                entities.push((defined, extra));
            }
        }

        let static_imports = info
            .imports
            .iter()
            .map(|import| {
                let file_id = FileId::new(dep_info[*import]);

                let loc = snapshot.unpack_ref(*import);
                let module = &input_files.get_file(loc.file_id).module;
                let name = module.get_name(loc.entity).to_string();
                let entity_ty = module
                    .get_type(loc.entity)
                    .expect("Entity should have type")
                    .clone();
                (loc, (entity_ty, file_id, name))
            })
            .collect();

        Self {
            entities,
            static_imports,
            trampoline_fns: TrampolinesState::Declared {
                trampoline_fns: dyn_import_fns,
            },
            is_main: got_converter,
            tmp_refs: Vec::new(),
        }
    }

    ///
    /// Create trampoline function without body
    /// - body will be filled on finish with call_indirect to static index.
    ///
    fn declare_trampoline(
        trampoline: &TrampolineDeclared,
        module: &mut crate::typed::ModuleBuilder<'_>,
    ) -> TrampolineCreated {
        let mut export_as = ExportNames::default();
        if let ExtraExport::AddExport(name) = &trampoline.extra {
            export_as.add_export(name.clone().into());
        }
        let new_defined_ref = module
            .functions
            .push_defined(crate::typed::DefinedFunction {
                entity_type: FuncType::new([], []),
                body: EntityBody::new_empty(smallvec![]),
                name: Some(trampoline.name.clone().into()),
                export_as,
            });

        TrampolineCreated {
            src_ref: trampoline.flat_entity,
            defined_func: new_defined_ref,
        }
    }

    /// Add segment with initialisation of exported split points.
    fn add_export_init_segment(
        module: &mut crate::typed::ModuleBuilder<'_>,
        sp_exports: &[Export],
    ) {
        // Get first virtual space
        // Add virtual space with this table ref,
        let vs = module
            .extra
            .get_indirect_fn_vs()
            .expect("Indirect function table should be created");

        let function_elements = &mut module.extra.function_elements;
        let vs = &function_elements.virtual_spaces[vs];
        let table_ref = vs.table().expect("Virtual space should be active");

        for Export {
            defined_ref,
            sp_index,
        } in sp_exports.iter()
        {
            log::trace!(
                "Adding export init for split point {defined_ref} with index {sp_index} in indirect function table"
            );

            let location = SegmentPlacement::ConstantOffset(*sp_index as u32);
            // adding as new virtual space will prevent it future merging.
            let new_space = function_elements.virtual_spaces.push(ElementKind::Active {
                table_ref,
                location,
            });
            let new_segment = function_elements.segments.push(ElementSegmentSpec {
                vs_id: new_space,
                name: format!("export_init_{sp_index}").into(),
            });
            function_elements.items.push(ElementInTable {
                segment_id: new_segment,
                item: *defined_ref,
            })
        }
    }
    fn fill_trampolines(
        &self,
        trampolines: &TrampolinesState,
        module: &mut crate::typed::Module<'_>,
        main_layout: &IndirectFnLayout,
    ) -> Result<()> {
        let TrampolinesState::Created { trampoline_fns } = trampolines else {
            panic!("Trampolines should be create before finish");
        };
        let indirect_fn_segment = module
            .extra
            .get_indirect_fn_segment()
            .expect("Indirect function segment should be created");

        let table_index = match module.extra.function_elements.segments[indirect_fn_segment].kind {
            ElementKind::Active { table_ref, .. } => table_ref,
            _ => panic!("Indirect function segment should be active"),
        };

        // Fill bodies with call_indirect to corresponding index in indirect function table.
        let fill = main_layout.calculate_trampolines(trampoline_fns.clone());
        for trampoline in fill {
            Self::add_trampoline_body(
                module,
                module.functions.stable_id(trampoline.external_ref),
                trampoline.sp_index,
                table_index,
            )?;
        }
        Ok(())
    }
    ///
    /// Fill trampoline body with call_indirect to corresponding split point index.
    ///
    fn add_trampoline_body(
        module: &mut crate::typed::Module<'_>,
        trampoline_ref: FunctionRef,
        sp_index: usize,
        table_index: TableRef,
    ) -> anyhow::Result<()> {
        match module.functions.get_entity_mut(trampoline_ref) {
            ImportOrDefined::Defined(d) => {
                let mut func = wasm_encoder::Function::new(vec![]);
                func.instructions()
                    .i32_const(sp_index as i32)
                    // TODO: resolve type_index
                    .call_indirect(table_index.as_u32(), 0)
                    .end();

                d.body = EntityBody::new_empty(func.into_raw_body().into());
            }
            _ => panic!("Trampoline should be defined"),
        }
        Ok(())
    }
}

impl<'src, F> OutputPlan<'src> for SplitModulePlan<F>
where
    Blacklist<F>: IsSet,
{
    type ExtraData = ExtraExport;

    type Artifacts = SplitModuleResult;

    // 1. For dyn modules - create got entries.
    //
    // 2. create static imports entities
    // 3. create trampolines for dynamic imports
    fn setup(&mut self, module: &mut crate::typed::ModuleBuilder<'src>) -> anyhow::Result<()> {
        for (loc, (entity_ty, file_id, name)) in &self.static_imports {
            log::trace!(
                "Adding static import {name}({entity_ty:?}) from file {file_id:?} for entity {loc:?}"
            );

            macro_rules! push_import {
                ($ty:expr => $($where:ident).+) => {

                    module.$($where).+.push_import(ImportedEntity {
                        entity_type: $ty.clone(),
                        module: file_id.to_string().into(),
                        name: name.clone().into(),
                        renamed_as: None,
                        export_as: ExportNames::default(),
                    }).into()
                };
            }

            let tmp_ref: TempEntityKind = match entity_ty {
                EntityType::Function(ty) => push_import!(ty => functions),
                EntityType::Global(ty) => push_import!(ty => globals),
                EntityType::Memory(ty) => push_import!(ty => memories),
                EntityType::Table(ty) => push_import!(ty => tables),
                EntityType::Tag(ty) => push_import!(ty => tags),
                // TODO: Handle?
                EntityType::DataSymbol(ty) => push_import!(ty => extra.mem_layout),
            };
            self.tmp_refs.push((*loc, tmp_ref));
        }
        match &mut self.is_main {
            GotConverter::Static => {}
            GotConverter::Initted { .. } => {
                panic!("Called setup for already initted module")
            }
            GotConverter::NotInitted {
                blacklist,
                main_layout,
                sp_exports,
            } => {
                let converter = AbsToGot::setup(blacklist.clone(), module)?;

                log::error!("add got to module");
                self.is_main = GotConverter::Initted {
                    converter,
                    main_layout: main_layout.clone(),
                    sp_exports: sp_exports.clone(),
                };
            }
        };

        match self.trampoline_fns {
            TrampolinesState::Declared { ref trampoline_fns } => {
                let created_trampolines = trampoline_fns
                    .iter()
                    .map(|trampoline| {
                        log::trace!(
                            "Creating trampoline for split point import {src_location:?} with extra export {extra:?}",
                            src_location = trampoline.src_location,
                            extra = trampoline.extra
                        );
                        let t = Self::declare_trampoline(trampoline, module);
                        // push ref to preserve original relocation targets.
                        self.tmp_refs.push((trampoline.src_location, TempEntityKind::Function(t.defined_func)));
                        t
                    })
                    .collect();
                self.trampoline_fns = TrampolinesState::Created {
                    trampoline_fns: created_trampolines,
                };
            }
            _ => panic!("Trampolines should be in declare state before setup"),
        }

        Ok(())
    }
    fn entities_to_copy(&self) -> impl Iterator<Item = (FlatEntityRef, Self::ExtraData)> {
        self.entities.iter().cloned()
    }
    fn transform(
        &self,
        source_info: crate::emit::plan::SourceInfo<'_>,
        mut entity: crate::emit::plan::AnyEntity<'src, '_>,
        extra_data: Self::ExtraData,
    ) -> anyhow::Result<Self::Artifacts> {
        let mut split_res = SplitModuleResult::new();

        // Process extra data
        if let ExtraExport::AddExport(name) = &extra_data {
            match &mut entity {
                AnyEntity::Function { entity, .. } => {
                    entity.export_as_mut().add_export(name.clone().into());
                }
                AnyEntity::Global { entity, .. } => {
                    entity.export_as_mut().add_export(name.clone().into());
                }
                AnyEntity::Table { entity, .. } => {
                    entity.export_as_mut().add_export(name.clone().into());
                }
                AnyEntity::Memory { entity, .. } => {
                    entity.export_as_mut().add_export(name.clone().into());
                }
                AnyEntity::Tag { entity, .. } => {
                    entity.export_as_mut().add_export(name.clone().into());
                }
                AnyEntity::DataSymbol { entity, .. } => {
                    entity.export_as_mut().add_export(name.clone().into());
                }
            }
        }

        let GotConverter::Initted {
            main_layout,
            sp_exports,
            converter,
        } = &self.is_main
        else {
            return Ok(split_res);
        };

        // Convert to got
        split_res
            .abs_to_got_res
            .merge(converter.modify_entity(source_info, entity.reborrow())?);

        // find export split points
        let AnyEntity::Function {
            new_ref: fn_ref, ..
        } = entity
        else {
            return Ok(split_res);
        };
        if sp_exports.contains(&source_info.source_flat) {
            split_res.sp_defined_exports.push(Export {
                defined_ref: fn_ref,
                sp_index: main_layout
                    .find_export_fn(source_info.source_flat)
                    .expect("Exported split point should be in main layout"),
            });
        }

        Ok(split_res)
    }
    fn before_lock(
        &mut self,
        module: &mut crate::typed::ModuleBuilder<'src>,
        aggregated_data: &Self::Artifacts,
    ) -> anyhow::Result<()> {
        Self::add_export_init_segment(module, &aggregated_data.sp_defined_exports);
        Ok(())
    }
    fn finish(
        &mut self,
        module: &mut crate::typed::Module<'src>,
        artifacts: Self::Artifacts,
        resolver: &mut crate::emit::relocation::resolver::OutputEntitiesResolver,
    ) -> anyhow::Result<()> {
        for (loc, tmp_ref) in &self.tmp_refs {
            let tmp_ref = tmp_ref.to_stable(module);
            resolver.add_entity_mapping(*loc, tmp_ref);
        }

        match &self.is_main {
            GotConverter::Initted {
                main_layout,
                converter,
                ..
            } => {
                converter.finish(module, resolver, artifacts.abs_to_got_res)?;

                self.fill_trampolines(&self.trampoline_fns, module, main_layout)?;
            }
            GotConverter::Static => {
                return Ok(());
            }
            _ => panic!("Plan is not initted"),
        };

        Ok(())
    }
}

impl SplitProgramInfo {
    #[tracing::instrument(skip_all, name = "Convert split info to emit context")]
    pub fn into_emit_context<'src>(
        &self,
        input_files: &'src FileLoader,
    ) -> Result<EmitContext<'src>> {
        let split_points: Vec<_> = self
            .output_modules
            .iter()
            .flat_map(|(_, info)| info.split_points.clone())
            .collect();
        let snapshot = input_files.get_snapshot();
        let (id, main_info) = &self.output_modules[0];
        assert!(id.is_main(), "First module in split plan should be main");

        let mut ctx = EmitContext::new_plan(
            input_files,
            self.output_modules
                .iter()
                .map(|(id, _)| id.to_string())
                .collect(),
            self.symbol_output_module
                .iter()
                .map(|(sym, module_idx)| (sym, FileId::new(*module_idx)))
                .collect(),
        );

        let mut main_plan = SplitModulePlan::new_main(
            &snapshot,
            input_files,
            main_info,
            &self.symbol_output_module,
            split_points.clone(),
        );

        let mut output = OutputModule::<'src>::from_copy_plan(
            &mut main_plan,
            input_files,
            &snapshot,
            DyLinkInfo::Static,
        )?;

        // extract indirect fn layout from main module.
        let indirect_fn_layout =
            IndirectFnLayout::from_module(&output.module, split_points.clone());

        // Fill sp imports for main module
        main_plan.fill_trampolines(
            &main_plan.trampoline_fns,
            &mut output.module,
            &indirect_fn_layout,
        )?;

        // Now we can add main module to the ctx.
        ctx.output_modules.push(output);

        // emit other modules
        let is_static = |flat| main_info.defined_symbols.contains(&flat);

        for info in &self.output_modules[1..] {
            let mut plan = SplitModulePlan::new_submodule(
                &snapshot,
                input_files,
                info,
                &self.symbol_output_module,
                split_points.clone(),
                Blacklist::new(is_static),
                indirect_fn_layout.clone(),
            );

            // TODO: optimize this.
            let used_modules = info
                .1
                .dependencies
                .keys()
                .map(|id| {
                    self.output_modules
                        .iter()
                        .position(|(module_id, _)| module_id == id)
                        .expect("Dependency module should be in output modules")
                })
                .map(FileId::new)
                .collect();
            let output = OutputModule::<'src>::from_copy_plan(
                &mut plan,
                input_files,
                &snapshot,
                DyLinkInfo::Dynamic { used_modules },
            )?;
            ctx.output_modules.push(output);
        }
        Ok(ctx)
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

impl IndirectFnLayout {
    pub fn from_module(module: &Module, dyn_fns: Vec<SplitPoint>) -> Self {
        let indirect_fn_segment = module
            .extra
            .get_indirect_fn_segment()
            .expect("Indirect function segment should be created");
        let segment = &module.extra.function_elements.segments[indirect_fn_segment];
        let main_indirect_len = segment.parts.len();

        let start = segment
            .kind
            .location()
            .expect("Indirect function segment should be active")
            .offset() as usize;
        Self::new(start + main_indirect_len, dyn_fns)
    }
    pub fn new(main_indirect_len: usize, dyn_fns: Vec<SplitPoint>) -> Self {
        Self {
            start_dyn: main_indirect_len,
            dyn_fns,
        }
    }

    /// Return index of split point import function in indirect function table.
    pub fn find_import_fn(&self, func: FlatEntityRef) -> Option<usize> {
        self.dyn_fns
            .iter()
            .position(|sp| sp.import_func() == func)
            .map(|idx| self.start_dyn + idx)
    }

    /// Return index of split point export function in indirect function table.
    pub fn find_export_fn(&self, func: FlatEntityRef) -> Option<usize> {
        self.dyn_fns
            .iter()
            .position(|sp| sp.export_func() == func)
            .map(|idx| self.start_dyn + idx)
    }

    fn calculate_trampolines(
        &self,
        trampolines: SVec<TrampolineCreated>,
    ) -> SVec<TrampolineCalculated> {
        trampolines
            .into_iter()
            .map(|trampoline| {
                log::trace!(
                    "Calculating trampoline for split point import {src_location:?}",
                    src_location = trampoline.src_ref
                );
                let sp_index = self
                    .find_import_fn(trampoline.src_ref)
                    .expect("Trampoline function should be in indirect layout");
                TrampolineCalculated {
                    external_ref: trampoline.defined_func,
                    sp_index,
                }
            })
            .collect()
    }
}
