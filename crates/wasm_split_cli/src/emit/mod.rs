use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::identity;
use std::ops::Range;

use anyhow::{anyhow, bail, Context, Result};
use globals::GlobalConstructor;
use wasm_encoder::GlobalType;
use wasmparser::{RelocationEntry, RelocationType, TypeRef};

use crate::analysis;
use crate::analysis::split_point::{ModuleIdentifier, SplitModuleIdentifier};
use crate::helpers::iter_if;
use crate::index::{
    DataId, DataSegmentId, FuncTypeId, GlobalId, IdVec, MemoryId, OutputFuncId, OutputGlobalId,
    OutputSymbolDataId,
};
use crate::read::linking::SymbolIndex;
use modify::{init_each_store_var, ModifyContext, StoreType};

use crate::{
    analysis::dep_graph::DepNode,
    analysis::split_point::SplitProgramInfo,
    index::{ImportId, InputFuncId},
    read::InputModule,
};
pub use data_segments::{DataSegment, DataSegmentOutput};
use modify::GlobalVar;

mod data_segments;
mod globals;

mod modify;

#[derive(Debug, Clone, PartialEq, Eq)]
struct DefinedFunction {
    input_func_id: InputFuncId,
    export: bool,
    // List of modifications that should be applied to this function.
    modification_list: Vec<modify::ModifyEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ImportedFunction<'a> {
    input_func_id: InputFuncId,
    kind: ImportFunctionKind<'a>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum ImportFunctionKind<'a> {
    // use existing import function
    Existing {
        module_name: &'a str,
    },
    // Add new import function from another module (e.g. main module).
    New {
        link_module: usize,
        output_function_index: usize,
    },
}

impl ImportedFunction<'_> {
    pub fn input_func_id(&self) -> InputFuncId {
        self.input_func_id
    }
    pub fn module_name(&self) -> String {
        match self.kind {
            ImportFunctionKind::Existing { module_name, .. } => module_name.to_string(),
            ImportFunctionKind::New { .. } => {
                "__wasm_split".to_string()
                // format!("__wasm_split_link_{}", link_module)
            }
        }
    }
}

enum Global<'a> {
    PlainCopy(wasmparser::Global<'a>),
    WithConstructor(GlobalConstructor),
}

struct ExtraImportGlobal {
    global_id: u32,
    global_type: wasm_encoder::GlobalType,
}

// ignore relocations field in order
impl Ord for DefinedFunction {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.input_func_id.cmp(&other.input_func_id) {
            std::cmp::Ordering::Equal => self.export.cmp(&other.export),
            ord => return ord,
        }
    }
}
impl PartialOrd for DefinedFunction {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

// 'any are used because associated types are invariant, and used in default impls for Indexed Vec/Map impls.
pub struct ModuleEmitState<'any, 'src> {
    // Function section
    import_functions: Vec<ImportedFunction<'src>>,
    defined_functions: Vec<DefinedFunction>,
    // Global variables:
    // - lib_base_id for library base address (import)
    // - existing globals from src module
    // - "store" globals for `modify::constant_extractions`
    // - globals for data segments (lib_base_id + offset)
    globals: Vec<Global<'src>>,
    pub global_tmp_store: HashMap<StoreType, OutputGlobalId>,
    // extra imports that should be emitted for lib
    // Not available for main module.
    lib_base_import: Option<ExtraImportGlobal>,

    // Data Section
    data: IdVec<data_segments::DataSegmentOutput>,

