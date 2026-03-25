use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io::{self, Write},
    path::Path,
};

use anyhow::Result;
use cranelift_entity::{EntityRef, PrimaryMap, SecondaryMap};
use wasm_encoder::{Encode, FunctionSection};
use wasmparser::{FuncType, GlobalType};

use crate::{
    analysis::{OutputModuleInfo, SplitModuleIdentifier, SplitProgramInfo},
    emit::{
        memory_layout::{DataSymbolsOffsets, SegmentLayout},
        modify::{
            OutputEntityRef, code_abs_to_got::CodeAbsToGot, data_abs_to_got::DataAbsToGot,
            wasm_emitter,
        },
        plan::{EmitContext, OutputId, OutputModule},
        relocation::{
            EntityLocation, FunctionInfo, ImportedDataDep, ModuleLayout, RelocationState,
            resolver::OutputEntitiesResolver,
        },
    },
    helpers::{ShiftMap, ShiftPoint},
    index::{GappedMap, TempIndex},
    linkage::{file_db::FileRelocs, reloc::EntityRelocationEntry},
    raw::FuncTypeId,
    typed::{
        Building, DefinedEntity, DefinedFunction, EntityBody, EntityBodyCopy, EntityKind,
        ExportNames, FileId, FileLoader, FunctionRef, ImportOrDefined, ImportedEntity, Module,
        TableRef, TempEntityKind,
        data::DataSymbolRef,
        elements::ElementItemId,
        snapshot::FlatEntityRef,
    },
};

pub mod memory_layout;
pub mod modify;
#[macro_use]
pub mod plan;
pub mod relocation;

impl<'src> Module<'src> {
    #[tracing::instrument(skip_all)]
    pub fn generate(&self, output_module: &mut wasm_encoder::Module) -> Result<ModuleLayout> {
        // self.generate_dylink0_section(output_module)?;

        let fn_type_map = self.generate_type_section(output_module);

        self.generate_import_section(&fn_type_map, output_module);
        self.generate_function_type_ids_section(&fn_type_map, output_module);
        self.generate_table_sections(output_module);
        self.generate_memory_section(output_module);

        self.generate_tag_section(output_module);
        self.generate_global_section(output_module)?;
        self.generate_export_section(output_module);
        self.generate_start_function_section(output_module);

        let indirect_fn_mapping = self.generate_element_section(output_module)?;
        self.generate_data_count_section(output_module);
        let code_start = output_module.len() + 1 + 5; // +1 for code section id + 5 for bytes len, we need to know offset of code section for code relocs.
        let mut functions_mapping = self.generate_code_section(output_module)?;
        let code_range = code_start..output_module.len();

        // Copy indexes of elements ids in indirect function table.
        for (func_ref, elem_id) in indirect_fn_mapping.iter() {
            functions_mapping[func_ref].indirect_table_index = Some(*elem_id);
        }

        let data_start = output_module.len() + 1 + 5; // +1 for data section id + 5 for bytes len, we need to know offset of data section for data relocs.
        let data_mapping = self.generate_data_section(output_module)?;
        let data_range = data_start..output_module.len();

        // // self.generate_wasm_bindgen_sections(output_module);
        // // Names + Linking + Relocations
        // self.generate_compiler_tools_sections(output_module, code_relocs, data_relocs)?;
        // self.generate_target_features_section(output_module)?;
        // self.generate_custom_sections(output_module)?;
        // todo!()
        Ok(ModuleLayout {
            code_section: code_range,
            data_section: data_range,
            functions_mapping,
            data_mapping,
        })
    }

