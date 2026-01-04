use std::{
    borrow::{self, Cow},
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    ops::Range,
};

use anyhow::{Context, Result, anyhow, bail};
use cranelift_entity::EntityRef;
use index_safety::OutputFuncId;
pub use memory_layout::{DataChunk, DataSegmentOutput, SegmentLayout, SymbolRelation};
use modify::{ModifyContext, StoreType};
use wamex_types::{BumpVersion, dylink0::Dylink0Section, map_vec::MiniSet};
use wasmparser::RelocationEntry;

use crate::{
    InputObject,
    emit::{
        globals::DefinedGlobal,
        index_safety::OutputGlobalId,
        modify::{RelocateState, StartFnGen},
        split::{
            ModuleIdentifier, SharedModuleIdentifier, Split, SplitModuleIdentifier, SplitPoint,
            SplitProgramInfo,
        },
    },
    helpers::encoding_size,
    index::{GappedMap, SecondaryMap},
    read::{
        EntitiesFromInput, MemoryRef,
        raw::{DataSegmentId, FuncTypeId},
        typed::{FunctionRef, GlobalRef},
    },
    symbols::SymbolId,
};

mod builder;
pub mod split;

mod globals;
mod memory_layout;

mod functions;
mod index_safety;
mod modify;
use functions::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkageType {
    /// Keep original layout of data segment and indirect_function_table.
    /// This will emit "gaps" in "element" and in "data" sections.
    ///
    /// This layout is non-portable and doesn't support dynamic module re-building/re-loading.
    /// It is useful in production, to split size of generated WASM binaries.
    ///
    OriginalLayout,

    /// Replace constant offsets with GOT+offset (for memory and table access).
    /// Allocate memory and table dynamically on module loading.
    ///
    /// This allow dynamic reloading of modules.
    /// Modules entrypoints are still located in fixed position in table.
    ///
    DynamicLinking {
        /// Fixed offset for storing entrypoints.
        table_offset: u32,
        /// Number of reserved elements in the table.
        table_num_entrypoints: u32,
    },
}

trait ImportedEntity {
    fn import_name(&self) -> Cow<'_, str>;
    fn module_name(&self) -> Cow<'_, str>;
}

pub(crate) struct GotBase {
    lib_base_id: OutputGlobalId,
    table_base_id: OutputGlobalId,
}

struct SubModuleExtra {
    self_base: GotBase,
    entrypoints: Vec<FunctionRef>,
    extern_modules: Vec<(SharedModuleIdentifier, GotBase)>,
    export_got_with_id: Option<SharedModuleIdentifier>,
}
impl SubModuleExtra {
    const MAIN_GLOBAL_EXPORTS: &[&str] = &["__stack_pointer"]; // "__data_end", "__heap_base" - i
    #[allow(dead_code)]
    const MAIN_GLOBAL_EXPORTS_COUNT: u32 = Self::MAIN_GLOBAL_EXPORTS.len() as u32;
}

// 'any are used because associated types are invariant, and used in default impls for Indexed Vec/Map impls.
pub struct ModuleEmitState<'any, 'src> {
    functions: EntitiesFromInput<'src, OutputFuncId>,

    // Global variables:
    // - lib_base_id for library base address (import)
    // - existing globals from src module
    // - "store" globals for `modify::constant_extractions`
    // - globals for data segments (lib_base_id + offset)
    globals: EntitiesFromInput<'src, OutputGlobalId>,
    pub global_tmp_store: BTreeMap<StoreType, OutputGlobalId>,
    // extra imports that should be emitted for lib
    // Not available for main module.
    sub_module_extra: Option<SubModuleExtra>,

    // Data Section
    data: GappedMap<DataSegmentId, memory_layout::DataSegmentOutput>,
    //TODO: Remove data_relocations, instead of DataSegmentOutput use SegmentLayout
    data_relocations: SecondaryMap<DataSegmentId, Vec<modify::DataModifyEntry>>,

    // src module
    pub src: &'any InputObject<'src>,
    // Indirect function table Functions from original table that are used in this module.
    pub indirect_functions: IndirectFunctionEmitInfo,
    linkage_type: LinkageType,
    pub linked_modules: Vec<SharedModuleIdentifier>,
    pub incremental_version: BumpVersion,
}

const MEMORY_INDEX: u32 = 0; //TODO: Support multiple memories
impl<'any, 'src> ModuleEmitState<'any, 'src> {
    pub fn produce_state(
        module_info: &'any InputObject<'src>,
        verbose: bool,
        emit_info: &CommonEmitInfo<'src>,
        modinfo: &(SplitModuleIdentifier, split::OutputModuleInfo),
        // deps of current module
        shared_modules: &[SharedModuleIdentifier],
        linkage_type: LinkageType,
        static_symbols: &BTreeSet<SymbolId>,
        nonexported_symbols: &MiniSet<SymbolId>,
        version: BumpVersion,
    ) -> ModuleEmitState<'any, 'src> {
        Split::build_split_object(
            module_info,
            verbose,
            emit_info,
            modinfo,
            shared_modules,
            linkage_type,
            static_symbols,
            nonexported_symbols,
            version,
        )
    }

    // Return got info for a given dep or this module itself.
    pub(crate) fn get_submodule_extra(
        &self,
        shared: Option<&SharedModuleIdentifier>,
    ) -> Option<&GotBase> {
        if let Some(sub_module_extra) = &self.sub_module_extra {
            if let Some(shared) = shared {
                for (module_id, got_base) in &sub_module_extra.extern_modules {
                    if module_id == shared {
                        return Some(got_base);
                    }
                }
            } else {
                return Some(&sub_module_extra.self_base);
            }
        }
        None
    }

