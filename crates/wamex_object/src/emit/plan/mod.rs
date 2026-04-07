//!
//! Output module emitting plans.
//! This module contains a logic to manage the pipeline of
//! transformations of input mudules into output modules.
//!

mod definition;

use anyhow::Result;
use cranelift_entity::{PrimaryMap, packed_option::ReservedValue};
pub use definition::OutputId;
use itertools::Itertools;

pub use self::definition::{AnyEntity, Merge, OutputPlan, SourceInfo};
use crate::{
    emit,
    emit::relocation::{
        EntityLocation, ImportedDataDep, ModuleLayout, RelocationState,
        resolver::OutputEntitiesResolver,
    },
    index::{GappedMap, Temp},
    layouts::DataSymbolRef,
    linkage::file_db::FileRelocs,
    typed::{
        EntityKind, EntityType, ExportNames, FileId, FileLoader, FunctionRef, GlobalRef,
        ImportedEntity, Module, ModuleBuilder, TempEntityKind,
        snapshot::{FlatEntityRef, MultiSnapshot},
    },
};
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct GotInfo<T = GlobalRef> {
    pub memory_base: T,
    pub table_base: T,
}

impl<Ref> ReservedValue for GotInfo<Ref>
where
    Ref: ReservedValue,
{
    fn reserved_value() -> Self {
        Self {
            memory_base: Ref::reserved_value(),
            table_base: Ref::reserved_value(),
        }
    }
    fn is_reserved_value(&self) -> bool {
        self.memory_base.is_reserved_value() && self.table_base.is_reserved_value()
    }
}

///
/// Extra information needed for perfom dynamic linking.
///
#[derive(Debug, Clone)]
pub struct DyLinkGots<Ref: Clone + Default + ReservedValue = GlobalRef> {
    pub our_got: GotInfo<Ref>,
    /// Information about GOT entries with output file id, as defined in `EmitContext::output_plans`.
    pub deps: GappedMap<FileId, GotInfo<Ref>>,
}

impl<Ref: Clone + Default + ReservedValue> ReservedValue for DyLinkGots<Ref> {
    fn reserved_value() -> Self {
        Self {
            our_got: GotInfo::reserved_value(),
            deps: GappedMap::new(),
        }
    }
    fn is_reserved_value(&self) -> bool {
        self.our_got.is_reserved_value() && self.deps.is_empty()
    }
}

pub enum DyLinkInfo {
    Static,
    Dynamic { used_modules: Vec<FileId> },
}

#[derive(Debug)]
pub struct OutputModule<'src> {
    pub module: Module<'src>,
    pub resolver: OutputEntitiesResolver,
    pub dyn_info: Option<DyLinkGots>,
    pub relocs: FileRelocs,
}

// extension to EnteredSpan to allow replacing it corerctly.
// If we use naive:
// `action_span = tracing::info_span!("old_span").entered();`
// or `let _= mem::replace(&mut action_span, tracing::info_span!("new_span").entered());`
// then old span is closed after entering into new span,
// and therefore new_span will have old_span as parent.
//
//
// Implementing this as extension trait is also imposible, because new_span expression should be evaluated after
// old_span is closed.
//
#[macro_export]
macro_rules! replace_span {
    ($action_span:expr, $new_span:expr) => {{
        // first cleanup close self.
        drop(std::mem::replace(
            $action_span,
            tracing::Span::none().entered(),
        ));
        // then enter and replace.
        drop(std::mem::replace($action_span, $new_span.entered()));
    }};
}

