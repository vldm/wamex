use std::{collections::HashMap, io::Write};

use anyhow::Result;
use cranelift_entity::{EntityRef, PrimaryMap, SecondaryMap};
use wasm_encoder::{Encode, FunctionSection};
use wasmparser::FuncType;

use crate::{
    analysis::OutputModuleInfo,
    emit::{
        memory_layout::{DataSymbolsOffsets, SegmentLayout},
        modify::OutputEntityRef,
        relocation::{EntityLocation, file_mapping::OutputFileInfo},
    },
    helpers::{ShiftMap, ShiftPoint, encoding_size},
    index::{Building, GappedMap},
    linkage::{
        file_db::{EntityRelocationEntry, FileRelocs},
        reloc::EntitySymbol,
    },
    raw::{DataSegmentId, FuncTypeId},
    typed::{
        DefinedFunction, EntityBody, ExportNames, FileId, FileLoader, FunctionRef, ImportedEntity,
        Module,
        common_index::{EntitiesSnapshot, EntityKind, TempEntityKind},
        data::SpecificLocation,
    },
};

pub mod memory_layout;
pub mod modify;
pub mod relocation;

/// Module layout suitable for applying relocates.
pub struct ModuleLayout {
    pub code_start: usize,
    pub data_start: usize,
    pub functions_mapping: SecondaryMap<FunctionRef, usize>,
    /// Mapping of module data symbols, to their offsets.
    pub data_mapping: DataSymbolsOffsets,
}

impl<'src> Module<'src> {
    pub fn generate(&self, output_module: &mut wasm_encoder::Module) -> Result<ModuleLayout> {
        // TODO: Support extra segments.
        let (segments, data_mapping) = SegmentLayout::build_for_module(self)?;

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

        self.generate_element_section(output_module)?;
        self.generate_data_count_section(&segments, output_module);
        let code_start = output_module.len() + 1; // +1 for code section id, we need to know offset of code section for code relocs.
        let functions_mapping = self.generate_code_section(output_module)?;
        let data_start = output_module.len() + 1; // +1 for data section id, we need to know offset of data section for data relocs.

        self.generate_data_section(&segments, output_module)?;

        // // self.generate_wasm_bindgen_sections(output_module);
        // // Names + Linking + Relocations
        // self.generate_compiler_tools_sections(output_module, code_relocs, data_relocs)?;
        // self.generate_target_features_section(output_module)?;
        // self.generate_custom_sections(output_module)?;
        // todo!()
        Ok(ModuleLayout {
            code_start,
            data_start,
            functions_mapping,
            data_mapping,
        })
    }

    fn start_fn_type(&self) -> Option<(FunctionRef, FuncType)> {
        if self.start_functions.is_empty() {
            None
        } else {
            Some((
                FunctionRef::new(self.functions.len()),
                FuncType::new([], []),
            ))
        }
    }