    pub emit_info: &'any EmitInfo,
    // src module
    pub info: &'any analysis::ModuleInfo<'src>,
    // Generated fields:
    // Fields that calculated from other fields, and should be updated after any change.
    pub input_function_output_id: HashMap<InputFuncId, usize>,
    pub input_data_to_output_id: HashMap<DataId, (DataSegmentId, OutputSymbolDataId)>,
    pub indirect_function_table_range: Range<usize>,
}

impl<'any, 'src> ModuleEmitState<'any, 'src> {
    pub fn produce_state(
        module_info: &'any analysis::ModuleInfo<'src>,
        data_segments: &'any IdVec<data_segments::DataSegment<'src>>,
        emit_info: &'any EmitInfo,
        program_info: &SplitProgramInfo,
        output_module_index: usize,
    ) -> ModuleEmitState<'any, 'src> {
        let output_module_info = &program_info.output_modules[output_module_index].1;

        log::debug!("output_module_info: {output_module_info:#?}");
        // We need to include definitions for all of the `included_symbols`.
        let mut funcs_to_define = HashSet::<(InputFuncId, bool)>::new();
        let mut import_functions = Vec::new();

        let main_module = output_module_index == 0;

        let shared_imports = program_info
            .output_modules
            .iter()
            .filter_map(|(id, output_module)| {
                if let SplitModuleIdentifier::Shared(_) = id {
                    Some(output_module.included_symbols.iter())
                } else {
                    None
                }
            })
            .flatten();

        let mut used_funcs = HashSet::new();
        for func_id in output_module_info
            .included_symbols
            .iter()
            .chain(iter_if(main_module, shared_imports.clone()))
            .filter_map(DepNode::as_function)
        {
            if used_funcs.contains(&func_id) {
                continue;
            }
            used_funcs.insert(func_id);
            if let Some(import_id) = module_info.get_function_import_id(func_id) {
                import_functions.push(ImportedFunction {
                    input_func_id: func_id,
                    kind: ImportFunctionKind::Existing {
                        module_name: module_info.source.imports[import_id].module,
                    },
                });
            } else {
                funcs_to_define.insert((func_id, false));
            }
        }

        if !main_module {
            // submodule imports needed function from main module.
            import_functions.extend(
                output_module_info
                    .link_symbols
                    .iter()
                    .filter_map(DepNode::as_function)
                    .map(|func_id| ImportedFunction {
                        input_func_id: func_id,
                        kind: ImportFunctionKind::New {
                            link_module: 0,
                            output_function_index: 0,
                        },
                    }),
            );
        }

        let mut globals: Vec<_> = module_info
            .source
            .globals
            .iter()
            .map(|(_id, global)| Global::PlainCopy(global.clone()))
            .collect();

        let mut num_global_imports = module_info
            .source
            .imports
            .iter()
            .filter(|(_id, v)| matches!(v.ty, wasmparser::TypeRef::Global(_)))
            .count();
        let lib_base_id = num_global_imports as u32;
        let lib_base_import = if !main_module {
            num_global_imports += 1; // for lib_base_id
            Some(ExtraImportGlobal {
                global_id: lib_base_id,
                global_type: wasm_encoder::GlobalType {
                    val_type: wasm_encoder::ValType::I32,
                    mutable: false,
                    shared: false,
                },
            })
        } else {
            None
        };

        let mut data_to_define = BTreeMap::new();
        for i in output_module_info
            .included_symbols
            .iter()
            .filter_map(|dep| match dep {
                DepNode::DataSymbol(data_segment, data_id) => Some((*data_segment, *data_id)),
                _ => None,
            })
        {
            data_to_define
                .entry(i.0)
                .or_insert_with(HashSet::new)
                .insert(i.1);
        }

        // Include all shared data segments also.
        if main_module {
            for i in shared_imports.filter_map(DepNode::as_data_symbol) {
                data_to_define
                    .entry(i.0)
                    .or_insert_with(HashSet::new)
                    .insert(i.1);
            }
        }

        // filter only used entries
        let data_segments = data_segments
            .iter()
            .map(|(data_segment_id, data)| {
                let empty = HashSet::new();
                let entries = data_to_define.get(&data_segment_id).unwrap_or(&empty);
                let mut data_segment = data.clone();
                data_segment.retain_symbols(entries);
                data_segment
            })
            .collect::<IdVec<_>>();

        let mut global_tmp_store = HashMap::new();
        if !main_module {
            for (store_type, val_type) in init_each_store_var() {
                let global_id = globals.len() + num_global_imports;
                global_tmp_store.insert(store_type, global_id);
                globals.push(Global::WithConstructor(GlobalConstructor::TempStore(
                    GlobalType {
                        val_type,
                        mutable: true,
                        shared: false,
                    },
                )));
            }
        }

        let mut globals_map = BTreeMap::new();
        let mut data_segment_outputs = IdVec::new();
        let mut offset = 0;

        log::debug!("Data segments for module: {:#?}", data_segments);
        for (segment_id, segment) in data_segments.iter() {
            let out = segment.to_lib_output((!main_module).then_some(lib_base_id), offset as i32);
            // TODO: apply relocations to data segment
            offset += out.as_raw().len();
            if !main_module {
                for constructor in out.globals() {
                    globals_map.insert(
                        (segment_id, constructor.symbol_index),
                        (globals.len() + num_global_imports) as u32,
                    );
                    globals.push(Global::WithConstructor(GlobalConstructor::DataSymbol(
                        constructor.clone(),
                    )));
                }
            }
            data_segment_outputs.push(out);
        }

        let mut defined_functions: Vec<_> = funcs_to_define
            .iter()
            .map(|&(func_id, export)| {
                let defined_id = module_info.as_defined_function_id(func_id).unwrap();
                // Collect all relocation entries that modify something within this function.
                let func_info = &module_info.source.code.defined_funcs[defined_id];
                let range = func_info.body.range();
                let func_relocs =
                    Self::get_relocations_for_range(&emit_info.all_relocations, &range);

                let modification_list = func_relocs
                    .iter()
                    .map(|entry| {
                        // TODO: Filter shared symbols - don't relocate them
                        modify::ModifyEntry::from_relocation_entry(
                            |id| {
                                let SymbolIndex::DataDefined(segment_id, data_id) =
                                    module_info.source.linking.linking_symbols.original_indexes[id]
                                else {
                                    bail!(
                                        "Relocation {entry:?} does not refer to a valid data symbol"
                                    );
                                };
                                Ok(globals_map
                                    .get(&(segment_id, data_id))
                                    .copied()
                                    .map(GlobalVar::Extract)
                                    .unwrap_or_default())
                            },
                            entry,
                            !main_module, // extract const only on sub modules
                            range.start,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();

                DefinedFunction {
                    export,
                    input_func_id: func_id,
                    modification_list,
                }
            })
            .collect();
        import_functions.sort();
        defined_functions.sort();

        log::trace!("import_functions: {:#?}", import_functions);
        log::trace!("defined_functions: {:#?}", defined_functions);
        let mut input_function_output_id: HashMap<_, _> = {
            let import_fns = import_functions
                .iter()
                .map(|import_func| import_func.input_func_id());
            let defined_fns = defined_functions
                .iter()
                .map(|DefinedFunction { input_func_id, .. }| *input_func_id);

            import_fns
                .chain(defined_fns)
                .enumerate()
                .map(|(output_func_id, input_func_id)| (input_func_id, output_func_id))
                .collect()
        };

        let input_data_to_output_id: HashMap<DataId, (DataSegmentId, usize)> = data_segment_outputs
            .iter()
            .flat_map(|(segment_index, segment)| {
                segment
                    .globals()
                    .iter()
                    .enumerate()
                    .map(move |(data_index, data)| {
                        (
                            (segment_index, data.symbol_index),
                            (segment_index, data_index),
                        )
                    })
            })
            .collect();
        // Map references to `import_func` to `export_func`.
        for (_, output_module) in program_info.output_modules.iter() {
            for split_point in output_module.split_points.iter() {
                if let Some(&output_func_id) =
                    input_function_output_id.get(&split_point.export_func)
                {
                    log::trace!("Mapping split point {split_point:?} -> {output_func_id}");
                    input_function_output_id.insert(split_point.import_func, output_func_id);
                } else {
                    log::trace!("Split point {split_point:?} export function not found");
                }
            }
        }

        let indirect_function_table_range =
            emit_info.indirect_functions.table_range_for_output_module[output_module_index].clone();

        Self {
            import_functions,
            defined_functions,
            info: module_info,
            data: data_segment_outputs,
            globals,
            emit_info,
            input_function_output_id,
            indirect_function_table_range,
            lib_base_import,
            global_tmp_store,
            input_data_to_output_id,
        }
    }

    fn get_relocations_for_range<'b>(
        all_relocations: &'b [wasmparser::RelocationEntry],
        range: &Range<usize>,
    ) -> &'b [wasmparser::RelocationEntry] {
        let start = all_relocations
            .binary_search_by_key(&range.start, |reloc| reloc.offset as usize)
            .map_or_else(identity, identity);
        let end = all_relocations
            .binary_search_by_key(&range.end, |reloc| reloc.offset as usize)
            .map_or_else(identity, identity);

        &all_relocations[start..end]
    }
    fn is_main(&self) -> bool {
        self.lib_base_import.is_none()
    }

    fn generate(
        &'any self,
        main_module: &'any ModuleEmitState<'any, 'src>,
        output_module: &mut wasm_encoder::Module,
    ) -> Result<()> {
        // Encode type section
        self.generate_type_section(output_module)?;
        self.generate_import_section(output_module);
        self.generate_function_section(output_module);
        if self.is_main() {
            self.generate_table_element_sections(output_module)?;
            self.generate_memory_section(output_module);
        }
        self.generate_global_section(output_module)?;
        self.generate_export_section(output_module);
        if self.is_main() {
            self.generate_element_section(output_module)?;
        }
        self.generate_code_section(main_module, output_module)?;
        self.generate_data_section(output_module)?;
        // self.generate_custom_sections(output_module)?;

        // self.generate_wasm_bindgen_sections(output_module);
        // Names + Linking + Relocations
        // self.generate_compiler_tools_sections(output_module)?;
        // self.generate_name_section(output_module)?;
        // self.generate_target_features_section(output_module);
        Ok(())
    }

    // TODO: Regenerate function types section (remove unused types)
    fn generate_type_section(&self, output_module: &mut wasm_encoder::Module) -> Result<()> {
        // Simply copy all types.  Unneeded types may be pruned by `wasm-opt`.
        let mut section = wasm_encoder::TypeSection::new();
        // Only use func_types from OutputFunctions
        for (_id, input_func_type) in self.info.source.types.iter() {
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

    fn generate_import_section(&self, output_module: &mut wasm_encoder::Module) {
        // TODO: Only main module should have original imports.
        // Submodule should contain data + function imports from main module.
        // Additionally import memory + heap + stack globals.

        let mut section = wasm_encoder::ImportSection::new();

        for (index, import_fn) in self.import_functions.iter().enumerate() {
            let ty = wasm_encoder::EntityType::Function(
                self.get_function_type(index).as_raw_index() as u32,
            );
            let fn_name = self.get_function_name(index);
            let module_name = import_fn.module_name();
            section.import(&module_name, &fn_name, ty);
        }

        // Copy all non-function imports from input.
        for (_id, import) in self.info.source.imports.iter() {
            if let wasmparser::TypeRef::Func(_) = import.ty {
                continue;
            }
            let ty: wasm_encoder::EntityType = import.ty.clone().try_into().unwrap();
            section.import(import.module, import.name, ty);
        }

        if let Some(lib_base_import) = &self.lib_base_import {
            section.import(
                "__wasm_split",
                "lib_base_id",
                lib_base_import.global_type.clone(),
            );
        }

        if !self.is_main() {
            // Import all globals defined by the input module.
            for (global_index, global) in self.info.source.globals.iter() {
                let ty: wasm_encoder::GlobalType = global.ty.try_into().unwrap();
                if !ty.mutable {
                    continue;
                }
                section.import(
                    "__wasm_split",
                    self.get_global_name(global_index).as_str(),
                    ty,
                );
            }

            // Import all memories defined by the input module.
            for (memory_index, memory) in self.info.source.memories.iter() {
                let ty: wasm_encoder::MemoryType = memory.clone().try_into().unwrap();
                section.import(
                    "__wasm_split",
                    self.get_memory_name(memory_index).as_str(),
                    ty,
                );
            }
        }
        output_module.section(&section);
    }

    fn _get_input_func_id(&self, index: OutputFuncId) -> InputFuncId {
        if index < self.import_functions.len() {
            self.import_functions[index].input_func_id()
        } else {
            let defined_func_id = index - self.import_functions.len();
            self.defined_functions[defined_func_id].input_func_id
        }
    }

    // Get type of output function by index.
    // TODO: regenerate type section
    fn get_function_type(&self, index: OutputFuncId) -> FuncTypeId {
        let input_func_id = self._get_input_func_id(index);

        let Some(defined_index) = self.info.as_defined_function_id(input_func_id) else {
            // It's import function - recover from import id.
            let import_id =
                self.info.import_funcs_info.imported_funcs[input_func_id.as_raw_index()];
            let TypeRef::Func(ty) = self.info.source.imports[import_id].ty else {
                panic!("Expected function type")
            };
            return FuncTypeId::from_index(ty);
        };
        // It's a defined function.
        self.info.source.defined_func_type_id(defined_index)
    }
    // Get name of output function by index.
    fn get_function_name(&self, index: OutputFuncId) -> String {
        let input_func_id = self._get_input_func_id(index);
        self.info
            .source
            .names
            .functions
            .get(input_func_id)
            .map(|name| name.to_string())
            .unwrap_or_else(|| format!("func_{index}"))
    }

    fn get_global_name(&self, index: GlobalId) -> String {
        self.info
            .source
            .names
            .globals
            .get(index)
            .map(|name| name.to_string())
            .or_else(|| {
                self.info
                    .export_map
                    .get(&(
                        wasmparser::ExternalKind::Global as isize,
                        index.as_raw_index(), // TODO: convert indexes?
                    ))
                    .map(|(_, name)| name.to_string())
            })
            .unwrap_or_else(|| format!("__global_{index}"))
    }

    fn get_memory_name(&self, index: MemoryId) -> String {
        self.info
            .source
            .names
            .memories
            .get(index)
            .map(|name| name.to_string())
            .or_else(|| {
                self.info
                    .export_map
                    .get(&(
                        wasmparser::ExternalKind::Memory as isize,
                        index.as_raw_index(), // TODO: convert indexes?
                    ))
                    .map(|(_, name)| name.to_string())
            })
            .unwrap_or_else(|| format!("__memory_{index}"))
    }
    fn get_indirect_function_table_type(&self) -> wasm_encoder::TableType {
        // + 1 due to empty entry at index 0
        let indirect_table_size = self.emit_info.indirect_functions.table_entries.len() + 1;
        wasm_encoder::TableType {
            element_type: wasm_encoder::RefType::FUNCREF,
            minimum: indirect_table_size as u64,
            maximum: Some(indirect_table_size as u64),
            shared: false,
            table64: false,
        }
    }
    fn generate_export_section(&self, output_module: &mut wasm_encoder::Module) {
        let mut section = wasm_encoder::ExportSection::new();
        let mut existing_exports = HashSet::<&str>::new();
        // left original exports as is (because this module should be in drop replacement)
        for (_id, export) in self.info.source.exports.iter() {
            let mut index = export.index;
            if export.kind == wasmparser::ExternalKind::Func {
                let Some(&func_id) = self
                    .input_function_output_id
                    .get(&(InputFuncId::from_index(index)))
                else {
                    continue;
                };
                index = func_id as u32;
            }
            section.export(export.name, export.kind.try_into().unwrap(), index);
            existing_exports.insert(export.name);
        }

        for (func_id, func) in self.defined_functions.iter().enumerate() {
            if !func.export {
                continue;
            }
            let func_id = func_id + self.import_functions.len();
            let name = self.get_function_name(func_id);
            if existing_exports.contains(name.as_str()) {
                continue;
            }
            section.export(
                name.as_str(),
                wasm_encoder::ExportKind::Func,
                func_id as u32,
            );
        }

        let num_extra_globals = if self.lib_base_import.is_some() { 1 } else { 0 };
        // Export globals.
        for (global_index, global) in self.info.source.globals.iter() {
            let name = self.get_global_name(global_index);
            if existing_exports.contains(name.as_str()) {
                continue;
            }
            if !global.ty.mutable {
                break;
            }
            // TODO: fix global id?
            section.export(
                name.as_str(),
                wasm_encoder::ExportKind::Global,
                global_index.as_raw_index() as u32 + num_extra_globals,
            );
        }
        output_module.section(&section);
    }

    fn generate_function_section(&self, output_module: &mut wasm_encoder::Module) {
        let mut section: wasm_encoder::FunctionSection = wasm_encoder::FunctionSection::new();
        for (index, _func) in self.defined_functions.iter().enumerate() {
            let func_type = self.get_function_type(index + self.import_functions.len());
            section.function(func_type.as_raw_index() as u32);
        }

        output_module.section(&section);
    }

    // only for main
    fn generate_table_element_sections(
        &self,
        output_module: &mut wasm_encoder::Module,
    ) -> Result<()> {
        let mut section = wasm_encoder::TableSection::new();
        section.table(self.get_indirect_function_table_type());
        output_module.section(&section);
        Ok(())
    }

    fn generate_element_section(&self, output_module: &mut wasm_encoder::Module) -> Result<()> {
        let indirect_range = self.indirect_function_table_range.clone();
        if indirect_range.is_empty() {
            panic!("No indirect range");
            return Ok(());
        }
        let mut section = wasm_encoder::ElementSection::new();
        let func_ids: Vec<u32> = indirect_range
            .clone()
            .map(|table_index| -> Result<u32> {
                let input_func_id =
                    self.emit_info.indirect_functions.table_entries[table_index - 1];
                let output_func_id = *self
                    .input_function_output_id
                    .get(&input_func_id)
                    .ok_or_else(|| {
                        anyhow!(
                            "No output function corresponding to input function {input_func_id:?}"
                        )
                    })?;
                Ok(output_func_id as u32)
            })
            .collect::<Result<Vec<_>>>()?;
        section.segment(wasm_encoder::ElementSegment {
            mode: wasm_encoder::ElementMode::Active {
                table: Some(0),
                offset: &wasm_encoder::ConstExpr::i32_const(indirect_range.start as i32),
            },
            elements: wasm_encoder::Elements::Functions(std::borrow::Cow::Borrowed(&func_ids)),
        });
        output_module.section(&section);
        Ok(())
    }

    fn generate_memory_section(&self, output_module: &mut wasm_encoder::Module) {
        if self.info.source.memories.is_empty() {
            return;
        }
        let mut section = wasm_encoder::MemorySection::new();
        for (_idx, memory) in self.info.source.memories.iter() {
            section.memory(memory.clone().try_into().unwrap());
        }
        output_module.section(&section);
    }

    fn generate_global_section(&self, output_module: &mut wasm_encoder::Module) -> Result<()> {
        let mut section = wasm_encoder::GlobalSection::new();
        for global in self.globals.iter() {
            match global {
                Global::PlainCopy(global) => {
                    section.global(
                        global.ty.clone().try_into().unwrap(),
                        &global.init_expr.clone().try_into().unwrap(),
                    );
                }
                Global::WithConstructor(new) => {
                    if let Some(v) = &self.lib_base_import {
                        section.global(new.global_type(), &new.global_init(v.global_id));
                    } else {
                        bail!("Trying to define global for main module");
                    }
                }
            }
        }
        output_module.section(&section);
        Ok(())
    }

    fn generate_code_section(
        &'any self,
        main_module: &'any ModuleEmitState<'any, 'src>,
        output_module: &mut wasm_encoder::Module,
    ) -> Result<()> {
        let mut section = wasm_encoder::CodeSection::new();
        for output_func in self.defined_functions.iter() {
            let defined_id = self
                .info
                .as_defined_function_id(output_func.input_func_id)
                .expect("Defined function expected");

            // TODO: collect relocations.

            let result = ModifyContext::emit_code_with_changes(
                self,
                main_module,
                if self.lib_base_import.is_some() { 1 } else { 0 },
                defined_id,
                &output_func.modification_list,
            )?;
            section.raw(&result);
        }
        output_module.section(&section);
        Ok(())
    }
    fn generate_data_section(&self, output_module: &mut wasm_encoder::Module) -> Result<()> {
        const MEMORY_INDEX: u32 = 0; //TODO: Support multiple memories
        let mut section = wasm_encoder::DataSection::new();

        for (_id, out) in self.data.iter() {
            section.segment(out.data_segment(MEMORY_INDEX));
        }

        output_module.section(&section);
        Ok(())
    }

    // linking| names | wasm-bindgen
    // other whitelisted
    fn generate_custom_sections(&self, output_module: &mut wasm_encoder::Module) -> Result<()> {
        todo!("Not supported")
    }
}

fn is_indirect_function_reloc(ty: RelocationType) -> bool {
    use RelocationType::*;
    match ty {
        TableIndexSleb | TableIndexI32 | TableIndexRelSleb | TableIndexSleb64 | TableIndexI64
        | TableIndexRelSleb64 => true,
        _ => false,
    }
}

fn get_indirect_functions(module: &InputModule) -> Result<HashSet<InputFuncId>> {
    let mut funcs = HashSet::new();

    for relocs in [module.code.section_index, module.data.section_index]
        .iter()
        .filter_map(|section_index| module.relocs.relocs.get(*section_index))
    {
        for entry in relocs.entries.iter() {
            if is_indirect_function_reloc(entry.ty) {
                let sym_index =
                    &module.linking.linking_symbols.original_indexes[entry.index as usize];
                let SymbolIndex::Func(index) = sym_index else {
                    bail!("Invalid symbol {sym_index:?} referenced by relocation {entry:?}");
                };
                funcs.insert(*index);
            }
        }
    }

    Ok(funcs)
}

#[derive(Debug, Default)]
pub struct IndirectFunctionEmitInfo {
    pub table_entries: Vec<InputFuncId>,
    pub function_table_index: HashMap<InputFuncId, usize>,
    pub table_range_for_output_module: Vec<Range<usize>>,
}

impl IndirectFunctionEmitInfo {
    fn new(module: &InputModule, program_info: &SplitProgramInfo) -> Result<Self> {
        let mut indirect_functions = get_indirect_functions(module)?;

        indirect_functions.extend(
            program_info
                .shared_nodes
                .iter()
                .filter_map(|dep| match dep {
                    DepNode::Function(func_id) => Some(*func_id),
                    _ => None,
                }),
        );

        // Remove all split point imports. These are placeholders. Any
        // references to these functions will be replaced by a reference to the
        // corresponding `SplitPoint::export_func`.
        for (_, output_module) in program_info.output_modules.iter() {
            for split_point in output_module.split_points.iter() {
                indirect_functions.remove(&split_point.import_func);
            }
        }

        let mut table_entries: Vec<_> = indirect_functions.into_iter().collect();
        table_entries.sort_by_key(|&func_id| {
            (
                program_info
                    .symbol_output_module
                    .get(&DepNode::Function(func_id)),
                func_id,
            )
        });
        let function_table_index: HashMap<_, _> = table_entries
            .iter()
            .enumerate()
            .map(|(i, func_id)| (*func_id, i + 1))
            .collect();

        let mut table_range_for_output_module: Vec<Range<usize>> = program_info
            .output_modules
            .iter()
            .map(|_| Range {
                start: usize::MAX,
                end: 0,
            })
            .collect();

        for (&func, &table_index) in function_table_index.iter() {
            if let Some(&output_module_index) = program_info
                .symbol_output_module
                .get(&DepNode::Function(func))
            {
                let range = &mut table_range_for_output_module[output_module_index];
                range.start = range.start.min(table_index);
                range.end = range.end.max(table_index + 1);
            }
        }

        Ok(Self {
            table_entries,
            function_table_index,
            table_range_for_output_module,
        })
    }
}
#[derive(Debug)]
pub struct EmitInfo {
    pub indirect_functions: IndirectFunctionEmitInfo,
    // All relocations, ordered by offset, which are relative to the start of
    // the file rather than the start of the section.
    pub all_relocations: Vec<RelocationEntry>,

    // Imports (corresponding to split points) to exclude from all modules.
    pub split_point_imports: HashSet<ImportId>,
}

impl EmitInfo {
    fn new(module: &InputModule<'_>, program_info: &SplitProgramInfo) -> Result<Self> {
        let indirect_functions = IndirectFunctionEmitInfo::new(module, program_info)?;
        let mut all_relocations = Vec::<RelocationEntry>::new();
        for (section_index, section_offset) in [
            (module.code.section_index, module.code.starting_offset),
            (module.data.section_index, module.data.starting_offset),
        ] {
            let Some(section_relocs) = module.relocs.relocs.get(section_index) else {
                continue;
            };
            for reloc in &section_relocs.entries {
                let mut reloc = reloc.clone();
                reloc.offset =
                    reloc
                        .offset
                        .checked_add(section_offset as u32)
                        .ok_or_else(|| {
                            anyhow!(
                            "Invalid relocation {reloc:?} for section offset {section_offset:?}"
                        )
                        })?;
                all_relocations.push(reloc);
            }
        }
        all_relocations.sort_by_key(|reloc| reloc.offset);
        let mut split_point_imports = HashSet::<ImportId>::new();
        for (_, output_module) in program_info.output_modules.iter() {
            for split_point in output_module.split_points.iter() {
                split_point_imports.insert(split_point.import);
            }
        }
        Ok(EmitInfo {
            indirect_functions,
            all_relocations,
            split_point_imports,
        })
    }
}

pub fn emit_modules<'a>(
    module: &'a analysis::ModuleInfo<'a>,
    program_info: &SplitProgramInfo,
    emit_fn: &dyn Fn(usize, &[u8]) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    // For now we will ignore data symbols because that simplifies things quite a bit.

    let emit_info = EmitInfo::new(&module.source, program_info)?;
    let modules_ids_iter = program_info.output_modules.iter().enumerate().filter_map(
        |(output_module_index, (id, _))| {
            // Skip shared modules, they are processed inside modules.
            SplitModuleIdentifier::as_single(id).map(|id| (output_module_index, id))
        },
    );

    // re-build data_segments (using only available symbols)
    let data_segments_symbols = module
        .data_symbols
        .chunk_by(|left, right| left.segment_index == right.segment_index)
        .collect::<Vec<_>>();
    let data_segments = module
        .source
        .data
        .section_payload
        .data_segments
        .iter()
        .enumerate()
        .map(|(data_segment, data)| {
            DataSegment::new_inner(
                data.clone(),
                data_segments_symbols
                    .get(data_segment)
                    .cloned()
                    .expect("Symbols for data segment not found"),
            )
        })
        .collect::<Result<IdVec<_>>>()?;
    log::debug!("Data segments with symbols: {:#?}", data_segments);

    let output_modules = modules_ids_iter
        .clone()
        .map(|(output_module_index, _)| {
            ModuleEmitState::produce_state(
                module,
                &data_segments,
                &emit_info,
                program_info,
                output_module_index,
            )
        })
        .collect::<Vec<_>>();

    let main_module_id = modules_ids_iter
        .clone()
        .find(|(_, id)| matches!(id, ModuleIdentifier::Main))
        .map(|(output_module_index, _)| output_module_index)
        .expect("Main module not found");

    for (output_module_index, _) in modules_ids_iter.clone() {
        let state = &output_modules[output_module_index];
        let identifier = &program_info.output_modules[output_module_index].0;
        let mut encoder = wasm_encoder::Module::new();
        state
            .generate(&output_modules[main_module_id], &mut encoder)
            .with_context(|| format!("Error generating {:?}", identifier))?;

        emit_fn(output_module_index, encoder.as_slice())
            .with_context(|| format!("Error emitting {:?}", identifier))?;
    }

    Ok(())
}

// data transformation:
// 1. add global symbol
// 2. init segment at (global.get $env.lib_memory_base)
// 3. implement fn that init global symbols
//
// example:

//```wat
// (module
//   (type $t0 (func))
//   (type $t1 (func (param i32) (result i32)))
//   (memory (;0;) 17)
//   (global $__stack_pointer (;0;) (mut i32) i32.const 1048576)
//   (func $getter (type $t1) (param $p0 i32) (result i32)
//     i32.const 100
//     i32.load)
//   (export "getter" (func $getter))
//   (data $d0 (i32.const 100) "Hello, world!"))

//```
// Result:
// ```wat
// (module
//   (type $t0 (func))
//   (type $t1 (func (param i32) (result i32)))
//   (import "env" "__lib_base" (global $__lib_base i32))
//   (import "env" "__stack_pointer" (global $__stack_pointer i32))
//   (import "env" "memory" (memory $memory 1))
//   (func $getter (type $t1) (param $p0 i32) (result i32)
//     global.get $greeting
//   )
//   ;; THIS CODE IS NOT WORK with wat2wasm CLI so use `wasm-tools parse` instead
//   (global $greeting i32 global.get $__lib_base i32.const 0 i32.add) ;; 0 is local offset
//   (export "getter" (func $getter))
//   (data $d0 (global.get $__lib_base) "Hello, world!"))
//```