    /// Generate type section, return map from FunctionId to type index.
    pub fn generate_type_section(
        &self,
        output_module: &mut wasm_encoder::Module,
    ) -> SecondaryMap<FunctionRef, FuncTypeId> {
        let mut function_types = SecondaryMap::new();
        let mut uniq_types = HashMap::<&wasmparser::FuncType, FuncTypeId>::new();

        let func_types = self
            .functions
            .iter()
            .map(|(id, func)| (id, func.get_type()));

        // Collect unique types
        for (id, func_type) in func_types {
            let new_func_type_id = FuncTypeId::new(uniq_types.len());

            let func_type_id = uniq_types.entry(func_type).or_insert(new_func_type_id);
            function_types[id] = *func_type_id;
        }

        // build section based on collected types
        let mut section = wasm_encoder::TypeSection::new();

        let mut uniq_types: Vec<(_, _)> = uniq_types
            .into_iter()
            .map(|(k, v)| (k.clone(), v))
            .collect();
        uniq_types.sort_by_key(|v| v.1);

        for (func_type, _id) in uniq_types {
            let output_func_type: wasm_encoder::FuncType = func_type.clone().try_into().unwrap();
            section.ty().func_type(&output_func_type);
        }

        output_module.section(&section);
        // return map used to generate code section
        function_types
    }
    pub fn generate_import_section(
        &self,
        fn_type_map: &SecondaryMap<FunctionRef, FuncTypeId>,
        output_module: &mut wasm_encoder::Module,
    ) {
        let mut section = wasm_encoder::ImportSection::new();
        macro_rules! for_entity {
            // $construct:ident - constructor in wasmparser::TypeRef
            // $kind:ident - kind of entity in BuilderState (functions, globals, etc)
            // $convert:expr - optional conversion from BuilderState entity type to wasmparser type FuncType -> FuncTypeId (u32)
            ($construct:ident ($kind:ident $(=> $($convert:tt)+)?) ) => {
                for (import_ref, import) in self.$kind.imports_iter() {
                    let val = for_entity!(@build_val import_ref, import.entity_type $(=> $($convert)+)?);
                    let entity: wasm_encoder::EntityType =
                        wasmparser::TypeRef::$construct(val).try_into().unwrap();
                    section.import(&import.module, &import.name, entity);
                }
            };
            (@build_val $ref:expr, $v:expr => $($convert:tt)+) => {$($convert)*( $ref, &$v)};
            (@build_val $ref:expr, $v:expr) => {{let _ = $ref; $v}};
        }

        let func_convert =
            |fn_ref: FunctionRef, _func_type: &FuncType| -> u32 { fn_type_map[fn_ref].as_u32() };

        for_entity!(Global(globals));
        for_entity!(Func(functions => func_convert));
        for_entity!(Table(tables));
        for_entity!(Memory(memories));
        for_entity!(Tag(tags));
        // for_entity!(Func(functions));
        output_module.section(&section);
    }

    fn generate_export_section(&self, output_module: &mut wasm_encoder::Module) {
        let mut section = wasm_encoder::ExportSection::new();

        macro_rules! for_entity {
            // $construct:ident - constructor in wasmparser::TypeRef
            // $kind:ident - kind of entity in BuilderState (functions, globals, etc)
            ($construct:ident ($kind:ident )) => {
                for (entity_ref, export_name) in self.$kind.exports_iter() {
                    section.export(
                        &export_name,
                        wasm_encoder::ExportKind::$construct,
                        entity_ref.as_u32(),
                    );
                }
            };
        }
        for_entity!(Global(globals));
        for_entity!(Func(functions));
        for_entity!(Table(tables));
        for_entity!(Memory(memories));
        for_entity!(Tag(tags));
        output_module.section(&section);
    }

    fn generate_function_type_ids_section(
        &self,
        fn_type_map: &SecondaryMap<FunctionRef, FuncTypeId>,
        output_module: &mut wasm_encoder::Module,
    ) {
        let mut section = FunctionSection::new();
        for (fn_ref, _) in self.functions.defined_iter() {
            let type_id = fn_type_map[fn_ref];
            section.function(type_id.as_u32());
        }
        output_module.section(&section);
    }

    fn generate_table_sections(&self, output_module: &mut wasm_encoder::Module) {
        let mut section = wasm_encoder::TableSection::new();
        for (_table_ref, table) in self.tables.defined_iter() {
            section.table(table.entity_type.try_into().unwrap());
        }
        output_module.section(&section);
    }

    fn generate_memory_section(&self, output_module: &mut wasm_encoder::Module) {
        let mut section = wasm_encoder::MemorySection::new();
        for (_memory_ref, memory) in self.memories.defined_iter() {
            section.memory(memory.entity_type.into());
        }
        output_module.section(&section);
    }

    fn generate_tag_section(&self, output_module: &mut wasm_encoder::Module) {
        let mut section = wasm_encoder::TagSection::new();
        for (_tag_ref, tag) in self.tags.defined_iter() {
            section.tag(tag.entity_type.try_into().unwrap());
        }
        output_module.section(&section);
    }