    /// Generate type section, return map from FunctionId to type index.
    pub fn generate_type_section(
        &self,
        output_module: &mut wasm_encoder::Module,
    ) -> SecondaryMap<FunctionRef, FuncTypeId> {
        let mut function_types = SecondaryMap::new();
        let mut uniq_types = HashMap::<&wasmparser::FuncType, FuncTypeId>::new();

        // add start fn void type if start fn exist
        let start_fn = self.start_fn_type();
        let start_fn_iter = start_fn
            .iter()
            .map(|(func_ref, func_type)| (*func_ref, func_type));

        let func_types = self
            .functions
            .iter()
            .map(|(id, func)| (id, func.get_type()))
            .chain(start_fn_iter.clone());

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
        dbg!(fn_type_map.iter().count());

        dbg!(self.functions.iter().count());

        let mut section = FunctionSection::new();
        for (_, type_id) in fn_type_map.iter() {
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
        if !self.start_functions.is_empty() {
            #[cfg(debug_assertions)]
            self.start_functions.iter().for_each(|f| {
                let func = self.functions.items.get_entity(*f);
                let is_void =
                    func.get_type().params().is_empty() && func.get_type().results().is_empty();
                assert!(
                    is_void,
                    "Start function must have void signature. Function {} has non-void signature",
                    f
                );
            });
            // then during code generation we will generate start function as latest defined function.
            let start_fn_id = self.functions.len();
            output_module.section(&wasm_encoder::StartSection {
                function_index: start_fn_id.try_into().unwrap(),
            });
        }
    }

    fn _generate_element_section_segment(
        section: &mut wasm_encoder::ElementSection,
        offset: &wasm_encoder::ConstExpr,
        func_ids: Vec<u32>,
    ) {
        section.segment(wasm_encoder::ElementSegment {
            mode: wasm_encoder::ElementMode::Active {
                table: None,
                offset,
            },
            elements: wasm_encoder::Elements::Functions(func_ids.into()),
        });
    }

    // <DOC FOR WAMEX-SPLIT part>
    // The indirect_function table is shared between main module and submodules.
    // it's layout is:
    // [ 0: empty ]
    // [ 1..N: functions used in this module ]
    // [ N+1..N+M: reserved space for lazy stubs, main module fill it empty, and submodules fill it with stubs ]
    // [ N+M+1.. : dynamic allocated entries - used for tables in submodules ]
    //
    // Example of final layout:
    // 1. After main load:
    // [0, f1, f2, f3, ..., s1_entry1_uninit, s1_entry2_uninit, s2_entry1_uninit, ...]
    // 2. After submodule load:
    // [0, f1, f2, f3, ..., s1_entry1,        s1_entry2,        0,                 s1_f1, s1_f2, ...]
    // 3. If submodule reloaded, the following changes are applied:
    // [_, _, _, _, ...,    s1_FIX_entry1,    s1_FIX_entry2,    _,                 _,     _,     s1_FIX_f1, s1_FIX_f2, ...]
    // Note that original s1_f1 and s1_f2 are not removed, because other submodules may use them.
    // And only after calling linker::unload we can reuse these entries.
    fn _generate_indirect_function_table(
        &self,
        section: &mut wasm_encoder::ElementSection,
    ) -> Result<()> {
        let func_id_table = self.indirect_function_table.table_id;
        assert!(
            self.tables.items.try_get_entity(func_id_table).is_some(),
            "Indirect function table must be defined as a table in the module"
        );

        log::error!(
            "TEST _generate_indirect_function_table {:?}",
            self.indirect_function_table
        );

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
                    .map(|(_elem_id, func_ref)| func_ref.as_u32())
                    .collect::<Vec<_>>();

                Self::_generate_element_section_segment(section, &element_start, func_ids);
            });
        Ok(())
    }

    fn generate_element_section(&self, output_module: &mut wasm_encoder::Module) -> Result<()> {
        let mut section = wasm_encoder::ElementSection::new();

        self._generate_indirect_function_table(&mut section)?;

        output_module.section(&section);
        Ok(())
    }

    fn generate_data_count_section(
        &self,
        segments: &PrimaryMap<DataSegmentId, SegmentLayout>,
        output_module: &mut wasm_encoder::Module,
    ) {
        output_module.section(&wasm_encoder::DataCountSection {
            count: segments.len() as u32,
        });
    }

    fn _generate_defined_function(
        &self,
        func_id: FunctionRef,
        func: &DefinedFunction<'_>,

        section: &mut wasm_encoder::CodeSection,
    ) -> Result<()> {
        let mut writer = Vec::new();
        let function_name = self.get_name(func_id.into());
        log::debug!("Emitting function {function_name}");

        // TODO: We can write bytes directly.
        Self::emit_body(&func.body, &mut writer)?;

        section.raw(&writer);

        Ok(())
    }
    fn generate_code_section(
        &self,
        output_module: &mut wasm_encoder::Module,
    ) -> Result<SecondaryMap<FunctionRef, usize>> {
        let defined_functions_count = self.functions.defined_iter().len() as u32
            + if !self.start_functions.is_empty() {
                1 // start function
            } else {
                0
            };

        // function definitions start after small "header"
        let start_of_functions_def = encoding_size(defined_functions_count);

        let mut functions_mapping = SecondaryMap::new();
        let mut section = wasm_encoder::CodeSection::new();
        for (id, output_func) in self.functions.defined_iter() {
            // TODO: shift relocs
            let function_start_offset = start_of_functions_def + section.byte_len();
            self._generate_defined_function(id, output_func, &mut section)?;
            functions_mapping[id] = function_start_offset;
        }

        // TODO: push it as last defined during into_finalized() call?
        // TODO: check fn type to be void.
        if !self.start_functions.is_empty() {
            let start_fn_ref = FunctionRef::new(self.functions.len());
            // generate start function as last function in code section, and add call to all start functions in its body
            let mut func = wasm_encoder::Function::new([]);
            let mut sink = func.instructions();
            for func_ref in &self.start_functions {
                sink.call(func_ref.as_u32()); // TODO: Relocs?
            }
            sink.end();

            let function_start_offset = start_of_functions_def + section.byte_len();
            section.function(&func);
            functions_mapping[start_fn_ref] = function_start_offset;
        }

        output_module.section(&section);

        Ok(functions_mapping)
    }

    fn generate_data_section(
        &self,
        segments: &PrimaryMap<DataSegmentId, SegmentLayout>,
        output_module: &mut wasm_encoder::Module,
    ) -> Result<()> {
        let mut section = wasm_encoder::DataSection::new();

        for (_id, layout) in segments.iter() {
            if layout.is_empty() {
                continue;
            }

            let expr = layout
                .memory_location()
                .as_ref()
                .map(SpecificLocation::to_init_expr);

            section.segment(wasm_encoder::DataSegment {
                mode: expr
                    .as_ref()
                    .map_or(wasm_encoder::DataSegmentMode::Passive, |offset| {
                        wasm_encoder::DataSegmentMode::Active {
                            memory_index: layout.memory_index(),
                            offset,
                        }
                    }),
                data: layout.data_stream(),
            });
            // todo: collect relocs
        }

        output_module.section(&section);

        Ok(())
    }

    /// Copy relocs to new FileRelocs.
    ///
    /// - Move out all relocs from entities bodies (patches, and new entities).
    /// - Resolve unresolved relocs (e.g. when symbol is
    ///   copied their relocs is refer to symbol in input file, and we need to remap it to symbol in output file).
    /// - Shift their offset to be relative to entity body start.
    ///
    /// Returns `FileRelocs` with relocs related to symbol start.
    pub fn copy_and_resolve_relocs(
        &mut self,
        module_info: &OutputFileInfo,
        input_files: &FileLoader,
    ) -> anyhow::Result<FileRelocs> {
        let mut relocs = Vec::new();
        let mut code_owners = GappedMap::new();
        let mut data_owners = GappedMap::new();

        for (entity, body) in self.entities_bodies_mut() {
            let src_ref = module_info
                .get_entity_src(entity)
                .expect("module_info must have a corresponding src entity");
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
                    .expect("reloc symbol not found in output file");
                Some(entity)
            };

            let start = relocs.len();

            match body {
                EntityBody::Copied {
                    patches,
                    filtered_relocs,
                    original_range,
                    ..
                } => {
                    let mut shift_map = ShiftMap::default();
                    for patch in patches {
                        // Apply shifts to patch relocations (this is done before adding patch shift point)
                        for reloc in patch.new_relocs.drain(..) {
                            let reloc_start = patch.old_range.start as u32 + reloc.offset;

                            let shifted_offset = shift_map
                                .get_shifted_offset(reloc_start)
                                .expect("new relocation cannot be in removed area")
                                - original_range.start as u32;

                            let symbol = match reloc.symbol {
                                OutputEntityRef::Resolved(v) => v,
                                OutputEntityRef::FromInput(mut v) => {
                                    let Some(entity) = resolve_entity(v.ty) else {
                                        continue;
                                    };
                                    v.ty = entity;
                                    v
                                }
                            };
                            relocs.push(EntityRelocationEntry {
                                offset: shifted_offset,
                                symbol,
                                addend: reloc.addend,
                                relation: reloc.relation,
                                encoding: reloc.encoding,
                                width: reloc.width,
                            });
                        }
                        // then add new shift point
                        shift_map.add_shift_point(ShiftPoint {
                            at: patch.old_range.end as u32,
                            shift: patch.size() as i32,
                        });
                    }

                    // Now we can add pre-existing relocs with shifts applied
                    for (i, reloc) in original_relocs.iter().enumerate() {
                        if filtered_relocs.contains(i) {
                            // reloc was removed.
                            continue;
                        }

                        // id from input file, map to id in output file.
                        let mut symbol: EntitySymbol = reloc.symbol;
                        let Some(entity) = resolve_entity(symbol.ty) else {
                            continue;
                        };

                        // offset relative to body.
                        let shifted_offset = shift_map
                            .get_shifted_offset(reloc.offset)
                            .expect("relocation cannot be in removed area")
                            - original_range.start as u32; // and then shift to section-relative offset

                        symbol.ty = entity;
                        relocs.push(EntityRelocationEntry {
                            offset: shifted_offset,
                            symbol,
                            ..*reloc
                        });
                    }
                }
                EntityBody::New { new_relocs, .. } => {
                    debug_assert!(
                        original_relocs.is_empty(),
                        "new body cannot have original relocs"
                    );
                    relocs.extend(new_relocs.drain(..));
                }
            }

            let range = start..relocs.len();
            if range.is_empty() {
                continue;
            }
            log::warn!("we can sort relocs there");
            // relocs[range.clone()].sort_unstable_by_key(|v| v.offset);

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
    /// Write byte using the modifications to the given writer.
    /// Returns relocations with id's that can be found in and offsets relative to section start.
    fn emit_body(
        body: &EntityBody,
        // output
        writer: &mut impl Write,
    ) -> Result<()> {
        for buf in body.iter_chunks() {
            writer.write_all(buf)?;
        }
        Ok(())
    }
}

