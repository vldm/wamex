//!
//! Output module emitting plans.
//! This module contains a logic to manage the pipeline of
//! transformations of input mudules into output modules.
//!

mod definition;

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use cranelift_entity::{EntityRef, PrimaryMap, SecondaryMap, packed_option::ReservedValue};
use definition::EntityType;
pub use definition::{
    AddressingMode, CopyEntity, ImportSpec, NewImportRef, OutputId, OutputModuleCopyPlan,
    PlannedGotInfo,
};
use wasmparser::GlobalType;

use crate::{
    analysis::{OutputModuleInfo, SplitProgramInfo},
    emit::{
        self,
        modify::{self, code_abs_to_got::CodeAbsToGot, data_abs_to_got::DataAbsToGot},
        relocation::{
            EntityLocation, ImportedDataDep, ModuleLayout, RelocationState,
            resolver::OutputEntitiesResolver,
        },
    },
    index::GappedMap,
    linkage::file_db::FileRelocs,
    typed::{
        Building, DefinedEntity, EntityBody, EntityKind, ExportNames, FileId, FileLoader,
        GlobalRef, ImportOrDefined, ImportedEntity, LoadedFile, Module, TempEntityKind,
        data::DataSymbolRef,
        snapshot::{EntitiesSnapshot, FlatEntityRef},
    },
};
#[derive(Debug, Clone)]
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
pub struct DyLinkDeps<Ref: Clone + Default + ReservedValue = GlobalRef> {
    pub our_got: GotInfo<Ref>,
    /// Information about GOT entries with output file id, as defined in `EmitContext::output_plans`.
    pub deps: GappedMap<FileId, GotInfo<Ref>>,
}

impl<Ref: Clone + Default + ReservedValue> ReservedValue for DyLinkDeps<Ref> {
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

pub struct OutputModule<'src> {
    pub module: Module<'src>,
    pub resolver: OutputEntitiesResolver,
    pub dyn_info: Option<DyLinkDeps>,
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

fn unpack_multi_ref(snapshot: &'_ EntitiesSnapshot, entity: FlatEntityRef) -> (FileId, EntityKind) {
    todo!()
}

fn entity_iter<'src, U>(
    input_files: &'src FileLoader,
    snapshot: &'_ EntitiesSnapshot,
    entities: impl IntoIterator<Item = (FlatEntityRef, U)>,
) -> impl Iterator<Item = (FileId, &'src LoadedFile<'src>, EntityKind, U)> {
    entities.into_iter().map(move |(entity, extra)| {
        let (file_id, entity_kind) = unpack_multi_ref(snapshot, entity);
        let loaded_file = input_files.get_file(file_id);
        (file_id, loaded_file, entity_kind, extra)
    })
}