impl<'src> OutputModule<'src> {
    /// Copy virtual space and segments from input module to output module
    pub fn copy_vs_segments(output: &mut ModuleBuilder<'src>, input_files: &'src FileLoader) {
        // TODO: merge info from all modules
        let first = input_files.get_file(FileId::from_u32(0));
        // let temp_mem =
        //     output.memories.push_import(MemoryType {
        //         minimum: 0,
        //         memory64: false,
        //         shared: false,
        //         page_size_log2: None,
        //         maximum: None,
        //     }.into());

        log::error!("Need to create memory");
        let module = &first.module;
        let output_layout = module
            .extra
            .mem_layout
            .recover_vs_segments(|_| Temp::from_defined(0));
        output.extra.mem_layout = output_layout;
    }
    /// Execute the plan and copy entities from input to output modules.
    pub fn from_copy_plan<M>(
        plan: &mut M,
        input_files: &'src FileLoader,
        snapshot: &'_ MultiSnapshot,
        dylink: DyLinkInfo,
    ) -> Result<OutputModule<'src>>
    where
        M: OutputPlan<'src>,
    {
        let mut action_span = tracing::info_span!("Setup plan").entered();
        let mut module = ModuleBuilder::new();

        Self::copy_vs_segments(&mut module, input_files);

        let mut tmp_dylink = None;
        if let DyLinkInfo::Dynamic { used_modules } = dylink {
            let memory_base = module
                .globals
                .push_import(crate::typed::ImportedGlobal::memory_base());
            let table_base = module
                .globals
                .push_import(crate::typed::ImportedGlobal::table_base());
            module.extra.got_info = Some(GotInfo {
                memory_base,
                table_base,
            });
            tmp_dylink = Some(DyLinkGots {
                our_got: GotInfo {
                    memory_base,
                    table_base,
                },
                // add global import for each dep
                deps: used_modules
                    .iter()
                    .map(|file| {
                        (
                            *file,
                            GotInfo {
                                memory_base: module.globals.push_import(
                                    crate::typed::ImportedGlobal::memory_base()
                                        .with_module(file.to_string().into()),
                                ),
                                table_base: module.globals.push_import(
                                    crate::typed::ImportedGlobal::table_base()
                                        .with_module(file.to_string().into()),
                                ),
                            },
                        )
                    })
                    .collect(),
            });
        }

        plan.setup(&mut module)?;

        let mut modifier_artifact = <M::Artifacts as Merge>::new();

        let mut ref_map: Vec<(EntityLocation, TempEntityKind)> = Vec::new();

        replace_span!(&mut action_span, tracing::info_span!("Copy entities"));
        {
            let copy_entities = plan
                .entities_to_copy()
                .map(move |(flat, extra)| {
                    let EntityLocation { file_id, entity } = snapshot.unpack_ref(flat);
                    let loaded_file = input_files.get_file(file_id);
                    (file_id, flat, loaded_file, entity, extra)
                })
                .chunk_by(|(file_id, _, _, _, _)| *file_id);

            for (file_id, group) in copy_entities.into_iter() {
                let snapshot = snapshot.file_snapshot(file_id);

                for (_, flat, file, entity_ref, extra) in group {
                    macro_rules! copy_entity {
                    ($($entity_collection:ident).+, $entity_type:ident => $id:expr) => {
                        let mut entity = file.module.$($entity_collection).+.get_entity($id).cloned();
                        assert!(!entity.is_external(), "Copying external entities looks like a bug");
                        let new = module.$($entity_collection).+.dry_push_entity(&entity);
                        let source_info = SourceInfo {
                            input_file: file_id,
                            snapshot,
                            source_entity: entity_ref,
                            source_flat: flat,
                            entity_relocs: file.relocs.get_entity_relocs($id.into()).unwrap_or_default(),
                        };
                        let any_entity = AnyEntity::$entity_type {
                            new_ref: new,
                            entity: entity.as_mut(),
                        };
                        modifier_artifact.merge(plan.transform(source_info, any_entity, extra)?);

                        let new = module.$($entity_collection).+.push_entity(entity);
                        ref_map.push((EntityLocation::from_parts(file_id, $id.into()), new.into()));
                    };
                }
                    match entity_ref {
                        EntityKind::Function(f) => {
                            copy_entity!(functions, Function => f);
                        }
                        EntityKind::Global(g) => {
                            copy_entity!(globals, Global => g);
                        }
                        EntityKind::Memory(m) => {
                            copy_entity!(memories, Memory => m);
                        }
                        EntityKind::Table(t) => {
                            copy_entity!(tables, Table => t);
                        }
                        EntityKind::Tag(t) => {
                            copy_entity!(tags, Tag => t);
                        }
                        EntityKind::DataSymbol(d) => {
                            copy_entity!(extra.mem_layout, DataSymbol => d);
                        }
                        EntityKind::Type(_) => {} // type is pseudo-entity - and doesn't exist in module.
                    }
                }
            }
        }

        replace_span!(&mut action_span, tracing::info_span!("lock_module"));
        // Try to add indirect table after copying (if it wasn't imported).
        module.create_empty_indirect_fn_table();
        plan.before_lock(&mut module, &modifier_artifact)?;
        // after index finalization, we can make some additional transformation
        let mut module = module.into_locked();

        // Convert temp ids to stable
        replace_span!(
            &mut action_span,
            tracing::info_span!("convert_ids_to_stable")
        );

        // map of entities from input file to entities in output module.
        let mut file_info = OutputEntitiesResolver::new();
        // Fill mapping for all used entities.
        for (src, entity) in ref_map {
            file_info.add_entity_mapping(src, entity.to_stable(&module));
        }

        let dyn_info = tmp_dylink.map(|got_info| {
            macro_rules! conv {
                    ($got:expr) => {
                        GotInfo {
                            memory_base: conv!(@ref $got.memory_base),
                            table_base: conv!(@ref $got.table_base),
                        }
                    };
                    (@ref $e:expr) => {
                        module.globals.stable_id($e)
                    };
                }
            let deps = got_info
                .deps
                .iter()
                .map(|(file, got)| {
                    let got = conv!(got);
                    (file, got)
                })
                .collect();

            DyLinkGots {
                our_got: conv!(got_info.our_got),
                deps,
            }
        });
        log::warn!("Dep info for module: {:#?}", dyn_info);
        replace_span!(&mut action_span, tracing::info_span!("finish_modifier"));
        // resolve got entries in code modifier, and fill start function body
        // do it before copy_and_resolve_relocs to ensure that all relocs are copied into file_relocs.

        plan.finish(&mut module, modifier_artifact, &mut file_info)?;

        replace_span!(&mut action_span, tracing::info_span!("resolve_relocs"));
        // Now copy and resolve relocs.
        let relocs = module.copy_and_resolve_relocs(&file_info, input_files)?;

        replace_span!(&mut action_span, tracing::info_span!("extend info"));
        // collect indirect table (used by relocs)
        Self::add_indirect_fns_to_segment(&mut module, relocs.list_indirect_fns())?;

        Ok(OutputModule {
            module,
            resolver: file_info,
            relocs,
            dyn_info,
        })
    }