    fn generate_global_section(&self, output_module: &mut wasm_encoder::Module) -> Result<()> {
        let mut section = wasm_encoder::GlobalSection::new();
        for (_global_ref, global) in self.globals.defined_iter() {
            let mut bytes = Vec::new();
            let entity_type: wasm_encoder::GlobalType = global.entity_type.try_into().unwrap();
            entity_type.encode(&mut bytes);
            Self::emit_body(&global.body, &mut bytes)?;

            section.raw(&bytes);
        }
        output_module.section(&section);
        Ok(())
    }

    /// Start fn section - is just a number of entrypoint function.
    /// We will place one function with void signature as start function, with calls of all functions defined in `self.start_functions` array.
    fn generate_start_function_section(&self, output_module: &mut wasm_encoder::Module) {
        if let Some(start_function) = &self.extra_state.start_function {
            output_module.section(&wasm_encoder::StartSection {
                function_index: start_function.as_u32(),
            });
        }
    }

    fn _generate_element_section_segment(
        section: &mut wasm_encoder::ElementSection,
        offset: &wasm_encoder::ConstExpr,
        table: Option<TableRef>,
        func_ids: Vec<u32>,
    ) {
        section.segment(wasm_encoder::ElementSegment {
            mode: wasm_encoder::ElementMode::Active {
                table: table.map(|t| t.as_u32()),
                offset,
            },
            elements: wasm_encoder::Elements::Functions(func_ids.into()),
        });
    }

    fn _generate_indirect_function_table(
        &self,
        section: &mut wasm_encoder::ElementSection,
    ) -> Result<GappedMap<FunctionRef, ElementItemId>> {
        let func_id_table = self.indirect_function_table.table_id;
        assert!(
            self.tables.try_get_entity(func_id_table).is_some(),
            "Indirect function table must be defined as a table in the module"
        );

        let func_id_table = if self.tables.len() == 1 {
            // TODO: if multi-table disabled?
            None // default if only one table
        } else {
            Some(func_id_table)
        };

        let mut result_indirect_fn_mapping = GappedMap::new();

        self.indirect_function_table
            .for_each_segment(|_segment_id, content| {
                let mut content = content.peekable();
                let segment_offset = content
                    .peek()
                    .map(|(elem_id, _)| elem_id.as_u32())
                    .expect("BUG: iterator without items");

                let element_start = self
                    .indirect_function_table
                    .location
                    .add_offset(segment_offset)
                    .to_init_expr();

                let func_ids = content
                    .map(|(elem_id, func_ref)| {
                        result_indirect_fn_mapping.insert(*func_ref, elem_id);
                        func_ref.as_u32()
                    })
                    .collect::<Vec<_>>();

                Self::_generate_element_section_segment(
                    section,
                    &element_start,
                    func_id_table,
                    func_ids,
                );
            });
        Ok(result_indirect_fn_mapping)
    }

    fn generate_element_section(
        &self,
        output_module: &mut wasm_encoder::Module,
    ) -> Result<GappedMap<FunctionRef, ElementItemId>> {
        let mut section = wasm_encoder::ElementSection::new();

        let res = self._generate_indirect_function_table(&mut section)?;

        output_module.section(&section);
        Ok(res)
    }

    fn generate_data_count_section(&self, output_module: &mut wasm_encoder::Module) {
        output_module.section(&wasm_encoder::DataCountSection {
            count: self.mem_spec.data_segments.len() as u32,
        });
    }

    fn _generate_defined_function(
        &self,
        func_id: FunctionRef,
        func: &DefinedFunction<'_>,
    ) -> Result<Vec<u8>, std::io::Error> {
        let mut writer = Vec::new();
        let function_name = self.get_name(func_id.into());

        // TODO: We can write bytes directly.
        Self::emit_body(&func.body, &mut writer)?;

        log::debug!(
            "Emitting function {function_name}, bytes: {}",
            hex::encode(&writer)
        );

        Ok(writer)
    }
    fn generate_code_section(
        &self,
        output_module: &mut wasm_encoder::Module,
    ) -> Result<SecondaryMap<FunctionRef, FunctionInfo>> {
        let mut functions_mapping = SecondaryMap::new();
        let section =
            wasm_emitter::SectionAdapter::new(wasm_encoder::SectionId::Code.into(), |section| {
                for (id, output_func) in self.functions.defined_iter() {
                    let function = self._generate_defined_function(id, output_func)?;

                    let function_start_offset = section.raw_len_prefixed(&function)?;
                    functions_mapping[id] = FunctionInfo {
                        code_offset: function_start_offset,
                        indirect_table_index: None,
                    }
                }

                Ok(())
            })?;
        output_module.section(&section);

        Ok(functions_mapping)
    }