impl OutputModuleCopyPlan {
    /// Execute the plan and copy entities from input to output modules.
    pub fn copy_entities<'src, F>(
        &self,
        input_files: &'src FileLoader,
        snapshot: &'_ EntitiesSnapshot,
        is_static_symbol: F,
    ) -> Result<OutputModule<'src>>
    where
        F: Fn(FlatEntityRef) -> bool + Clone,
    {
        let is_static = matches!(self.addressing, AddressingMode::Static);

        log::info!(
            "Applying plan for output module with {} entities and {} imports",
            self.entities.len(),
            self.imports.len()
        );
        let mut action_span = tracing::info_span!("Copy entities").entered();
        let mut module: Module<'src, Building> = Module::new();

        let code_modifier = (!is_static)
            .then(|| CodeAbsToGot::new(is_static_symbol.clone(), &snapshot, &mut module));
        let data_modifier =
            (!is_static).then(|| DataAbsToGot::new(is_static_symbol, &snapshot, &mut module));

        let mut data_modifier_artifacts = Vec::new();

        // map of entities from input file to entities in output module.
        let mut file_info = OutputEntitiesResolver::new();

        let mut used_queue: Vec<(EntityLocation, TempEntityKind)> = Vec::new();

        // Copy defined symbols to the output module.
        // TODO: Clear exports if not set?
        let copy_entities = entity_iter(
            input_files,
            &snapshot,
            self.entities.iter().map(|(e, c)| (*e, c)),
        );
        for (file_id, file, entity, copy) in copy_entities {
            match entity {
                EntityKind::Function(f) => {
                    let new = match file.module.functions.get_entity(f) {
                        ImportOrDefined::Import(i) => module.functions.push_import(i.clone()),
                        ImportOrDefined::Defined(d) => {
                            let new_body = if let Some(modifier) = &code_modifier {
                                // todo: RemoveReloc target use EntityBody directly
                                let EntityBody::Copied(b) = &d.body else {
                                    panic!(
                                        "Trying to modify already modified function {f} has body {:#?}",
                                        d.body
                                    );
                                };
                                assert!(b.fixups.is_empty());
                                let relocs =
                                    file.relocs.get_entity_relocs(f.into()).unwrap_or_default();
                                let mut modified_body = b.clone();
                                let entity_ref = module.functions.next_defined_key();
                                modify::create_fixup_for_entity(
                                    &mut modified_body,
                                    entity_ref,
                                    file_id,
                                    relocs,
                                    modifier,
                                )?;

                                DefinedEntity {
                                    body: modified_body.into(),
                                    // type/names/exports remain the same.
                                    ..d.clone()
                                }
                            } else {
                                d.clone()
                            };
                            module.functions.push_defined(new_body)
                        }
                    };
                    used_queue.push((EntityLocation::from_parts(file_id, f.into()), new.into()));
                }
                EntityKind::Global(g) => {
                    let global = file.module.globals.get_entity(g);
                    let new = module.globals.push_entity(global.cloned());
                    used_queue.push((EntityLocation::from_parts(file_id, g.into()), new.into()));
                }
                EntityKind::Memory(m) => {
                    let memory = file.module.memories.get_entity(m);
                    let new = module.memories.push_entity(memory.cloned());
                    used_queue.push((EntityLocation::from_parts(file_id, m.into()), new.into()));
                }
                EntityKind::Table(t) => {
                    let table = file.module.tables.get_entity(t);
                    let new = module.tables.push_entity(table.cloned());
                    used_queue.push((EntityLocation::from_parts(file_id, t.into()), new.into()));
                }
                EntityKind::Tag(t) => {
                    let tag = file.module.tags.get_entity(t);
                    let new = module.tags.push_entity(tag.cloned());
                    used_queue.push((EntityLocation::from_parts(file_id, t.into()), new.into()));
                }
                EntityKind::DataSymbol(srcd) => {
                    let new = match file.module.data.get_entity(srcd) {
                        ImportOrDefined::Import(i) => module.data.push_import(i.clone()),
                        ImportOrDefined::Defined(d) => {
                            let new_body = if let Some(modifier) = &data_modifier {
                                let EntityBody::Copied(b) = &d.body else {
                                    panic!(
                                        "Trying to modify already modified data {srcd} has body {:#?}",
                                        d.body
                                    );
                                };
                                assert!(b.fixups.is_empty());
                                let relocs = file
                                    .relocs
                                    .get_entity_relocs(srcd.into())
                                    .unwrap_or_default();
                                let mut modified_body = b.clone();
                                let entity_ref = module.data.next_defined_key();
                                data_modifier_artifacts.extend(modify::create_fixup_for_entity(
                                    &mut modified_body,
                                    entity_ref,
                                    file_id,
                                    relocs,
                                    modifier,
                                )?);

                                DefinedEntity {
                                    body: modified_body.into(),
                                    // type/names/exports remain the same.
                                    ..d.clone()
                                }
                            } else {
                                d.clone()
                            };
                            module.data.push_defined(new_body)
                        }
                    };
                    used_queue.push((EntityLocation::from_parts(file_id, srcd.into()), new.into()));
                }
                EntityKind::Type(_) => {} // type is pseudo-entity - and doesn't exist in module.
            }
        }

        let mut new_imports: GappedMap<_, TempEntityKind> = GappedMap::new();
        // Add imports for used symbols (even if they are defined in source).
        for (r, import) in &self.imports {
            macro_rules! push_import {
                ($entity_type:ident => $ty: expr) => {{
                    let new_ref = module.$entity_type.push_import(ImportedEntity {
                        module: import.module.clone().into(),
                        name: import.name.clone().into(),
                        entity_type: $ty.clone(),
                        export_as: ExportNames::default(),
                        renamed_as: None,
                    });
                    new_imports.insert(r, new_ref.into());
                }};
            }
            match &import.ty {
                EntityType::Function(f) => {
                    push_import!(functions => f);
                }
                // stack_pointer, mb heap_base/__data_end, etc.
                EntityType::Global(g) => {
                    push_import!(globals => g);
                }
                // indirect_function_table
                EntityType::Table(t) => {
                    push_import!(tables => t);
                }
                // only one memory
                EntityType::Memory(m) => {
                    push_import!(memories => m);
                }
                // Future support
                EntityType::Tag(m) => {
                    push_import!(tags => m);
                }
                EntityType::DataSymbol(d) => {
                    push_import!(data => d);
                }
            }
        }

        replace_span!(&mut action_span, tracing::info_span!("lock_module"));
        log::warn!("module after copying entities: {:#?}", module);
        // after index finalization, we can make some additional transformation
        let mut module = module.into_locked();

        // Convert temp ids to stable
        replace_span!(
            &mut action_span,
            tracing::info_span!("convert_ids_to_stable")
        );
        // Fill mapping for all used entities.
        for (src, entity) in used_queue {
            file_info.add_entity_mapping(src, entity.to_stable(&module));
        }

        let dyn_info = match &self.addressing {
            AddressingMode::Static => None,
            AddressingMode::GotRelative(g) => {
                macro_rules! conv {
                    ($got:expr) => {
                        GotInfo {
                            memory_base: conv!(@ref $got.memory_base),
                            table_base: conv!(@ref $got.table_base),
                        }
                    };
                    (@ref $e:expr) => {
                        match new_imports.get($e)
                            .expect("Got entry not found in imports")
                            .to_stable(&module) {
                            EntityKind::Global(g) => g,
                            _ => panic!("Got entry should be global"),
                        }
                    };
                }
                let deps = g
                    .deps
                    .iter()
                    .map(|(file, got)| {
                        let got = conv!(got);
                        (file, got)
                    })
                    .collect();

                Some(DyLinkDeps {
                    our_got: conv!(g.our_got),
                    deps,
                })
            }
        };
        replace_span!(&mut action_span, tracing::info_span!("resolve_relocs"));
        // resolve got entries in code modifier, and fill start function body
        // do it before copy_and_resolve_relocs to ensure that all relocs are copied into file_relocs.
        let data_modifier_artifacts_mapped = DataAbsToGot::convert_to_stable_refs_and_resolve(
            &mut module,
            &file_info,
            data_modifier_artifacts,
        );

        if let Some(modifier) = data_modifier {
            modifier.fill_start_fn(&mut module, data_modifier_artifacts_mapped)?;
        }
        // Now copy and resolve relocs.
        let relocs = module.copy_and_resolve_relocs(&file_info, input_files)?;

        replace_span!(&mut action_span, tracing::info_span!("extend info"));
        // collect indirect table (used by relocs)
        module.extend_indirect_table_from_relocs(&relocs);

        Ok(OutputModule {
            module,
            resolver: file_info,
            relocs,
            dyn_info,
        })
    }
}