    fn add_indirect_fns_to_segment(
        module: &mut Module,
        indirect_fns: Vec<FunctionRef>,
    ) -> Result<()> {
        let segment_id = module
            .extra
            .get_indirect_fn_segment()
            .expect("Indirect function segment should be created.");
        let segment = &mut module.extra.function_elements.segments[segment_id];

        segment
            .parts
            .extend(indirect_fns.into_iter().map(|f| f.into()));
        Ok(())
    }
}

#[derive(derive_more::Debug)]
#[debug("{}", hex::encode(_0))]
pub struct HexDebug(pub Vec<u8>);
///
/// The top-level plan for an entire emit job.
/// It containts basic information about all entities that need to be copied into output modules.
///
#[derive(derive_more::Debug)]
pub struct EmitContext<'a> {
    // Static arguments
    #[debug("input_files: <FileLoader>")]
    pub input_files: &'a FileLoader,
    pub snapshot: MultiSnapshot,
    // module information
    pub output_names: PrimaryMap<FileId, OutputId>,
    // 1. where to search entity if dynamic linking is used
    pub dylinkg_exports_map: GappedMap<FlatEntityRef, FileId>,
    // 2. Build modules from copy plans.
    pub output_modules: PrimaryMap<FileId, OutputModule<'a>>,

    // The result of `emit_modules` pipeline.
    pub writers: PrimaryMap<FileId, HexDebug>,
    pub layouts: PrimaryMap<FileId, ModuleLayout>,
}
impl<'src> EmitContext<'src> {
    pub fn new_plan(
        input_files: &'src FileLoader,
        output_names: PrimaryMap<FileId, OutputId>,
        exported_symbols: GappedMap<FlatEntityRef, FileId>,
    ) -> Self {
        Self {
            input_files,
            snapshot: input_files.get_snapshot(),
            output_names,
            dylinkg_exports_map: exported_symbols,
            output_modules: PrimaryMap::new(),
            writers: PrimaryMap::new(),
            layouts: PrimaryMap::new(),
        }
    }
    // #[tracing::instrument(skip_all, name = "Copy entities")]
    // pub fn copy_entities<M>(&mut self, entity_modifier_setup: M::SetupData) -> Result<()>
    // where
    //     M: EntityModifier<'src>,
    //     M::SetupData: Clone,
    // {
    //     let input_files = self.input_files;
    //     let snapshot = &self.snapshot;

    //     let mut outputs = PrimaryMap::new();

    //     for (_file_id, (_name, plan)) in &self.output_plans {
    //         let output =
    //             plan.copy_entities::<M>(input_files, snapshot, entity_modifier_setup.clone())?;
    //         outputs.push(output);
    //     }
    //     self.output_modules = outputs;
    //     Ok(())
    // }

    #[tracing::instrument(skip_all, name = "Build module layouts")]
    pub fn build_layouts(&mut self) -> Result<()> {
        let mut writers = PrimaryMap::new();
        let mut layouts = PrimaryMap::new();
        for (file, output) in &self.output_modules {
            log::info!("Generating module {ident}", ident = self.output_names[file]);
            let mut writer = wasm_encoder::Module::new();
            let layout = output.module.generate(&mut writer)?;
            let writer = HexDebug(writer.finish());
            writers.push(writer);
            layouts.push(layout);
        }
        self.writers = writers;
        self.layouts = layouts;
        Ok(())
    }