pub fn create_split_module<'src>(
    input_files: &FileLoader,
    input_file: FileId,
    src: &Module<'src>,          // tmp field, should be FileLoader instead.
    snapshot: &EntitiesSnapshot, // this is
    output_info: OutputModuleInfo,
) -> Result<(Module<'src>, OutputFileInfo)> {
    let mut module: Module<'src, Building> = Module::new();
    // map of entities from input file to entities in output module.
    let mut file_info = OutputFileInfo::new();

    let mut used_queue: Vec<(_, TempEntityKind)> = Vec::new();

    // Copy defined symbols to the output module.
    // TODO: Clear exports if not set?
    for entity in output_info.defined_symbols {
        match snapshot.unpack_ref(entity) {
            EntityKind::Function(f) => {
                let func = src.functions.get_entity(f);
                let new = module.functions.push_entity(func.cloned());
                used_queue.push((EntityLocation::from_parts(input_file, f.into()), new.into()));
            }
            EntityKind::Global(g) => {
                let global = src.globals.get_entity(g);
                let new = module.globals.push_entity(global.cloned());
                used_queue.push((EntityLocation::from_parts(input_file, g.into()), new.into()));
            }
            EntityKind::Memory(m) => {
                let memory = src.memories.get_entity(m);
                let new = module.memories.push_entity(memory.cloned());
                used_queue.push((EntityLocation::from_parts(input_file, m.into()), new.into()));
            }
            EntityKind::Table(t) => {
                let table = src.tables.get_entity(t);
                let new = module.tables.push_entity(table.cloned());
                used_queue.push((EntityLocation::from_parts(input_file, t.into()), new.into()));
            }
            EntityKind::Tag(t) => {
                let tag = src.tags.get_entity(t);
                let new = module.tags.push_entity(tag.cloned());
                used_queue.push((EntityLocation::from_parts(input_file, t.into()), new.into()));
            }
            EntityKind::DataSymbol(d) => {
                let data = src.data.get_entity(d);
                let new = module.data.push_entity(data.cloned());
                used_queue.push((EntityLocation::from_parts(input_file, d.into()), new.into()));
            }
            EntityKind::Type(_) => {} // type is pseudo-entity - and doesn't exist in module.
        }
    }

    // Add imports for used symbols (even if they are defined in source).
    for import in output_info.imports {
        match snapshot.unpack_ref(import) {
            EntityKind::Function(f) => {
                let id = module.functions.push_import(ImportedEntity {
                    module: input_file.to_string().into(), // use file_id
                    name: src.get_name(f.into()),          // use original name
                    entity_type: src.functions.get_entity(f).get_type().clone(),
                    export_as: ExportNames::default(),
                    renamed_as: None,
                });
                used_queue.push((EntityLocation::from_parts(input_file, f.into()), id.into()));
            }
            // stack_pointer, mb heap_base/__data_end, etc.
            EntityKind::Global(g) => {
                let id = module.globals.push_import(ImportedEntity {
                    module: input_file.to_string().into(), // use file_id
                    name: src.get_name(g.into()),          // use original name
                    entity_type: *src.globals.get_entity(g).get_type(),
                    export_as: ExportNames::default(),
                    renamed_as: None,
                });
                used_queue.push((EntityLocation::from_parts(input_file, g.into()), id.into()));
            }
            // indirect_function_table
            EntityKind::Table(t) => {
                let id = module.tables.push_import(ImportedEntity {
                    module: input_file.to_string().into(), // use file_id
                    name: src.get_name(t.into()),          // use original name
                    entity_type: *src.tables.get_entity(t).get_type(),
                    export_as: ExportNames::default(),
                    renamed_as: None,
                });
                used_queue.push((EntityLocation::from_parts(input_file, t.into()), id.into()));
            }
            // only one memory
            EntityKind::Memory(m) => {
                let id = module.memories.push_import(ImportedEntity {
                    module: input_file.to_string().into(), // use file_id
                    name: src.get_name(m.into()),          // use original name
                    entity_type: *src.memories.get_entity(m).get_type(),
                    export_as: ExportNames::default(),
                    renamed_as: None,
                });
                used_queue.push((EntityLocation::from_parts(input_file, m.into()), id.into()));
            }

            // Future support
            EntityKind::Tag(m) => {
                let id = module.tags.push_import(ImportedEntity {
                    module: input_file.to_string().into(), // use file_id
                    name: src.get_name(m.into()),          // use original name
                    entity_type: *src.tags.get_entity(m).get_type(),
                    export_as: ExportNames::default(),
                    renamed_as: None,
                });
                used_queue.push((EntityLocation::from_parts(input_file, m.into()), id.into()));
            }
            EntityKind::DataSymbol(d) => {
                let id = module.data.push_import(ImportedEntity {
                    module: input_file.to_string().into(), // use file_id
                    name: src.get_name(d.into()),          // use original name
                    entity_type: (), // TODO: add type for data symbols if needed
                    export_as: ExportNames::default(),
                    renamed_as: None,
                });
                used_queue.push((EntityLocation::from_parts(input_file, d.into()), id.into()));
            }
            EntityKind::Type(_) => {} // type is pseudo-entity - and doesn't exist in module.
        }
    }

    // after index finalization, we can make some additional transformation
    let mut module = module.into_locked();

    // Fill mapping for all used entities.
    for (src, entity) in used_queue {
        file_info.add_entity_mapping(src, entity.to_stable(&module));
    }

    // Now copy and resolve relocs.
    let relocs = module.copy_and_resolve_relocs(&file_info, input_files)?;
    // collect indirect table (used by relocs)
    module.extend_indirect_table_from_relocs(&relocs);
    // Fill segment spec (by clonning from source)
    // TODO: what to do if more than one source?
    module.mem_spec = src.mem_spec.clone();

    Ok((module, file_info))
}

#[cfg(test)]
mod tests {
    use crate::{
        emit::create_split_module,
        typed::{FileLoader, common_index::EntitiesSnapshot},
    };

    #[test]
    fn emit_loaded() {
        env_logger::try_init().ok();
        let module_name = "simple_graph";
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

        let (output, file_info) = create_split_module(
            &file_loader,
            input_file,
            &input.module,
            &EntitiesSnapshot::new(&input.module),
            split.output_modules[0].1.clone(),
        )
        .unwrap();

        let mut buf = wasm_encoder::Module::new();
        output.generate(&mut buf).unwrap();
        let res: Vec<u8> = buf.finish();

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
}