///
/// The top-level plan for an entire emit job.
/// It containts basic information about all entities that need to be copied into output modules.
///
pub struct EmitContext<'a> {
    pub input_files: &'a FileLoader,
    pub snapshot: EntitiesSnapshot,
    // 1. Build copy plan for each module.
    pub output_plans: PrimaryMap<FileId, (OutputId, OutputModuleCopyPlan)>,
    // 1. where to search entity if dynamic linking is used
    pub dylinkg_exports_map: GappedMap<FlatEntityRef, FileId>,
    // 2. Build modules from copy plans.
    pub output_modules: PrimaryMap<FileId, OutputModule<'a>>,
    // 3. build writer and layout for each module.
    pub writers: PrimaryMap<FileId, Vec<u8>>,
    pub layouts: PrimaryMap<FileId, ModuleLayout>,
}
impl<'src> EmitContext<'src> {
    pub fn new_plan(
        input_files: &'src FileLoader,
        output_plans: PrimaryMap<FileId, (OutputId, OutputModuleCopyPlan)>,
        exported_symbols: GappedMap<FlatEntityRef, FileId>,
    ) -> Self {
        Self {
            input_files,
            snapshot: input_files.get_snapshot(),
            output_plans,
            dylinkg_exports_map: exported_symbols,
            output_modules: PrimaryMap::new(),
            writers: PrimaryMap::new(),
            layouts: PrimaryMap::new(),
        }
    }
    #[tracing::instrument(skip_all, name = "Copy entities")]
    pub fn copy_entities<F>(&mut self, is_static: F) -> Result<()>
    where
        F: Fn(FlatEntityRef) -> bool + Clone,
    {
        let input_files = self.input_files;
        let snapshot = &self.snapshot;

        let mut outputs = PrimaryMap::new();

        for (_file_id, (_name, plan)) in &self.output_plans {
            let output = plan.copy_entities(input_files, &snapshot, is_static.clone())?;
            outputs.push(output);
        }
        self.output_modules = outputs;
        Ok(())
    }