    #[tracing::instrument(skip_all, name = "Apply relocs")]
    pub fn apply_relocs(&mut self) -> Result<()> {
        // 3. apply relocs
        log::info!("Applying relocs for modules");

        for file in self.output_modules.keys() {
            let ident = &self.output_names[file];
            let output_module = &self.output_modules[file];

            let imported_data = self.collect_dylink_data_deps(ident, output_module);

            let output_module_info = &mut self.output_modules[file];
            let writer = &mut self.writers[file];

            // 3. building relocation state and applying relocs
            log::debug!("Applying relocation state for module {ident}");
            // reborrow as mutable
            let reloc_state = RelocationState::new(
                &output_module_info.module,
                &self.layouts[file],
                output_module_info
                    .dyn_info
                    .as_ref()
                    .map(|d| d.our_got.clone()),
                imported_data,
                &self.layouts,
            );

            reloc_state
                .shift_offsets_and_apply_relocs(&mut writer.0, &mut output_module_info.relocs);
        }
        Ok(())
    }

    #[tracing::instrument(skip_all, name = "Write modules")]
    fn write_modules(
        &self,
        mut emit_fn: impl FnMut(&OutputId, &[u8]) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        for (file, ident) in &self.output_names {
            log::info!("Emitting module {ident}");
            let bytes = &self.writers[file];
            // debug
            {
                log::error!("Writing module {ident}_fxd to file for debug");
                let output_path =
                    AsRef::<std::path::Path>::as_ref("/tmp").join(format!("{}_fxd.wasm", ident));
                std::fs::write(&output_path, &bytes.0).unwrap();
            }
            emit_fn(ident, &bytes.0)?;
        }
        Ok(())
    }
    ///
    /// Emit output modules, from split program info.
    ///
    #[tracing::instrument(skip_all)]
    pub fn emit_modules(
        &mut self,
        emit_fn: impl FnMut(&OutputId, &[u8]) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        assert!(
            self.output_names.len() == self.output_modules.len(),
            "Output plans should be generated for all output modules before emitting"
        );

        // 2. Build layouts
        self.build_layouts()?;

        // 3. apply relocs
        self.apply_relocs()?;
        // 4. TODO: append linker (relocs,symtable) sections.
        // 5. write file using callback.
        self.write_modules(emit_fn)?;
        Ok(())
    }

    // TODO: for static modules - there might be no got.
    fn collect_dylink_data_deps(
        &self,
        ident: &OutputId,
        output_module: &OutputModule,
    ) -> GappedMap<DataSymbolRef, ImportedDataDep> {
        let mut imported_data = GappedMap::new();
        // 2. calculate memoffsets for imported data symbols.
        log::debug!("Calculating imported data offsets for module {ident}");
        for (orig_d, _i) in output_module.module.extra.mem_layout.external().iter() {
            let src = output_module
                .resolver
                .get_entity_src(orig_d.into())
                .expect("imported data symbol must have source entity");

            let flat_ref = self.snapshot.pack_ref(src);
            // find output module that defines this symbol, and get symbol offset in output module.
            let dep_file = *self
                .dylinkg_exports_map
                .get(flat_ref)
                .expect("imported symbol should be exported by some module");

            let dep_id = &self.output_names[dep_file];
            let dep_split_module = &self.output_modules[dep_file];
            let dep_layout = &self.layouts[dep_file];

            let sym_ref = dep_split_module
                .resolver
                .get_output_entity(&src)
                .expect("source entity must have output entity");
            let extern_ref = match sym_ref {
                EntityKind::DataSymbol(d) => dep_layout.data_mapping.get(d).unwrap(),
                _ => panic!(
                    "Only data symbols can be imported, but got import with source entity {sym_ref}"
                ),
            };

            log::debug!(
                "Importing data symbol {orig_d} from module {dep_id} ({dep_file}) with offset {extern_ref:?}"
            );
            let imported_dep = ImportedDataDep {
                output_location: extern_ref.offsets,
                got_entry: output_module
                    .dyn_info
                    .as_ref()
                    .expect("Dynamic info should be present for module with imported data symbols")
                    .deps
                    .get(dep_file)
                    .map(|entry| entry.memory_base),
            };

            // Only main module can be imported as static.
            if imported_dep.got_entry.is_some() && dep_file.as_u32() != 0 {
                log::error!("Using module {dep_id} data symbol, but no imported got entry found.");
            }
            imported_data.insert(orig_d, imported_dep);
        }
        imported_data
    }
}