    fn is_main(&self) -> bool {
        self.sub_module_extra.is_none()
    }

    fn _num_extra_global_imports(&self) -> usize {
        if !self.is_main() {
            SubModuleExtra::MAIN_GLOBAL_EXPORTS_COUNT as usize + 2
        } else {
            0
        }
    }

    fn generate(
        &'any self,
        computed_modules: &'any ComputedModules<'any, 'src>,
        output_module: &mut wasm_encoder::Module,
        precise_modification: bool,
    ) -> Result<()> {
        self.generate_dylink0_section(output_module)?;
        // Encode type section
        self.generate_type_section(output_module)?;
        self.generate_import_section(computed_modules, output_module);
        self.generate_function_section(output_module);
        if self.is_main() {
            // for submodules this is imported
            self.generate_table_element_sections(output_module)?;
            self.generate_memory_section(output_module);
        }
        self.generate_global_section(output_module)?;
        self.generate_export_section(output_module);
        self.generate_start_function_section(output_module)?;
        self.generate_element_section(output_module)?;

        let code_relocs =
            self.generate_code_section(computed_modules, output_module, precise_modification)?;
        let data_relocs = self.generate_data_section(computed_modules, output_module)?;

        // self.generate_wasm_bindgen_sections(output_module);
        // Names + Linking + Relocations
        self.generate_compiler_tools_sections(output_module, code_relocs, data_relocs)?;
        self.generate_target_features_section(output_module)?;
        self.generate_custom_sections(output_module)?;
        Ok(())
    }

    // TODO: Regenerate function types section (remove unused types)
    fn generate_type_section(&self, output_module: &mut wasm_encoder::Module) -> Result<()> {
        // Simply copy all types.  Unneeded types may be pruned by `wasm-opt`.
        let mut section = wasm_encoder::TypeSection::new();
        // Only use func_types from OutputFunctions
        for (_id, input_func_type) in self.src.wasm_reader.types.iter() {
            let output_func_type: wasm_encoder::FuncType =
                input_func_type.clone().try_into().unwrap();
            section.ty().function(
                output_func_type.params().iter().cloned(),
                output_func_type.results().iter().cloned(),
            );
        }
        output_module.section(&section);
        Ok(())
    }

    fn generate_import_section(
        &self,
        computed_modules: &'any ComputedModules<'any, 'src>,
        output_module: &mut wasm_encoder::Module,
    ) {
        // TODO: Only main module should have original imports.
        // Submodule should contain data + function imports from main module.
        // Additionally import memory + heap + stack globals.

        let mut section = wasm_encoder::ImportSection::new();

        for (index, import_fn) in self.functions.imports() {
            let ty =
                wasm_encoder::EntityType::Function(self.get_function_type(index).index() as u32);
            let fn_name = import_fn.import_name();
            let module_name = import_fn.module_name();
            section.import(&module_name, &fn_name, ty);
        }

        match &self.sub_module_extra {
            None => {
                // Copy all non-function imports from input.
                for (_id, import) in self.src.wasm_reader.imports.iter() {
                    if matches!(
                        import.ty,
                        wasmparser::TypeRef::Func(_) | wasmparser::TypeRef::Global(_)
                    ) {
                        continue;
                    }
                    let ty: wasm_encoder::EntityType = import.ty.try_into().unwrap();
                    section.import(import.module, import.name, ty);
                }
            }

            Some(_) => {
                // Import all globals that are exported from main module.
                for (_, item) in self.globals.imports() {
                    section.import(
                        item.module_name().as_ref(),
                        item.import_name().as_ref(),
                        *item.global_type(),
                    );
                }

                section.import(
                    "__wamex",
                    "__indirect_function_table",
                    computed_modules
                        .main_module
                        .indirect_functions
                        .calculate_indirect_function_table_type(),
                );

                // Import all memories defined by the input module.
                for (memory_index, memory) in self.src.memories.defined_iter() {
                    let ty: wasm_encoder::MemoryType = (*memory).into();
                    section.import("__wamex", self.get_memory_name(memory_index).as_str(), ty);
                }
            }
        }

        output_module.section(&section);
    }

    fn _get_input_func_id(&self, index: OutputFuncId) -> FunctionRef {
        self.functions
            .get_input_id(index)
            .expect("Output function index should be valid")
    }

    fn _get_output_func_id(&self, input_func_id: FunctionRef) -> Option<OutputFuncId> {
        self.functions.get_output_id(input_func_id)
    }

    // Get type of output function by index.
    // TODO: regenerate type section
    fn get_function_type(&self, index: OutputFuncId) -> FuncTypeId {
        let input_func_id = self._get_input_func_id(index);

        self.src.get_function_type_id(input_func_id)
    }
    // Get name of output function by index.
    // Used in generating exports and names section.
    // TODO: Use for generating import sections as well?
    fn get_function_name(&self, index: OutputFuncId, exported: bool) -> Cow<'src, str> {
        let input_func_id = self._get_input_func_id(index);
        let mut name = self
            .src
            .functions
            .names
            .get(input_func_id)
            .map(|name| (*name).into_inner().into())
            .unwrap_or_else(|| format!("func_{index}").into());

        let namespace = exported
            || matches!(
                self.functions
                    .get_defined_for_output_id(index)
                    .map(|def| &def.kind),
                // modify name for import stubs to avoid conflicts
                Some(DefinedFunctionKind::Trampoline { .. })
            );

