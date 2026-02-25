use std::{collections::HashMap, io::Write};

use anyhow::Result;
use cranelift_entity::{EntityRef, PrimaryMap};
use wasm_encoder::{Encode, FunctionSection};
use wasmparser::FuncType;

use crate::{
    emit::memory_layout::SegmentLayout,
    helpers::{ShiftMap, ShiftPoint},
    index::{GappedMap, IdVec},
    linkage::reloc::RelocationEntry,
    raw::{DataSegmentId, FuncTypeId},
    typed::{
        EntityBody, FunctionRef, Module, common_index::ErasedEntityRef, data::SpecificLocation,
    },
};

pub mod memory_layout;
pub mod modify;
pub mod relocation;

#[derive(Default, Debug, Clone, Copy)]
pub struct ModuleConfig {
    // Is this module is emitting as position-independent code
    pub dyn_base: bool,
}

impl<'src> Module<'src> {
    pub fn generate(&self, output_module: &mut wasm_encoder::Module) -> Result<()> {
        // TODO: Support extra segments.
        let (mut segments, _) = SegmentLayout::build_for_module(self)?;

        // self.generate_dylink0_section(output_module)?;

        let fn_type_map = self.generate_type_section(output_module);

        self.generate_import_section(&fn_type_map, output_module);
        self.generate_function_type_ids_section(&fn_type_map, output_module);
        self.generate_table_sections(output_module);
        self.generate_memory_section(output_module);
        self.generate_global_section(output_module)?;
        self.generate_tag_section(output_module);
        self.generate_export_section(output_module);
        self.generate_start_function_section(output_module);

        self.generate_element_section(output_module)?;

        // let code_relocs =
        //     self.generate_code_section(computed_modules, output_module, precise_modification)?;
        // let data_relocs = self.generate_data_section(output_module)?;

        // // self.generate_wasm_bindgen_sections(output_module);
        // // Names + Linking + Relocations
        // self.generate_compiler_tools_sections(output_module, code_relocs, data_relocs)?;
        // self.generate_target_features_section(output_module)?;
        // self.generate_custom_sections(output_module)?;
        todo!()
    }
    /// Generate type section, return map from FunctionId to type index.
    pub fn generate_type_section(
        &self,
        output_module: &mut wasm_encoder::Module,
    ) -> PrimaryMap<FunctionRef, FuncTypeId> {
        let mut function_types = PrimaryMap::new();
        let mut uniq_types = HashMap::<&wasmparser::FuncType, FuncTypeId>::new();

        // Collect unique types
        for (id, func) in self.functions.iter() {
            let func_type = func.get_type();
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
        fn_type_map: &PrimaryMap<FunctionRef, FuncTypeId>,
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
                for export in &self.$kind.exports {
                    section.export(
                        &export.name,
                        wasm_encoder::ExportKind::$construct,
                        export.entity_index.as_u32(),
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
        fn_type_map: &PrimaryMap<FunctionRef, FuncTypeId>,
        output_module: &mut wasm_encoder::Module,
    ) {
        let mut section = FunctionSection::new();
        for (_, type_id) in fn_type_map {
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
            section.memory((*memory).into());
        }
        output_module.section(&section);
    }

    fn generate_tag_section(&self, output_module: &mut wasm_encoder::Module) {
        let mut section = wasm_encoder::TagSection::new();
        for (_tag_ref, tag) in self.tags.defined_iter() {
            section.tag((*tag).try_into().unwrap());
        }
        output_module.section(&section);
    }

    fn generate_global_section(&self, output_module: &mut wasm_encoder::Module) -> Result<()> {
        let mut section = wasm_encoder::GlobalSection::new();
        for (_global_ref, global) in self.globals.defined_iter() {
            let mut bytes = Vec::new();
            let entity_type: wasm_encoder::GlobalType = global.entity_type.try_into().unwrap();
            entity_type.encode(&mut bytes);
            let relocs = emit_body(
                &global.body,
                &[], // no relocs for global
                &mut bytes,
            )?;
            assert_eq!(relocs.len(), 0); // global cannot have relocs, so this should be always 0.

            section.raw(&bytes);
        }
        output_module.section(&section);
        Ok(())
    }

    /// Start fn section - is just a number of entrypoint function.
    /// We will place one function with void signature as start function, with calls of all functions defined in `self.start_functions` array.
    fn generate_start_function_section(&self, output_module: &mut wasm_encoder::Module) {
        if self.start_functions.len() > 0 {
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

        self.indirect_function_table
            .for_each_segment(|_segment_id, content| {
                let segment_offset = content
                    .peekable()
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

    // fn generate_data_section(
    //     &self,
    //     segments: &PrimaryMap<DataSegmentId, DataSegmentOutput>,
    //     output_module: &mut wasm_encoder::Module,
    // ) -> Result<Vec<RelocationEntry<ErasedEntityRef>>> {

    //     // TODO: Add shifter relocs
    //     let relocs = Vec::new();
    //     let mut section = wasm_encoder::DataSection::new();

    //     for (id, layout) in segments.iter() {
    //         let out = layout.to_segment_output(lib_base_global_id, mem_start, segment_offset)
    //         let mut data = out.data_segment(MEMORY_INDEX);
    //         // Skip empty data segments
    //         // if data.data.is_empty() {
    //         //     continue;
    //         // }
    //         if let Some(relocs) = self.data_relocations.get(id) {
    //             for entry in relocs.iter() {
    //                 let state = modify::StartFnModifyContext {
    //                     data_segment: &mut data.data,
    //                     relocate: RelocateState {
    //                         input_module: self.src,
    //                         computed_modules,
    //                         emit_module: self,
    //                         global_id_mapper: &|global_id: GlobalRef| {
    //                             self.globals.get_output_id(global_id)
    //                         },
    //                     },
    //                 };
    //                 state.apply_relocation(entry)?;
    //             }
    //         }
    //         section.segment(data);
    //     }

    //     output_module.section(&section);
    //     Ok(relocs)
    // }
}

/// Write byte using the modifications to the given writer.
/// Returns relocations shifted to body start.
fn emit_body(
    body: &EntityBody,
    original_relocs: &[RelocationEntry<ErasedEntityRef>],
    writer: &mut impl Write,
) -> Result<Vec<RelocationEntry<ErasedEntityRef>>> {
    match body {
        EntityBody::Copied {
            bytes,
            patches,
            filtered_relocs,
            original_range,
        } => {
            let mut relocs = Vec::with_capacity(
                original_relocs.len() - filtered_relocs.len() + patches.len() * 3,
            ); // rough estimate

            let mut shift_map = ShiftMap::new();
            let mut src_offset = 0usize;
            for patch in patches {
                // write unchanged bytes before patch
                if patch.old_range.start > src_offset {
                    writer.write_all(&bytes[src_offset..patch.old_range.start])?;
                }

                // Apply shifts to patch relocations (this is done before adding patch shift point)
                for reloc in &patch.new_relocs {
                    let shifted_offset = shift_map
                        .get_shifted_offset(reloc.offset)
                        .expect("new relocation cannot be in removed area");
                    // relocs.push(RelocationEntry {
                    //     offset: shifted_offset,
                    //     ..reloc.clone()
                    // });

                    // map symbol_id
                    relocs.push(todo!());
                }
                // then add new shift point
                shift_map.add_shift_point(ShiftPoint {
                    at: patch.old_range.end as u32,
                    shift: patch.size() as i32,
                });

                // write new bytes
                writer.write_all(&patch.new_bytes)?;
                src_offset = patch.old_range.end;
            }
            // write remaining bytes
            if src_offset < bytes.len() {
                writer.write_all(&bytes[src_offset..])?;
            }

            // Now we can add pre-existing relocs with shifts applied
            for (i, reloc) in original_relocs.iter().enumerate() {
                if filtered_relocs.contains(i) {
                    // reloc was removed.
                    continue;
                }
                // offset relative to body.
                let reloc_offset = reloc.offset - original_range.start as u32;

                let shifted_offset = shift_map
                    .get_shifted_offset(reloc_offset)
                    .expect("relocation cannot be in removed area");

                relocs.push(RelocationEntry {
                    offset: shifted_offset,
                    ..reloc.clone()
                });
            }
            Ok(relocs)
        }
        EntityBody::New {
            new_bytes,
            new_relocs,
        } => {
            writer.write_all(new_bytes)?;
            let mapped_relocs = todo!();
            return Ok(mapped_relocs);
        }
    }
}