    #[tracing::instrument(skip_all, name = "Build module layouts")]
    pub fn build_layouts(&mut self) -> Result<()> {
        let mut writers = PrimaryMap::new();
        let mut layouts = PrimaryMap::new();
        for (file, output) in &self.output_modules {
            log::info!(
                "Generating module {ident}",
                ident = self.output_plans[file].0
            );
            let mut writer = wasm_encoder::Module::new();
            let layout = output.module.generate(&mut writer)?;
            let writer = writer.finish();
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
            let (ident, ..) = &self.output_plans[file];
            let output_module = &self.output_modules[file];

            let imported_data = self.collect_dylink_data_deps(ident, output_module);

            let split_module = &mut self.output_modules[file];
            let writer = &mut self.writers[file];

            // 3. building relocation state and applying relocs
            log::debug!("Applying relocation state for module {ident}");
            // reborrow as mutable
            let reloc_state = RelocationState::new(
                &split_module.module,
                &self.layouts[file],
                split_module.dyn_info.as_ref().map(|d| d.our_got.clone()),
                imported_data,
                &self.layouts,
            );

            reloc_state.shift_offsets_and_apply_relocs(writer, &mut split_module.relocs);
        }
        Ok(())
    }

    #[tracing::instrument(skip_all, name = "Write modules")]
    fn write_modules(
        &self,
        mut emit_fn: impl FnMut(&OutputId, &[u8]) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        for (file, (ident, ..)) in &self.output_plans {
            log::info!("Emitting module {ident}");
            let bytes = &self.writers[file];
            // debug
            {
                log::error!("Writing module {ident}_fxd to file for debug");
                let output_path =
                    AsRef::<std::path::Path>::as_ref("/tmp").join(format!("{}_fxd.wasm", ident));
                std::fs::write(&output_path, bytes).unwrap();
            }
            emit_fn(ident, bytes)?;
        }
        Ok(())
    }
    ///
    ///  Emit output modules, from split program info.
    ///
    #[tracing::instrument(skip_all)]
    pub fn emit_modules(
        input_files: &FileLoader,
        program_info: &SplitProgramInfo,
        emit_fn: impl FnMut(&OutputId, &[u8]) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        // 0. <split related logic> convert to ctx + get deps of main module
        let mut emit_ctx = program_info.into_emit_context(input_files);
        let main_deps = &program_info.output_modules[0].1.defined_symbols;
        let is_static = |e| main_deps.contains(&e);

        // 1. build modules
        emit_ctx.copy_entities(is_static)?;

        // 2. Build layouts
        emit_ctx.build_layouts()?;

        // 3. apply relocs
        emit_ctx.apply_relocs()?;
        // 4. TODO: append linker (relocs,symtable) sections.
        // 5. write file using callback.
        emit_ctx.write_modules(emit_fn)?;
        Ok(())
    }

    fn collect_dylink_data_deps(
        &self,
        ident: &OutputId,
        output_module: &OutputModule,
    ) -> GappedMap<DataSymbolRef, ImportedDataDep> {
        let mut imported_data = GappedMap::new();
        // 2. calculate memoffsets for imported data symbols.
        log::debug!("Calculating imported data offsets for module {ident}");
        for (orig_d, _i) in output_module.module.data.imports_iter() {
            let src = output_module
                .resolver
                .get_entity_src(orig_d.into())
                .expect("imported data symbol must have source entity");

            let flat_ref = self.snapshot.pack_ref(src.entity);
            // find output module that defines this symbol, and get symbol offset in output module.
            let dep_file = *self
                .dylinkg_exports_map
                .get(flat_ref)
                .expect("imported symbol should be exported by some module");

            let (dep_id, ..) = &self.output_plans[dep_file];
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
                "Importing data symbol {orig_d} from module {dep_id} with offset {extern_ref:?}"
            );
            imported_data.insert(
                orig_d,
                ImportedDataDep {
                    output_location: *extern_ref,
                    got_entry: Some(output_module.dyn_info.as_ref()
                        .expect("Dynamic info should be present for module with imported data symbols")
                        .deps
                        .get(dep_file)
                        .map(|entry| entry.memory_base)
                        .expect("Memory base should be present for imported data symbols")
                    ),
                },
            );
        }
        imported_data
    }
}