    fn generate_data_section(
        &self,
        output_module: &mut wasm_encoder::Module,
    ) -> Result<DataSymbolsOffsets> {
        // encoding:
        // - len of data section
        // - count of segments
        // - [segments]
        // where segment:
        // - header (mode/offset)
        // - len of data
        // - data bytes

        // Order data symbols by their segments.
        let (segments, data_mapping) = SegmentLayout::build_for_module(self)?;

        let adapter =
            wasm_emitter::SectionAdapter::new(wasm_encoder::SectionId::Data.into(), |section| {
                for (_id, layout) in segments.iter() {
                    section.item_from_encoder(|e| {
                        wasm_emitter::data_segment_adapter(
                            e,
                            layout.memory_location(),
                            layout.data_stream(),
                        )
                    })?;
                }

                Ok(())
            })?;

        output_module.section(&adapter);

        Ok(data_mapping)
    }

    /// Copy relocs to new FileRelocs.
    ///
    /// - Move out all relocs from entities bodies (patches, and new entities).
    /// - Resolve unresolved relocs (e.g. when symbol is
    ///   copied their relocs is refer to symbol in input file, and we need to remap it to symbol in output file).
    /// - Shift their offset to be relative to entity body start.
    ///
    /// Returns `FileRelocs` with relocs related to symbol start.
    #[tracing::instrument(skip_all)]
    pub fn copy_and_resolve_relocs(
        &mut self,
        module_info: &OutputEntitiesResolver,
        input_files: &FileLoader,
    ) -> anyhow::Result<FileRelocs> {
        let mut relocs = Vec::new();
        let mut code_owners = GappedMap::new();
        let mut data_owners = GappedMap::new();

        for (entity, body) in self.entities_bodies_mut() {
            let start = relocs.len();

            // - resolve+shift "input" relocs from fixups
            // - resolve+shift original relocs
            match body {
                EntityBody::Copied(copied) => {
                    Self::resolve_relocs_for_copied_body(
                        entity,
                        module_info,
                        input_files,
                        copied,
                        &mut relocs,
                    );
                }
                // relocs already resolved, and shifted relative to body
                // since this type of entity cannot have src file)
                EntityBody::New { new_relocs, .. } => {
                    debug_assert!(
                        module_info.get_entity_src(entity).is_none(),
                        "New entity cannot have src file, but got src for entity {entity}"
                    );
                    relocs.extend(new_relocs.drain(..));
                }
            }

            let range = start..relocs.len();
            if range.is_empty() {
                continue;
            }

            match entity {
                EntityKind::Function(func) => {
                    code_owners
                        .insert(func, crate::linkage::file_db::RelocRange::from_range(range));
                }
                EntityKind::DataSymbol(data) => {
                    data_owners
                        .insert(data, crate::linkage::file_db::RelocRange::from_range(range));
                }
                entity => panic!(
                    "Only functions and data symbols can have relocs, but got entity with id {entity} relocs: {relocs:?}"
                ),
            };
        }
        Ok(FileRelocs::build_from_parts(
            relocs.into_boxed_slice(),
            code_owners,
            data_owners,
        ))
    }