        if namespace {
            name = format!("__wamex_{}", name).into()
        }
        name
    }

    fn get_global_name(&self, index: GlobalRef) -> Cow<'src, str> {
        self.src
            .globals
            .names
            .get(index)
            .map(|name| (*name).into_inner().into())
            .unwrap_or_else(|| format!("__{index}",).into())
    }

    fn get_memory_name(&self, index: MemoryRef) -> String {
        self.src
            .memories
            .names
            .get(index)
            .map(|name| (*name).into_inner().to_owned())
            .unwrap_or_else(|| format!("__{index}"))
    }
    fn generate_export_section(&self, output_module: &mut wasm_encoder::Module) {
        let mut section = wasm_encoder::ExportSection::new();
        let mut existing_exports = HashSet::<borrow::Cow<'_, str>>::new();
        // left original exports as is (because this module should be drop-in replacement)
        if self.is_main() {
            for (_id, export) in self.src.wasm_reader.exports.iter() {
                let mut index = export.index;
                if export.kind == wasmparser::ExternalKind::Func {
                    let Some(func_id) = self._get_output_func_id(FunctionRef::from_u32(index))
                    else {
                        continue;
                    };
                    index = func_id.index() as u32;
                }
                section.export(export.name, export.kind.into(), index);
                existing_exports.insert(export.name.into());
            }
        }

        for (func_id, func) in self.functions.defined() {
            if !func.export {
                continue;
            }
            let name = self.get_function_name(func_id, true);

            if existing_exports.contains(&name) {
                continue;
            }
            section.export(
                &name,
                wasm_encoder::ExportKind::Func,
                func_id.index() as u32,
            );
        }

        match &self.sub_module_extra {
            Some(extra) => {
                if let Some(export_got_with_id) = &extra.export_got_with_id {
                    let lib_base_name = format!("__{}_lib_base", export_got_with_id);
                    let table_base_name = format!("__{}_table_base", export_got_with_id);
                    if existing_exports.contains(lib_base_name.as_str())
                        || existing_exports.contains(table_base_name.as_str())
                    {
                        panic!(
                            "GOT base globals {lib_base_name} or {table_base_name} already exist in exports"
                        );
                    }
                    // Export GOT base globals.
                    section.export(
                        &lib_base_name,
                        wasm_encoder::ExportKind::Global,
                        extra.self_base.lib_base_id.index() as u32,
                    );
                    section.export(
                        &table_base_name,
                        wasm_encoder::ExportKind::Global,
                        extra.self_base.table_base_id.index() as u32,
                    );
                    existing_exports.insert(lib_base_name.into());
                    existing_exports.insert(table_base_name.into());
                }
            }
            None => {
                // Export globals.
                let white_list = SubModuleExtra::MAIN_GLOBAL_EXPORTS;
                // TODO: export included?
                for (global_index, _) in self.src.globals.defined_iter() {
                    let name = self.get_global_name(global_index);
                    if existing_exports.contains(&name) {
                        continue;
                    }
                    if !white_list.contains(&&*name) {
                        continue;
                    }
                    // TODO: fix global id?
                    section.export(
                        &name,
                        wasm_encoder::ExportKind::Global,
                        global_index.index() as u32,
                    );
                    existing_exports.insert(name);
                }

                white_list.iter().for_each(|name| {
                    debug_assert!(
                        existing_exports.contains(*name),
                        "Main module should export {name}"
                    );
                });

                if !existing_exports.contains("__indirect_function_table") {
                    section.export(
                        "__indirect_function_table",
                        wasm_encoder::ExportKind::Table,
                        0,
                    );
                }
            }
        }

        output_module.section(&section);
    }

    fn find_void_type(&self) -> FuncTypeId {
        for (fn_id, fn_type) in self.src.wasm_reader.types.iter() {
            if fn_type.params().is_empty() && fn_type.results().is_empty() {
                return fn_id;
            }
        }

        panic!("Void type not found in type section");
    }
    fn generate_function_section(&self, output_module: &mut wasm_encoder::Module) {
        let mut section: wasm_encoder::FunctionSection = wasm_encoder::FunctionSection::new();
        for (index, _func) in self.functions.defined() {
            let func_type = self.get_function_type(index);
            section.function(func_type.index() as u32);
        }
        // add start function
        if !self.is_main() {
            section.function(self.find_void_type().index() as u32);
        }

        output_module.section(&section);
    }

    // only for main
    fn generate_table_element_sections(
        &self,
        output_module: &mut wasm_encoder::Module,
    ) -> Result<()> {
        let mut section = wasm_encoder::TableSection::new();
        section.table(
            self.indirect_functions
                .calculate_indirect_function_table_type(),
        );
        output_module.section(&section);
        Ok(())
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
    fn _function_ids_for_element_section(&self) -> Result<Vec<u32>> {
        let func_ids: Vec<u32> = self
            .indirect_functions
            .table_entries
            .iter()
            .map(|input_func_id| -> Result<u32> {
                let output_func_id = self._get_output_func_id(*input_func_id).ok_or_else(|| {
                    anyhow!("No output function corresponding to input function {input_func_id:?}")
                })?;
                Ok(output_func_id.index() as u32)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(func_ids)
    }

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
    fn generate_element_section(&self, output_module: &mut wasm_encoder::Module) -> Result<()> {
        let mut section = wasm_encoder::ElementSection::new();

        let element_start = if let Some(sub_module_extra) = &self.sub_module_extra {
            wasm_encoder::ConstExpr::global_get(
                sub_module_extra.self_base.table_base_id.index() as u32
            )
        } else {
            wasm_encoder::ConstExpr::i32_const(1_i32) // skip empty entry at index 0 for main module
        };

        let func_ids = self._function_ids_for_element_section()?;
        Self::_generate_element_section_segment(&mut section, &element_start, func_ids);

        // generate empty entries for lazy entrypoints
        match &self.sub_module_extra {
            None => {
                fn find_abort_function(
                    functions: &EntitiesFromInput<'_, OutputFuncId>,
                ) -> Option<OutputFuncId> {
                    for (id, _def) in functions.defined() {
                        return Some(id); // TODO: Place real abort function
                    }
                    None
                }
                let id = find_abort_function(&self.functions).expect("Abort function not found");

                let abort_fn_id = id.index() as u32;
                let num_lazy_entries = self.indirect_functions.num_extra_stubs;
                let start_of_lazy_fns = self.indirect_functions.table_entries.len() as i32 + 1;

                let stub_vec = vec![abort_fn_id; num_lazy_entries as usize];
                let element_start = wasm_encoder::ConstExpr::i32_const(start_of_lazy_fns);
                Self::_generate_element_section_segment(&mut section, &element_start, stub_vec);
            }
            Some(sub_module) => {
                if let LinkageType::DynamicLinking { table_offset, .. } = &self.linkage_type {
                    let entry_point_offset = *table_offset as i32;

                    let lazy_entrypoints = sub_module
                        .entrypoints
                        .iter()
                        .map(|input_func_id| {
                            let output_func_id = self
                                ._get_output_func_id(*input_func_id)
                                .expect("Function should be defined");
                            output_func_id.index() as u32
                        })
                        .collect::<Vec<_>>();
                    let element_start = wasm_encoder::ConstExpr::i32_const(entry_point_offset);

                    Self::_generate_element_section_segment(
                        &mut section,
                        &element_start,
                        lazy_entrypoints,
                    );
                }
            }
        }
        output_module.section(&section);
        Ok(())
    }

    fn generate_memory_section(&self, output_module: &mut wasm_encoder::Module) {
        if self.src.wasm_reader.memories.is_empty() {
            return;
        }
        let mut section = wasm_encoder::MemorySection::new();
        for (_idx, memory) in self.src.wasm_reader.memories.iter() {
            section.memory((*memory).into());
        }
        output_module.section(&section);
    }

    fn generate_global_section(&self, output_module: &mut wasm_encoder::Module) -> Result<()> {
        let mut section = wasm_encoder::GlobalSection::new();
        for (_, global) in self.globals.defined() {
            match global {
                DefinedGlobal::PlainCopy { global, .. } => {
                    section.global(
                        global.ty.try_into().unwrap(),
                        &global.init_expr.clone().try_into().unwrap(),
                    );
                }
                DefinedGlobal::WithConstructor(global_type) => {
                    if self.is_main() {
                        bail!("Trying to define global for main module");
                    }
                    section.global(
                        *global_type,
                        &globals::global_init_tmp(global_type.val_type),
                    );
                }
            }
        }
        output_module.section(&section);
        Ok(())
    }

    fn generate_start_function_section(
        &'any self,
        output_module: &mut wasm_encoder::Module,
    ) -> Result<()> {
        if !self.is_main() {
            let start = wasm_encoder::StartSection {
                function_index: self.functions.len() as u32,
            };
            output_module.section(&start);
        }
        Ok(())
    }

    // func [param i32 i32 ...] (result i32):
    // local.get 0
    // local.get 1
    // ...
    // i32.const table_index
    // call_indirect (type type_id) (table 0)
    // end
    fn _generate_indirect_stub_function(
        &'any self,
        section: &mut wasm_encoder::CodeSection,
        input_func_id: FunctionRef,
        table_index: u32,
    ) -> Result<Vec<RelocationEntry>> {
        let func_type_id = &self.src.get_function_type_id(input_func_id);
        let func_type = &self.src.wasm_reader.types[*func_type_id];

        let mut func = wasm_encoder::Function::new([]);
        for (param_i, _param_type) in func_type.params().iter().enumerate() {
            func.instruction(&wasm_encoder::Instruction::LocalGet(param_i as u32));
        }
        func.instruction(&wasm_encoder::Instruction::I32Const(table_index as i32));
        func.instruction(&wasm_encoder::Instruction::CallIndirect {
            type_index: func_type_id.index() as u32,
            table_index: 0, // __indirect_function_table // TODO: support multiple tables
        });
        func.instruction(&wasm_encoder::Instruction::End);
        section.function(&func);
        // TODO: Add relocations for call_indirect
        Ok(vec![])
    }

    // Import fns can't be exported directy because wasm convert their type to externrefs.
    // So instead generate stub function that will call the imported function.

    // func [param i32 i32 ...] (result i32):
    // local.get 0
    // local.get 1
    // ...
    // call import_func_index
    // end
    fn _generate_import_call_stub(
        &'any self,
        section: &mut wasm_encoder::CodeSection,
        input_func_id: FunctionRef,
    ) -> Result<Vec<RelocationEntry>> {
        let func_type_id = &self.src.get_function_type_id(input_func_id);
        let func_type = &self.src.wasm_reader.types[*func_type_id];

        let import_fn = self
            ._get_output_func_id(input_func_id)
            .expect("Imported function should have output id");

        let mut func = wasm_encoder::Function::new([]);
        for (param_i, _param_type) in func_type.params().iter().enumerate() {
            func.instruction(&wasm_encoder::Instruction::LocalGet(param_i as u32));
        }
        func.instruction(&wasm_encoder::Instruction::Call(import_fn.index() as u32));
        func.instruction(&wasm_encoder::Instruction::End);
        section.function(&func);
        // TODO: Add relocations for call/type_ids
        Ok(vec![])
    }

    fn _generate_defined_function(
        &'any self,
        section: &mut wasm_encoder::CodeSection,
        computed_modules: &'any ComputedModules<'any, 'src>,
        function_start_offset: usize,
        input_func_id: FunctionRef,
        modification_list: &[modify::CodeModifyEntry],
        precise_modification: bool,
    ) -> Result<Vec<RelocationEntry>> {
        let mut code_relocs = Vec::new();
        let defined_id = self
            .src
            .as_defined_function_id(input_func_id)
            .expect("Defined function expected");

        let global_id_mapper = |global_id: GlobalRef| self.globals.get_output_id(global_id);

        let modify_fn = if precise_modification {
            ModifyContext::emit_code_with_changes
        } else {
            ModifyContext::emit_code_in_place
        };

        let (result, modified_relocs) = modify_fn(
            self,
            computed_modules,
            global_id_mapper,
            defined_id,
            input_func_id,
            modification_list,
        )?;
        for mut reloc in modified_relocs {
            reloc.offset += function_start_offset as u32;
            code_relocs.push(reloc);
        }
        section.raw(&result);

        Ok(code_relocs)
    }

    fn generate_code_section(
        &'any self,
        computed_modules: &'any ComputedModules<'any, 'src>,
        output_module: &mut wasm_encoder::Module,
        precise_modification: bool,
    ) -> Result<Vec<RelocationEntry>> {
        let defined_functions_count = self.functions.defined().len() as u32
            + if !self.is_main() {
                1 // start function
            } else {
                0
            };

        let mut section = wasm_encoder::CodeSection::new();
        let mut code_relocs = Vec::new();
        for (_id, output_func) in self.functions.defined() {
            let relocs = match &output_func.kind {
                DefinedFunctionKind::Trampoline {} => {
                    self._generate_import_call_stub(&mut section, output_func.input_func_id)
                }
                DefinedFunctionKind::IndirectTrampoline { table_index_offset } => self
                    ._generate_indirect_stub_function(
                        &mut section,
                        output_func.input_func_id,
                        computed_modules.indirect_entrypoints_offset() + *table_index_offset,
                    ),
                DefinedFunctionKind::Copied { modification_list } => {
                    let function_start_offset =
                        encoding_size(defined_functions_count) + section.byte_len();
                    self._generate_defined_function(
                        &mut section,
                        computed_modules,
                        function_start_offset,
                        output_func.input_func_id,
                        modification_list,
                        precise_modification,
                    )
                }
            };

            code_relocs.extend(relocs?);
        }

        if self.sub_module_extra.is_some() {
            let relocate = RelocateState {
                input_module: self.src,
                computed_modules,
                emit_module: self,
                global_id_mapper: &|global_id: GlobalRef| self.globals.get_output_id(global_id),
            };

            let start_fn = StartFnGen::new(
                relocate,
                MEMORY_INDEX,
                self.data_relocations
                    .iter()
                    .flat_map(|(_, entries)| entries.iter()),
            )?;

            section.function(&start_fn.generate_fn());
        }
        output_module.section(&section);

        Ok(code_relocs)
    }
    fn generate_data_section(
        &'any self,
        computed_modules: &'any ComputedModules<'any, 'src>,
        output_module: &mut wasm_encoder::Module,
    ) -> Result<Vec<RelocationEntry>> {
        // TODO: Add shifter relocs
        let relocs = Vec::new();
        let mut section = wasm_encoder::DataSection::new();

        for (id, out) in self.data.iter() {
            let mut data = out.data_segment(MEMORY_INDEX);
            // Skip empty data segments
            // if data.data.is_empty() {
            //     continue;
            // }
            if let Some(relocs) = self.data_relocations.get(id) {
                for entry in relocs.iter() {
                    let state = modify::StartFnModifyContext {
                        data_segment: &mut data.data,
                        relocate: RelocateState {
                            input_module: self.src,
                            computed_modules,
                            emit_module: self,
                            global_id_mapper: &|global_id: GlobalRef| {
                                self.globals.get_output_id(global_id)
                            },
                        },
                    };
                    state.apply_relocation(entry)?;
                }
            }
            section.segment(data);
        }

        output_module.section(&section);
        Ok(relocs)
    }
    fn generate_target_features_section(
        &self,
        output_module: &mut wasm_encoder::Module,
    ) -> Result<()> {
        let mut features = self.src.wasm_reader.target_features.clone();
        features.features.extended_const = true;
        output_module.section(&features.encode_custom_section());
        Ok(())
    }

    fn generate_dylink0_section(
        &'any self,
        output_module: &mut wasm_encoder::Module,
    ) -> Result<()> {
        if !self.is_main() {
            let data = Dylink0Section {
                memory_alignment: std::mem::size_of::<u32>() as u32, // as power of 2
                memory_size: self
                    .data
                    .iter()
                    .last()
                    .map(|(_, seg)| seg.memory_offset() + seg.as_raw().len())
                    .unwrap_or_default() as u32,
                table_size: self.indirect_functions.table_entries.len() as u32,
                table_alignment: 0,
                needed_libraries: self
                    .linked_modules
                    .iter()
                    .map(|m| m.to_string().into())
                    .collect(),

                //TODO: calculate imports of deps.
                import_info: vec![],
            };
            let section = wasm_encoder::CustomSection {
                name: "dylink.0".into(),
                data: data.encode_section().into(),
            };
            output_module.section(&section);
        }
        Ok(())
    }

    // linking| names
    fn generate_compiler_tools_sections(
        &self,
        output_module: &mut wasm_encoder::Module,
        _shifted_code_relocs: Vec<RelocationEntry>,
        _shifted_data_relocs: Vec<RelocationEntry>,
    ) -> Result<()> {
        let wamex_version = wasm_encoder::CustomSection {
            name: "__wamex_version".into(),
            data: self.incremental_version.encode().to_vec().into(),
        };

        output_module.section(&wamex_version);

        let mut functions = wasm_encoder::NameMap::new();
        for output_id in self.functions.iter_all_ids() {
            let name = self.get_function_name(output_id, false);

            functions.append(output_id.index() as u32, &name);
        }

        let mut names = wasm_encoder::NameSection::new();
        names.functions(&functions);
        output_module.section(&names.as_custom());

        // let mut section = wasm_encoder::CustomSection::new("linking");
        // section.data(&self.info.source.linking);
        // output_module.section(&section);
        // dbg!(&self.info.source.names);
        // dbg!(&self.info.source.linking);
        // dbg!(&self.info.source.relocs);
        Ok(())
    }
    // wasm-bindgen
    // other whitelisted
    fn generate_custom_sections(&self, output_module: &mut wasm_encoder::Module) -> Result<()> {
        for (_, custom) in &self.src.wasm_reader.custom_sections {
            match &*custom.name {
                "__wasm_bindgen_unstable" => {
                    if !self.is_main() {
                        continue; // print only on main module
                    }
                }
                _ => {
                    log::warn!(
                        "Skipping unsuported custom section during emit: {}",
                        custom.name
                    );
                    continue;
                }
            };
            let section = wasm_encoder::CustomSection {
                name: (&*custom.name).into(),
                data: (&*custom.data).into(),
            };
            output_module.section(&section);
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct IndirectFunctionEmitInfo {
    pub table_entries: Vec<FunctionRef>,
    pub function_table_index: HashMap<FunctionRef, usize>,
    pub num_extra_stubs: u64,
}

impl IndirectFunctionEmitInfo {
    fn new(num_extra_stubs: Option<u64>, table_entries: Vec<FunctionRef>) -> Self {
        // main module has 1 stub at start
        let num_stub_at_start = if num_extra_stubs.is_some() { 1 } else { 0 };
        let function_table_index: HashMap<_, _> = table_entries
            .iter()
            .enumerate()
            .map(|(i, func_id)| (*func_id, i + num_stub_at_start))
            .collect();

        Self {
            table_entries,
            function_table_index,
            num_extra_stubs: num_extra_stubs.unwrap_or(0),
        }
    }
    fn calculate_indirect_function_table_type(&self) -> wasm_encoder::TableType {
        // + 1 due to empty entry at index 0
        let indirect_table_size = self.table_entries.len() as u64 + 1 + self.num_extra_stubs; // reserve space for stubs at start

        wasm_encoder::TableType {
            element_type: wasm_encoder::RefType::FUNCREF,
            minimum: indirect_table_size,
            maximum: None, //Some(indirect_table_size as u64), // TODO: limit?
            shared: false,
            table64: false,
        }
    }
}

#[derive(Debug)]
pub struct ModuleDecl {
    pub split_points: Vec<SplitPoint>,

    // offset in indirect_function table where this module's entrypoints start
    split_points_offset: u32,
}

#[derive(Debug)]
pub struct CommonEmitInfo<'src> {
    pub src_data_segments: GappedMap<DataSegmentId, SegmentLayout<'src>>,

    // Imports (corresponding to split points) to exclude from all modules.
    pub split_point_imports: BTreeSet<FunctionRef>,
    pub modules_decl: HashMap<ModuleIdentifier, ModuleDecl>,
}

impl<'src> CommonEmitInfo<'src> {
    // Return range in indirect_function table corresponding to module.
    // Use stubs_start offset to convert to final table indexes.
    // Returns None if module is not found.
    fn module_entrypoints_range_shifted(
        &self,
        stubs_start: u32,
        module_id: &ModuleIdentifier,
    ) -> Option<Range<u32>> {
        self.modules_decl.get(module_id).map(|r| {
            let start = stubs_start + r.split_points_offset;
            let end = stubs_start + r.split_points_offset + r.split_points.len() as u32;
            start..end
        })
    }
    fn external_entrypoint_index(&self, entrypoint_func: FunctionRef) -> Option<u32> {
        self.modules_decl.values().find_map(|module| {
            module
                .split_points
                .iter()
                .position(|sp| sp.import_func() == entrypoint_func)
                .map(|pos| module.split_points_offset + pos as u32)
        })
    }

    // Checks if given import function is an entrypoint for any module.
    fn is_external_entrypoint(&self, import_fn: &FunctionRef) -> bool {
        self.split_point_imports.contains(import_fn)
    }

    fn num_entrypoints(&self) -> u64 {
        self.split_point_imports.len() as u64
    }

    pub fn new(
        module: &InputObject<'src>,
        verbose: bool,
        program_info: &SplitProgramInfo,
    ) -> Result<Self> {
        let mut split_point_imports = BTreeSet::new();
        let mut modules_decl = HashMap::new();
        for (module_index, (id, output_module)) in program_info.output_modules.iter().enumerate() {
            let SplitModuleIdentifier::Single(id) = &id else {
                debug_assert!(
                    output_module.split_points.is_empty(),
                    "Expected no split points on shared module"
                );
                continue;
            };
            modules_decl.insert(
                id.clone(),
                ModuleDecl {
                    split_points: output_module.split_points.clone(),
                    split_points_offset: module_index as u32,
                },
            );

            for split_point in output_module.split_points.iter() {
                split_point_imports.insert(split_point.import_func());
            }
        }

        // re-build data_segments (using only available symbols)
        let data_segments_symbols = Self::chunk_by(
            module.symbols.iter_data_symbols(),
            |(left_segment, ..), (right_segment, ..)| left_segment == right_segment,
        );
        let data_segments: GappedMap<DataSegmentId, SegmentLayout<'src>> = module
            .wasm_reader
            .data
            .section_payload
            .data_segments
            .iter()
            .map(|(data_segment, data)| {
                let data_symbols = data_segments_symbols
                    .get(data_segment.index())
                    .cloned()
                    .expect("Symbols for data segment not found");
                let segment_info = &module.wasm_reader.linking.segments_info[data_segment.index()];

                let layout = SegmentLayout::new_inner(
                    data,
                    segment_info,
                    data_symbols.into_iter().map(|(_, id, record)| (id, record)),
                );
                layout.map(|l| (data_segment, l))
            })
            .collect::<Result<GappedMap<DataSegmentId, SegmentLayout<'src>>>>()?;

        if verbose {
            SegmentLayout::debug_layout(&module.symbols, String::from("input"), &data_segments);
        }
        Ok(CommonEmitInfo {
            split_point_imports,
            src_data_segments: data_segments,
            modules_decl,
        })
    }

    fn chunk_by<F, U>(items: impl Iterator<Item = U>, comparator: F) -> Vec<Vec<U>>
    where
        F: Fn(&U, &U) -> bool,
    {
        let mut result = Vec::new();
        let mut current_chunk = Vec::new();

        for item in items {
            if let Some(prev) = current_chunk.last() {
                if !comparator(prev, &item) {
                    result.push(current_chunk);
                    current_chunk = Vec::new();
                }
            }
            current_chunk.push(item);
        }

        if !current_chunk.is_empty() {
            result.push(current_chunk);
        }

        result
    }
}

const MAIN_ID: SplitModuleIdentifier = SplitModuleIdentifier::Single(ModuleIdentifier::Main);

struct ComputedModules<'a, 'src> {
    main_module: ModuleEmitState<'a, 'src>,
    shared_modules: BTreeMap<SharedModuleIdentifier, ModuleEmitState<'a, 'src>>,
    sub_modules: BTreeMap<ModuleIdentifier, ModuleEmitState<'a, 'src>>,
}

impl<'a, 'src> ComputedModules<'a, 'src> {
    pub fn produce_state(
        common_emit_info: &'a CommonEmitInfo<'src>,
        verbose: bool,
        module: &'a InputObject<'src>,
        program_info: &SplitProgramInfo,
        version: BumpVersion,
        nonexported_symbols: &'a MiniSet<SymbolId>,
    ) -> Result<Self> {
        // main module defined symbols
        let static_symbols = program_info
            .output_modules
            .iter()
            .find_map(|(id, output_module)| {
                if *id == MAIN_ID {
                    Some(&output_module.defined_symbols)
                } else {
                    None
                }
            })
            .expect("Main module not found");

        let modules_ids_iter = program_info
            .output_modules
            .iter()
            .enumerate()
            .map(|(output_module_index, (id, _))| (output_module_index, id.clone()));

        for (id, output_module) in program_info.output_modules.iter() {
            let SplitModuleIdentifier::Shared(_) = id else {
                continue;
            };
            log::debug!("Shared_modules_info {id:?}: {output_module:?}");
        }

        const NO_DEPS: Vec<SharedModuleIdentifier> = Vec::new();
        let all_shared_deps = modules_ids_iter
            .clone()
            .filter_map(|(_output_module_index, id)| {
                if let SplitModuleIdentifier::Shared(shared_with) = id {
                    Some(shared_with)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        let dyn_linkage = true; // TODO: from args

        let main_module = modules_ids_iter
            .clone()
            .into_iter()
            .find_map(|(output_module_index, id)| {
                if id == MAIN_ID {
                    Some((output_module_index, id))
                } else {
                    None
                }
            })
            .map(|(output_module_index, id)| {
                log::info!("Calculating module: {id}");
                let linkage_type = if dyn_linkage {
                    // Main module has no entrypoints.
                    LinkageType::DynamicLinking {
                        table_offset: 0,
                        table_num_entrypoints: 0,
                    }
                } else {
                    LinkageType::OriginalLayout
                };

                (
                    ModuleEmitState::produce_state(
                        module,
                        verbose,
                        common_emit_info,
                        &program_info.output_modules[output_module_index],
                        &NO_DEPS,
                        linkage_type,
                        static_symbols,
                        nonexported_symbols,
                        version,
                    ),
                    id,
                )
            })
            .expect("Main module not found");

        let all_sub_modules = modules_ids_iter
            .into_iter()
            .filter(|(_output_module_index, id)| *id != MAIN_ID)
            .map(|(output_module_index, id)| {
                log::info!("Calculating module: {id}");
                let stubs_start = main_module.0.indirect_functions.table_entries.len() + 1;

                let table_range = if let SplitModuleIdentifier::Single(id) = &id {
                    common_emit_info
                        .module_entrypoints_range_shifted(stubs_start as u32, id)
                        .expect("Module split points not found")
                } else {
                    0..0
                };

                let linkage_type = if dyn_linkage {
                    LinkageType::DynamicLinking {
                        table_offset: table_range.start,
                        table_num_entrypoints: table_range.len() as u32,
                    }
                } else {
                    LinkageType::OriginalLayout
                };

                let module_deps = id.collect_deps(&all_shared_deps);
                (
                    ModuleEmitState::produce_state(
                        module,
                        verbose,
                        common_emit_info,
                        &program_info.output_modules[output_module_index],
                        &module_deps,
                        linkage_type,
                        static_symbols,
                        nonexported_symbols,
                        version,
                    ),
                    id,
                )
            })
            .collect::<Vec<_>>();
        let mut sub_modules = BTreeMap::new();
        let mut shared_modules = BTreeMap::new();
        for (state_res, id) in all_sub_modules {
            match id {
                SplitModuleIdentifier::Single(id) => {
                    sub_modules.insert(id, state_res);
                }
                SplitModuleIdentifier::Shared(shared_with) => {
                    shared_modules.insert(shared_with, state_res);
                }
            }
        }

        Ok(Self {
            main_module: main_module.0,
            shared_modules,
            sub_modules,
        })
    }
    fn indirect_entrypoints_offset(&self) -> u32 {
        // +1 for empty first entry
        self.main_module.indirect_functions.table_entries.len() as u32 + 1
    }

    fn iter_modules(
        &self,
    ) -> impl Iterator<Item = (SplitModuleIdentifier, &ModuleEmitState<'a, 'src>)> {
        let shared_iters = self
            .shared_modules
            .iter()
            .map(|(id, state)| (SplitModuleIdentifier::Shared(id.clone()), state));
        let single_iters = self
            .sub_modules
            .iter()
            .map(|(id, state)| (SplitModuleIdentifier::Single(id.clone()), state));
        let main_iter = std::iter::once((MAIN_ID, &self.main_module));
        main_iter.chain(single_iters).chain(shared_iters)
    }

    fn emit_modules(
        &self,
        precise_modification: bool,
        whitelist: Option<&BTreeSet<SplitModuleIdentifier>>,
        mut emit_fn: impl FnMut(&SplitModuleIdentifier, &[u8]) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        for (identifier, state) in self.iter_modules() {
            if let Some(whitelist) = whitelist {
                if !whitelist.contains(&identifier) {
                    log::info!("Skipping module {identifier} as not in whitelist");
                    continue;
                }
            }
            log::info!("Generating module {identifier}");

            let mut encoder = wasm_encoder::Module::new();
            state
                .generate(self, &mut encoder, precise_modification)
                .with_context(|| format!("Error generating {:?}", identifier))?;

            emit_fn(&identifier, encoder.as_slice())
                .with_context(|| format!("Error emitting {:?}", identifier))?;
        }
        Ok(())
    }
}

// Merge modules that shared with main module into main itself.
pub fn merge_main_shared(program_info: &mut SplitProgramInfo) {
    let (shared_with_main, mut other): (Vec<_>, Vec<_>) =
        std::mem::take(&mut program_info.output_modules)
            .into_iter()
            .partition(|(id, _)| {
                if let SplitModuleIdentifier::Shared(shared_with) = id {
                    shared_with.contains(&ModuleIdentifier::Main)
                } else {
                    false
                }
            });

    // split iter at 3 parts: before main, main, after main
    let (left_to_main, main_module, right_to_main) = {
        let main_module_index = other
            .iter()
            .enumerate()
            .find(|(_, (id, _))| *id == MAIN_ID)
            .expect("Main module not found")
            .0;
        let (before, main_and_next) = other.split_at_mut(main_module_index);
        let (main_module, after) = main_and_next.split_at_mut(1);
        let main_module = &mut main_module[0].1;
        (before, main_module, after)
    };

    // check import in all remain modules except main
    let is_imported_by_other = |node: &SymbolId| {
        left_to_main
            .iter()
            .chain(right_to_main.iter())
            .any(|(_, mod_state)| mod_state.imports.contains(node))
            || right_to_main
                .iter()
                .any(|(_, mod_state)| mod_state.imports.contains(node))
    };

    #[cfg(debug_assertions)]
    let mut check_imports = vec![];

    for (id, mut shared_module) in shared_with_main {
        debug_assert!(shared_module.split_points.is_empty());

        for node in &shared_module.exports {
            // it was exported in shared module, so on main side it had been imported.
            // remove from main link symbols.
            if !main_module.imports.remove(node) {
                log::trace!(
                    "Shared module symbol not found in main: {node:?}. It probably was removed in other shared entry."
                );
            }
            // This was imported not only by main, so export is needed.
            if is_imported_by_other(node) {
                main_module.exports.insert(*node);
            }
        }

        // imported modules should already be in main
        #[cfg(debug_assertions)]
        for node in &shared_module.imports {
            check_imports.push(*node);
        }
        log::trace!(
            "extending main defined symbols with shared ({id:?}): {:?}",
            shared_module.defined_symbols
        );

        main_module
            .defined_symbols
            .extend(std::mem::take(&mut shared_module.defined_symbols));
    }

    debug_assert!(main_module.imports.is_empty());
    #[cfg(debug_assertions)]
    for node in check_imports {
        assert!(
            main_module.defined_symbols.contains(&node),
            "Shared module import not found in main defined symbols: {node:?}"
        );
    }

    program_info.output_modules = std::mem::take(&mut other);
}

pub fn emit_modules<'a, 'src>(
    module: &'a InputObject<'src>,
    verbose: bool,
    program_info: &SplitProgramInfo,
    wbg_fns: &MiniSet<SymbolId>,
    precise_modification: bool,
    whitelist: Option<&BTreeSet<SplitModuleIdentifier>>,
    version: BumpVersion,
    emit_fn: impl FnMut(&SplitModuleIdentifier, &[u8]) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let emit_info = CommonEmitInfo::new(module, verbose, program_info)?;
    let calculated = ComputedModules::produce_state(
        &emit_info,
        verbose,
        module,
        program_info,
        version,
        &wbg_fns,
    )
    .context("Error calculating modules")?;
    calculated.emit_modules(precise_modification, whitelist, emit_fn)?;
    Ok(())
}
