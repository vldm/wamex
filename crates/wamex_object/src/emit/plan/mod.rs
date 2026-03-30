//!
//! Output module emitting plans.
//! This module contains a logic to manage the pipeline of
//! transformations of input mudules into output modules.
//!

mod definition;

use anyhow::Result;
use cranelift_entity::{PrimaryMap, packed_option::ReservedValue};
pub use definition::{
    AddressingMode, CopySpec, ImportSpec, NewImportRef, OutputId, OutputModuleCopyPlan,
    PlannedGotInfo,
};
use itertools::Itertools;

use crate::{
    emit::{
        modify::{AnyEntity, EntityModifier, Merge, SourceInfo},
        relocation::{
            EntityLocation, ImportedDataDep, ModuleLayout, RelocationState,
            resolver::OutputEntitiesResolver,
        },
    },
    index::GappedMap,
    linkage::file_db::FileRelocs,
    typed::{
        EntityKind, EntityType, ExportNames, FileId, FileLoader, GlobalRef, ImportedEntity, Module,
        ModuleBuilder, TempEntityKind,
        data::{DataSymbolRef, MemSpec},
        snapshot::{FlatEntityRef, MultiSnapshot},
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

#[derive(Debug)]
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

impl OutputModuleCopyPlan {
    pub fn collect_mem_spec<'src>(input_files: &'src FileLoader) -> MemSpec<'src> {
        // TODO: implement merging of mem spec from multiple modules.
        input_files
            .get_file(FileId::from_u32(0))
            .module
            .mem_spec
            .clone()
    }
    /// Execute the plan and copy entities from input to output modules.
    pub fn copy_entities<'src, M>(
        &self,
        input_files: &'src FileLoader,
        snapshot: &'_ MultiSnapshot,
        entity_modifier_setup: Option<M::SetupData>,
    ) -> Result<OutputModule<'src>>
    where
        M: EntityModifier<'src>,
    {
        let mem_spec = Self::collect_mem_spec(input_files);
        log::info!(
            "Applying plan for output module with {} entities and {} imports",
            self.entities.len(),
            self.imports.len()
        );
        let mut action_span = tracing::info_span!("Copy entities").entered();
        let mut module = ModuleBuilder::new();

        let modifier = entity_modifier_setup
            .map(|shared| M::setup(shared, self, &mut module))
            .transpose()?;

        let mut modifier_artifact = <M::ExtraData as Merge>::new();

        // map of entities from input file to entities in output module.
        let mut file_info = OutputEntitiesResolver::new();

        let mut ref_map: Vec<(EntityLocation, TempEntityKind)> = Vec::new();

        let copy_entities = self
            .entities
            .iter()
            .map(move |(flat, extra)| {
                let EntityLocation { file_id, entity } = snapshot.unpack_ref(*flat);
                let loaded_file = input_files.get_file(file_id);
                (file_id, loaded_file, entity, *flat, extra)
            })
            .chunk_by(|(file_id, _, _, _, _)| *file_id);

        for (file_id, group) in copy_entities.into_iter() {
            let snapshot = snapshot.file_snapshot(file_id);

            for (_, file, entity, flat, copy) in group {
                macro_rules! copy_entity {
                    ($entity_collection:ident $entity_type:ident => $id:expr) => {
                        let mut entity = file.module.$entity_collection.get_entity($id).cloned();
                        let new = module.$entity_collection.dry_push_entity(&entity);
                        if let Some(modifier) = &modifier {

                            let source_info = SourceInfo {
                                input_file: file_id,
                                snapshot,
                                source_entity: flat,
                                entity_relocs: file.relocs.get_entity_relocs($id.into()).unwrap_or_default(),
                            };
                            let any_entity = AnyEntity::$entity_type {
                                new_ref: new,
                                entity: &mut entity,
                            };
                            modifier_artifact.merge(modifier.modify_entity(source_info, any_entity)?);
                        }
                        copy_entity!(@add_export entity.as_mut().export_as_mut());
                        let new = module.$entity_collection.push_entity(entity);
                        ref_map.push((EntityLocation::from_parts(file_id, $id.into()), new.into()));
                    };
                    (@add_export $exports:expr) => {
                        if let Some(new_export) = copy.export_as() {
                            $exports.add_export(new_export.to_string().into());
                        }
                    }
                }
                match entity {
                    EntityKind::Function(f) => {
                        copy_entity!(functions Function => f);
                    }
                    EntityKind::Global(g) => {
                        copy_entity!(globals Global => g);
                    }
                    EntityKind::Memory(m) => {
                        copy_entity!(memories Memory => m);
                    }
                    EntityKind::Table(t) => {
                        copy_entity!(tables Table => t);
                    }
                    EntityKind::Tag(t) => {
                        copy_entity!(tags Tag => t);
                    }
                    EntityKind::DataSymbol(d) => {
                        copy_entity!(data DataSymbol => d);
                    }
                    EntityKind::Type(_) => {} // type is pseudo-entity - and doesn't exist in module.
                }
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
                    if let Some(entity) = import.original_entity {
                        let location = snapshot.unpack_ref(entity);
                        ref_map.push((location, new_ref.into()));
                    }
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
        // after index finalization, we can make some additional transformation
        let mut module = module.into_locked();

        // Convert temp ids to stable
        replace_span!(
            &mut action_span,
            tracing::info_span!("convert_ids_to_stable")
        );
        // Fill mapping for all used entities.
        for (src, entity) in ref_map {
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
        if let Some(modifier) = modifier {
            modifier.finish(&mut module, &file_info, modifier_artifact)?;
        }
        // Now copy and resolve relocs.
        let relocs = module.copy_and_resolve_relocs(&file_info, input_files)?;

        replace_span!(&mut action_span, tracing::info_span!("extend info"));
        // collect indirect table (used by relocs)
        module.extend_indirect_table_from_relocs(&relocs);

        module.mem_spec = mem_spec;
        Ok(OutputModule {
            module,
            resolver: file_info,
            relocs,
            dyn_info,
        })
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
    #[debug("input_files: <FileLoader>")]
    pub input_files: &'a FileLoader,
    pub snapshot: MultiSnapshot,
    // 1. Build copy plan for each module.
    pub output_plans: PrimaryMap<FileId, (OutputId, OutputModuleCopyPlan)>,
    // 1.2. where to search entity if dynamic linking is used
    pub dylinkg_exports_map: GappedMap<FlatEntityRef, FileId>,
    // 2. Build modules from copy plans.
    pub output_modules: PrimaryMap<FileId, OutputModule<'a>>,
    // 3. build writer and layout for each module.
    pub writers: PrimaryMap<FileId, HexDebug>,
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
    pub fn copy_entities<M>(&mut self, entity_modifier_setup: Option<M::SetupData>) -> Result<()>
    where
        M: EntityModifier<'src>,
        M::SetupData: Clone,
    {
        let input_files = self.input_files;
        let snapshot = &self.snapshot;

        let mut outputs = PrimaryMap::new();

        for (_file_id, (_name, plan)) in &self.output_plans {
            let output =
                plan.copy_entities::<M>(input_files, snapshot, entity_modifier_setup.clone())?;
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

            reloc_state.shift_offsets_and_apply_relocs(&mut writer.0, &mut split_module.relocs);
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
            self.output_plans.len() == self.output_modules.len(),
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

            let flat_ref = self.snapshot.pack_ref(src);
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