    fn resolve_relocs_for_copied_body(
        entity: EntityKind,
        module_info: &OutputEntitiesResolver,
        input_files: &FileLoader,

        EntityBodyCopy {
            fixups: patches,
            filtered_relocs,
            original_range,
            ..
        }: &mut EntityBodyCopy,
        relocs: &mut Vec<EntityRelocationEntry>,
    ) {
        let src_ref = module_info
            .get_entity_src(entity)
            .unwrap_or_else(|| panic!("Entity {entity} should have src file"));
        let src_file = input_files.get_file(src_ref.file_id);
        let original_relocs = src_file
            .relocs
            .get_entity_relocs(src_ref.entity)
            .unwrap_or_default();

        // Convert entity from input entity index to index in output file.
        let resolve_entity = |input: EntityKind| -> Option<EntityKind> {
            if matches!(input, EntityKind::Type(_)) {
                log::error!(
                    "this kind of relocs are not supported yet, skipping reloc with symbol id {input}"
                );
                return None; // TODO: support type relocs
            }
            let file_entity_ref = src_ref.other_entity(input);

            let entity = module_info
                .get_output_entity(&file_entity_ref)
                .expect("reference to undefined entity");
            Some(entity)
        };
        let mut shift_map = ShiftMap::default();

        for patch in patches {
            // Apply shifts to patch relocations (this is done before adding patch shift point)
            for reloc in patch.new_relocs.drain(..) {
                let reloc_start = patch.old_range.start as u32 + reloc.offset;

                // new relocs are already relative to body start
                let shifted_offset = shift_map
                    .get_shifted_offset(reloc_start)
                    .expect("new relocation cannot be in removed area");

                let symbol = match reloc.symbol_id {
                    OutputEntityRef::Resolved(v) => v,
                    OutputEntityRef::FromInput(v) => {
                        let Some(entity) = resolve_entity(v) else {
                            continue;
                        };
                        entity
                    }
                };
                relocs.push(EntityRelocationEntry {
                    offset: shifted_offset,
                    symbol_id: symbol,
                    symbol_op: reloc.symbol_op,
                    addend: reloc.addend,
                    relation: reloc.relation,
                    encoding: reloc.encoding,
                    width: reloc.width,
                });
            }
            // if patch == old_bytes - no reason to add shifts.
            if patch.size() > 0 {
                // then add new shift point
                shift_map.add_shift_point(ShiftPoint {
                    at: patch.old_range.end as u32,
                    shift: patch.size() as i32,
                });
            }
        }

        // Now we can add pre-existing relocs with shifts applied
        for (i, reloc) in original_relocs.iter().enumerate() {
            if filtered_relocs.contains(i) {
                // reloc was removed.
                continue;
            }

            // id from input file, map to id in output file.
            let Some(entity) = resolve_entity(reloc.symbol_id) else {
                continue;
            };

            let reloc_start = reloc.offset - original_range.start as u32;
            // offset relative to body.
            let shifted_offset = shift_map
                .get_shifted_offset(reloc_start)
                .expect("relocation cannot be in removed area");

            relocs.push(EntityRelocationEntry {
                offset: shifted_offset,
                symbol_id: entity,
                symbol_op: reloc.symbol_op,
                ..*reloc
            });
        }
    }
    /// Write byte using the modifications to the given writer.
    /// Returns relocations with id's that can be found in and offsets relative to section start.
    fn emit_body(
        body: &EntityBody,
        // output
        writer: &mut impl Write,
    ) -> Result<(), io::Error> {
        for buf in body.iter_chunks() {
            writer.write_all(buf)?;
        }
        Ok(())
    }
}

///
///  Emit output modules, from split program info.
///
pub fn emit_modules(
    input_files: &FileLoader,
    program_info: &SplitProgramInfo,
    emit_fn: impl FnMut(&OutputId, &[u8]) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    EmitContext::emit_modules(input_files, program_info, emit_fn)
}

#[doc(hidden)]
pub fn split_routine_generic_test(src: &[u8]) -> anyhow::Result<Vec<(OutputId, Vec<u8>)>> {
    let mut file_loader = FileLoader::new();
    let input_file = file_loader
        .load_from_bytes(src.to_vec().into_boxed_slice())
        .unwrap();
    let input = file_loader.get_file(input_file);

    let dep_graph = crate::analysis::get_dependencies(input).unwrap();
    let split_points = crate::analysis::find_split_points(
        &input.module,
        crate::analysis::SplitPointExtractor::Legacy,
    )
    .unwrap();

    let wbg_descriptors = crate::analysis::wbg_closures(&input.module, &dep_graph);
    let split = crate::analysis::compute_split_modules(
        &input.module,
        &dep_graph,
        &split_points,
        &wbg_descriptors,
        true,
    )
    .unwrap();

    let mut result = vec![];
    emit_modules(&file_loader, &split, |ident, bytes| {
        log::debug!("Emitted module {ident} with size {}", bytes.len());
        result.push((ident.clone(), bytes.to_vec()));
        Ok(())
    })
    .unwrap();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use smallvec::smallvec;
    use tempdir::TempDir;
    use wasmparser::FuncType;

    use super::*;
    use crate::{
        emit::plan::OutputModule,
        raw::DataSegmentId,
        typed::{
            DefinedDataChunk, EntityBody, ExportNames, FileLoader, ImportedFunction, LoadedFile,
            Module, WithoutBody,
            data::{DataChunkType, DataSegmentInfo, SegmentPlacement},
        },
    };

    // Use split routine without extraction as a smoke test.
    // In the result we should get one module with all entities as in input module.
    #[test]
    fn emit_loaded_split_roundtrip() {
        env_logger::try_init().ok();
        // let module_name = "simple_graph";
        let src = crate::testfiles::SIMPLE_GRAPH;

        let mut file_loader = FileLoader::new();

        let data = src.to_vec().into_boxed_slice();
        let input_file = file_loader.load_from_bytes(data).unwrap();

        let input = file_loader.get_file(input_file);

        let dep_graph = crate::analysis::get_dependencies(input).unwrap();
        let split = crate::analysis::compute_split_modules(
            &input.module,
            &dep_graph,
            &[],
            &Default::default(),
            true,
        )
        .unwrap();
        assert!(
            split.output_modules.len() == 1,
            "There should be at least one split module"
        );

        dbg!(&dep_graph);
        dbg!(&split);

        let ctx: EmitContext<'_> = split.into_emit_context(&file_loader);

        // extract plan of first module
        let (_, (_, plan)) = ctx.output_plans.into_iter().next().unwrap();
        let snapshot = file_loader.get_snapshot();
        let OutputModule { module: output, .. } = plan
            .copy_entities(&file_loader, &snapshot, |_| true)
            .unwrap();

        let mut buf = wasm_encoder::Module::new();
        output.generate(&mut buf).unwrap();
        let res: Vec<u8> = buf.finish();
        let tmp_out = TempDir::new("emit").unwrap();
        let out_file =tmp_out.path().join("simple_graph_emit.wasm");
            
        std::fs::write(out_file, &res).unwrap();

        let raw = crate::raw::ObjectReader::parse(&res).unwrap();
        dbg!(&input.wasm_reader);
        dbg!(&raw);
        dbg!(&output);

        // ensure loadable, and compare with original
        let new_file = file_loader.load_from_bytes(res.into_boxed_slice()).unwrap();

        let result = file_loader.get_file(new_file);
        dbg!(&result.module);

        todo!()
    }

    #[test]
    fn split_routine_example_emit() {
        env_logger::try_init().ok();
        let src = crate::testfiles::EXAMPLE_WASM;
        split_routine_generic_test(src).unwrap();
    }

    #[test]
    fn split_routine_simple_emit() {
        env_logger::try_init().ok();
        let src = crate::testfiles::SIMPLE_GRAPH;
        split_routine_generic_test(src).unwrap();
    }

    #[test]
    fn split_routine_example_roundtrip() {
        env_logger::try_init().ok();
        let src = crate::testfiles::EXAMPLE_WASM;
        let emitted = split_routine_generic_test(src).unwrap();
        assert_eq!(emitted.len(), 3);
        for (ident, bytes) in emitted {
            log::info!("Emitted module {ident} with size {}", bytes.len());
            let loaded = LoadedFile::from_wasm_bytes(&bytes).unwrap();
            log::debug!("Loaded module {ident}: {:#?}", loaded.module);
        }
    }

    #[test]
    fn create_from_scratch() {
        let mut module = Module::new();

        let imported = module.functions.push_import(ImportedFunction {
            module: "env".into(),
            name: "bar".into(),
            renamed_as: None,
            export_as: ExportNames::default(),
            entity_type: FuncType::new(None, None),
        });
        module.extra_state.start_functions.push(imported);
        module.create_empty_indirect_fn_table();
        module.memories.push_defined(WithoutBody {
            entity_type: crate::raw::MemoryType {
                memory64: false,
                shared: false,
                initial: 1,
                maximum: None,
                page_size_log2: None,
            },
            export_as: ExportNames::default(),
            name: Some("__base_memory".into()),
        });

        module.mem_spec.data_segments.push(DataSegmentInfo {
            name: "data".into(),
            location: SegmentPlacement::ContinuesMemory,
            pow2align: 0,
        });
        module.data.push_defined(DefinedDataChunk {
            body: EntityBody::New {
                new_bytes: vec![1, 2, 3].into(),
                new_relocs: smallvec![],
            },
            name: Some("data".into()),
            entity_type: DataChunkType {
                segment_id: DataSegmentId::from_u32(0),
                pow2align: 0,
            },
            export_as: ExportNames::default(),
        });

        let module = module.into_locked();
        let mut output = wasm_encoder::Module::new();
        module.generate(&mut output).unwrap();
        let bytes = output.finish();

        let loaded = LoadedFile::from_wasm_bytes(&bytes).unwrap();
        assert_eq!(loaded.module.functions.len(), 1);
        assert_eq!(loaded.module.tables.len(), 1);
        assert_eq!(loaded.module.memories.len(), 1);
        assert_eq!(loaded.module.data.len(), 1);
    }
}
