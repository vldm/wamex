use std::{
    collections::{BTreeMap, HashMap, HashSet},
    convert::identity,
    ops::Range,
};

use anyhow::{anyhow, bail, Context, Result};
pub use data_segments::{DataSegment, DataSegmentOutput, NamedData, SymbolRelation};
use globals::GlobalConstructor;
use index_safety::OutputFuncId;
use modify::{init_each_store_var, GlobalSymbolOp, ModifyContext, StoreType};
use wasm_encoder::GlobalType;
use wasmparser::{Data, RelocationEntry, RelocationType, TypeRef};

use crate::{
    analysis::{
        self,
        dep_graph::DepNode,
        split_point::{ModuleIdentifier, SplitModuleIdentifier, SplitProgramInfo},
    },
    emit::{
        globals::DataSymbol,
        modify::{DataSymbolWithOffset, RelocateState, StartFnGen},
    },
    helpers::{encoding_size, iter_if},
    index::{
        AnySymbolId, DataId, DataSegmentId, FuncTypeId, GlobalId, IdMap, IdVec, ImportId, Indexed,
        InputFuncId, MemoryId, OutputGlobalId, OutputSymbolDataId, WithOriginalIndex,
    },
    read::{self, linking::SymbolIndex, InputModule},
};

mod data_segments;
mod globals;

mod index_safety;
mod modify;
mod names;

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct DefinedFunction {
    input_func_id: InputFuncId,
    export: bool,
    // List of modifications that should be applied to this function.
    modification_list: Vec<modify::CodeModifyEntry>,
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
        import_function_name: &'a str,
    },
    // Add new import function from another module (e.g. main module).
    New {
        link_module: usize,
        output_function_index: usize,
        mangled_function_name: &'a str,
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
    pub fn function_name(&self) -> &str {
        match self.kind {
            ImportFunctionKind::Existing {
                import_function_name,
                ..
            } => import_function_name,
            ImportFunctionKind::New {
                mangled_function_name,
                ..
            } => mangled_function_name,
        }
    }
}

#[derive(Debug)]
enum Global<'a> {
    PlainCopy(wasmparser::Global<'a>),
    WithConstructor(GlobalConstructor),
}

struct ExtraImportGlobal {
    global_id: u32,
    global_type: wasm_encoder::GlobalType,
}

impl ExtraImportGlobal {
    fn global_id(&self) -> u32 {
        self.global_id
    }
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
    functions: WithOriginalIndex<'src, DefinedFunction>,
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
    table_base_import: Option<ExtraImportGlobal>,
    start_fn: Option<StartFnGen>,

    // Data Section
    data: IdVec<data_segments::DataSegmentOutput, DataSegmentId>,
    data_relocations: IdMap<DataSegmentId, Vec<modify::DataModifyEntry>>,

    // src module
    pub info: &'any analysis::ModuleInfo<'any, 'src>,
    // Generated fields:
    // Fields that calculated from other fields, and should be updated after any change.
    pub input_data_to_output_id: HashMap<DataId, (DataSegmentId, OutputSymbolDataId)>,
    // Indirect function table Functions from original table that are used in this module.
    pub indirect_functions: IndirectFunctionEmitInfo,
    pub linkage_type: LinkageType,
}

const MEMORY_INDEX: u32 = 0; //TODO: Support multiple memories
impl<'any, 'src> ModuleEmitState<'any, 'src> {
    pub fn produce_state(
        module_info: &'any analysis::ModuleInfo<'any, 'src>,
        emit_info: &'any EmitInfo,
        // Merge use only output_module instead of program_info and output_module_index
        program_info: &SplitProgramInfo,
        output_module_index: usize,

        common_module: Option<&Self>,
        linkage_type: LinkageType,
    ) -> ModuleEmitState<'any, 'src> {
        let output_module_info = &program_info.output_modules[output_module_index].1;

        log::debug!("output_module_info: {output_module_info:#?}");
        // We need to include definitions for all of the `defined_symbols`.
        let mut funcs_to_define = HashSet::<(InputFuncId, bool)>::new();
        let mut import_functions = Vec::new();

        let main_module = output_module_index == 0;
        assert_eq!(main_module, common_module.is_none());

        let shared_imports = program_info
            .output_modules
            .iter()
            .filter_map(|(id, output_module)| {
                if let SplitModuleIdentifier::Shared(_) = id {
                    Some(output_module.defined_symbols.iter())
                } else {
                    None
                }
            })
            .flatten();

        let mut used_funcs = HashSet::new();
        for func_id in output_module_info
            .defined_symbols
            .iter()
            .chain(iter_if(main_module, shared_imports.clone()))
            .filter_map(DepNode::as_function)
        {
            if used_funcs.contains(&func_id) {
                continue;
            }
            used_funcs.insert(func_id);
            if let Some(import_id) = module_info.get_function_import_id(func_id) {
                let import_fn = module_info.source.imports[import_id];
                import_functions.push(ImportedFunction {
                    input_func_id: func_id,
                    kind: ImportFunctionKind::Existing {
                        module_name: import_fn.module,
                        import_function_name: import_fn.name,
                    },
                });
            } else {
                let need_export = program_info
                    .output_modules
                    .iter()
                    .any(|(_, output_module)| {
                        output_module
                            .link_symbols
                            .contains(&DepNode::Function(func_id))
                    });
                funcs_to_define.insert((func_id, need_export));
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
                            mangled_function_name: module_info
                                .source
                                .names
                                .functions
                                .get(func_id)
                                .expect("Function name should be defined"),
                        },
                    }),
            );
        }

        let mut globals: Vec<_> = module_info
            .source
            .globals
            .iter()
            .filter(|(_id, global)| {
                // Skip mutable globals in sub modules
                // They are imported from main module.
                main_module || !global.ty.mutable
            })
            .map(|(_id, global)| Global::PlainCopy(global.clone()))
            .collect();

        let num_main_module_imports = module_info.source.globals.len() - globals.len();

        let mut num_global_imports = module_info
            .source
            .imports
            .iter()
            .filter(|(_id, v)| matches!(v.ty, wasmparser::TypeRef::Global(_)))
            .count()
            + num_main_module_imports;
        let lib_base_import = (!main_module).then(|| {
            let lib_base_id = num_global_imports as u32;
            num_global_imports += 1;
            ExtraImportGlobal {
                global_id: lib_base_id,
                global_type: wasm_encoder::GlobalType {
                    val_type: wasm_encoder::ValType::I32,
                    mutable: false,
                    shared: false,
                },
            }
        });

        let table_base_import = (!main_module).then(|| {
            let table_base_id = num_global_imports as u32;
            num_global_imports += 1;
            ExtraImportGlobal {
                global_id: table_base_id,
                global_type: wasm_encoder::GlobalType {
                    val_type: wasm_encoder::ValType::I32,
                    mutable: false,
                    shared: false,
                },
            }
        });

        let mut data_to_define = BTreeMap::new();
        for i in output_module_info
            .defined_symbols
            .iter()
            // Include all shared data segments also.
            .chain(iter_if(main_module, shared_imports.clone()))
            .filter_map(DepNode::as_data_symbol)
        {
            data_to_define
                .entry(i.0)
                .or_insert_with(HashSet::new)
                .insert(i.1);
        }

        // filter only used entries
        let data_segments = emit_info
            .src_data_segments
            .iter()
            .map(|(data_segment_id, data)| {
                let empty = HashSet::new();
                let entries = data_to_define.get(&data_segment_id).unwrap_or(&empty);
                let data_segment = data.clone();
                data_segment.new_with_whitelist(entries)
            })
            .collect::<IdVec<_>>();

        let mut global_tmp_store = HashMap::new();
        if !main_module {
            for (store_type, val_type) in init_each_store_var() {
                let global_id = globals.len() + num_global_imports;
                global_tmp_store.insert(store_type, global_id as OutputGlobalId);
                globals.push(Global::WithConstructor(GlobalConstructor::TempStore(
                    GlobalType {
                        val_type,
                        mutable: true,
                        shared: false,
                    },
                )));
            }
        }

        let mut data_symbols_map = BTreeMap::new();
        let mut data_segment_outputs = IdVec::new();

        let first_segment = data_segments
            .iter()
            .next()
            .expect("There should be at least one data segment")
            .1;
        let mem_start = first_segment.memory_offset();

        let globals = globals; // stop mutation

        // offset of current segment.
        let mut segment_mem_offset = 0;
        log::debug!("Data segments for module: {:#?}", data_segments);
        for (segment_id, segment) in data_segments.iter() {
            let lib_base_global_id = lib_base_import.as_ref().map(ExtraImportGlobal::global_id);

            let (new_segment_offset, out) =
                segment.to_lib_output(lib_base_global_id, mem_start, segment_mem_offset);
            // TODO: apply relocations to data segment
            if out.is_active() {
                segment_mem_offset = new_segment_offset + out.as_raw().len();
            }

            if let Some(main_module) = common_module {
                let segment_id: DataSegmentId = segment_id;
                for constructor in main_module.data[segment_id].symbols() {
                    let input_key = (segment_id, constructor.symbol_index);
                    let offset = GlobalSymbolOp::StaticOffset {
                        offset: constructor.data_offset as u32,
                    };
                    data_symbols_map.insert(input_key, offset);
                }
            }

            for constructor in out.symbols() {
                let input_key = (segment_id, constructor.symbol_index);
                let offset = if !main_module {
                    GlobalSymbolOp::GotOffset {
                        offset: constructor.data_offset as u32,
                    }
                } else {
                    GlobalSymbolOp::StaticOffset {
                        offset: constructor.data_offset as u32,
                    }
                };
                data_symbols_map.insert(input_key, offset);
            }
            data_segment_outputs.push(out);
        }
        // for (i, g) in globals.iter().enumerate() {
        //     log::info!("Global {i}: {g:?}");
        // }
        // dbg!(&globals_map);
        let data_symbol_op = |id| {
            let result = match module_info.source.linking.linking_symbols.original_indexes[id] {
                SymbolIndex::DataDefined(segment_id, data_id) => data_symbols_map
                    .get(&(segment_id, data_id))
                    .copied()
                    .unwrap(),
                _ => return None,
            };
            Some(result)
        };

        let mut data_relocations = IdMap::new();

        // TODO: move shift in previous (segment_id, segment) in data_segments.iter()
        for (segment_id, data_segment) in data_segment_outputs.iter() {
            let segment_relocs = data_segment
                .relocations()
                .into_iter()
                .map(|reloc| {
                    let entry_data_symbol_op = data_symbols_map
                        .get(&(segment_id, reloc.reloc_in_symbol_index))
                        .copied()
                        .unwrap_or_else(|| {
                            panic!(
                                "Symbol ({segment_id}, {}) is not found in map",
                                reloc.reloc_in_symbol_index
                            );
                        });
                    let relocation_context = modify::RelocationContext {
                        dyn_relocate: !main_module,
                        referenced_symbol: data_symbol_op(reloc.entry.index as AnySymbolId),
                        containing_symbol: Some(DataSymbolWithOffset {
                            symbol: entry_data_symbol_op,
                            addend: reloc.offset,
                        }),
                    };
                    modify::DataModifyEntry::from_relocation_entry(
                        &reloc.entry,
                        &relocation_context,
                    )
                })
                .collect::<Result<Vec<_>>>()
                .unwrap();

            data_relocations.insert(segment_id, segment_relocs);
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
                        let relocation_context = modify::RelocationContext {
                            dyn_relocate: !main_module,
                            referenced_symbol: data_symbol_op(entry.index as AnySymbolId),
                            containing_symbol: None,
                        };
                        modify::CodeModifyEntry::from_relocation_entry(entry, &relocation_context)
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

        let input_data_to_output_id: HashMap<DataId, (DataSegmentId, usize)> = data_segment_outputs
            .iter()
            .flat_map(|(segment_index, segment)| {
                segment
                    .symbols()
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

        let funcs = crate::index::ImportsOrDefined::new(import_functions, defined_functions).lock();
        let indirect_function_table = module_info
            .indirect_function_list
            .iter()
            .filter(|indirect_func_id| funcs.get_output_id(**indirect_func_id).is_some())
            .copied()
            .collect();
        let indirect_functions = IndirectFunctionEmitInfo::new(indirect_function_table);

        let start_fn = if let Some(lib_base) = &lib_base_import {
            Some(StartFnGen::new(
                MEMORY_INDEX,
                lib_base.global_id as OutputGlobalId,
                data_relocations
                    .iter()
                    .flat_map(|(_, entries)| entries.iter()),
            ))
        } else {
            None
        };

        Self {
            info: module_info,
            data: data_segment_outputs,
            data_relocations,
            globals,
            lib_base_import,
            table_base_import,
            global_tmp_store,
            input_data_to_output_id,
            indirect_functions,
            start_fn,
            functions: funcs,
            linkage_type,
        }
    }

    pub fn get_relocations_for_range<'b>(
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
        self.generate_table_element_sections(output_module)?;
        if self.is_main() {
            self.generate_memory_section(output_module);
        }
        self.generate_global_section(output_module)?;
        self.generate_export_section(output_module);
        self.generate_start_function_section(output_module)?;
        self.generate_element_section(output_module)?;

        let code_relocs = self.generate_code_section(main_module, output_module)?;
        let data_relocs = self.generate_data_section(main_module, output_module)?;

        // self.generate_wasm_bindgen_sections(output_module);
        // Names + Linking + Relocations
        self.generate_compiler_tools_sections(output_module, code_relocs, vec![])?;
        self.generate_target_features_section(output_module)?;
        self.generate_custom_sections(output_module)?;
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

        for (index, import_fn) in self.functions.imports() {
            let ty = wasm_encoder::EntityType::Function(
                self.get_function_type(index).as_raw_index() as u32,
            );
            let fn_name = import_fn.function_name();
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

            if let Some(lib_base_import) = &self.lib_base_import {
                section.import(
                    "__wasm_split",
                    "__lib_base",
                    lib_base_import.global_type.clone(),
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
        self.functions
            .get_input_id(index)
            .expect("Output function index should be valid")
    }

    fn _get_output_func_id(&self, input_func_id: InputFuncId) -> Option<OutputFuncId> {
        self.functions.get_output_id(input_func_id)
    }

    // Get type of output function by index.
    // TODO: regenerate type section
    fn get_function_type(&self, index: OutputFuncId) -> FuncTypeId {
        let input_func_id = self._get_input_func_id(index);

        self.info.get_function_type_id(input_func_id)
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
        let indirect_table_size = self.indirect_functions.table_entries.len() + 1;
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
                let Some(func_id) = self._get_output_func_id(InputFuncId::from_index(index)) else {
                    continue;
                };
                index = func_id.as_raw_index() as u32;
            }
            section.export(export.name, export.kind.try_into().unwrap(), index);
            existing_exports.insert(export.name);
        }

        for (func_id, func) in self.functions.defined() {
            if !func.export {
                continue;
            }
            let name = self.get_function_name(func_id);
            if existing_exports.contains(name.as_str()) {
                continue;
            }
            section.export(
                name.as_str(),
                wasm_encoder::ExportKind::Func,
                func_id.as_raw_index() as u32,
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

        if !existing_exports.contains("__indirect_function_table") {
            section.export(
                "__indirect_function_table",
                wasm_encoder::ExportKind::Table,
                0,
            );
        }
        output_module.section(&section);
    }

    fn find_void_type(&self) -> FuncTypeId {
        for (fn_id, fn_type) in self.info.source.types.iter() {
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
            section.function(func_type.as_raw_index() as u32);
        }
        // add start function
        if let Some(_start_fn) = &self.start_fn {
            section.function(self.find_void_type().as_raw_index() as u32);
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
        let mut section = wasm_encoder::ElementSection::new();
        let func_ids: Vec<u32> = self
            .indirect_functions
            .table_entries
            .iter()
            .map(|input_func_id| -> Result<u32> {
                let output_func_id = self._get_output_func_id(*input_func_id).ok_or_else(|| {
                    anyhow!("No output function corresponding to input function {input_func_id:?}")
                })?;
                Ok(output_func_id.as_raw_index() as u32)
            })
            .collect::<Result<Vec<_>>>()?;
        section.segment(wasm_encoder::ElementSegment {
            mode: wasm_encoder::ElementMode::Active {
                table: None,
                offset: &wasm_encoder::ConstExpr::i32_const(1 as i32),
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

    fn generate_start_function_section(
        &'any self,
        output_module: &mut wasm_encoder::Module,
    ) -> Result<()> {
        if self.start_fn.is_some() {
            let start = wasm_encoder::StartSection {
                function_index: self.functions.len() as u32,
            };
            output_module.section(&start);
        }
        Ok(())
    }

    fn generate_code_section(
        &'any self,
        main_module: &'any ModuleEmitState<'any, 'src>,
        output_module: &mut wasm_encoder::Module,
    ) -> Result<Vec<RelocationEntry>> {
        let defined_functions_count = self.functions.defined().len() as u32
            + if self.start_fn.is_some() {
                1 // start function
            } else {
                0
            };

        let mut section = wasm_encoder::CodeSection::new();
        let mut code_relocs = Vec::new();
        for (_id, output_func) in self.functions.defined() {
            let function_start_offset = encoding_size(defined_functions_count) + section.byte_len();
            let defined_id = self
                .info
                .as_defined_function_id(output_func.input_func_id)
                .expect("Defined function expected");

            // TODO: collect relocations.

            let (result, modified_relocs) = ModifyContext::emit_code_with_changes(
                self,
                main_module,
                if self.lib_base_import.is_some() { 1 } else { 0 },
                defined_id,
                output_func.input_func_id,
                &output_func.modification_list,
            )?;
            for mut reloc in modified_relocs {
                reloc.offset += function_start_offset as u32;
                code_relocs.push(reloc);
            }
            section.raw(&result);
        }

        if let Some(start_fn) = &self.start_fn {
            section.function(&start_fn.generate_fn());
        }
        output_module.section(&section);

        Ok(code_relocs)
    }
    fn generate_data_section(
        &'any self,
        main_module: &'any ModuleEmitState<'any, 'src>,
        output_module: &mut wasm_encoder::Module,
    ) -> Result<()> {
        let num_new_global_imports = if self.lib_base_import.is_some() {
            1 // for lib_base_id
        } else {
            0
        };
        let mut section = wasm_encoder::DataSection::new();

        for (id, out) in self.data.iter() {
            let mut data = out.data_segment(MEMORY_INDEX);
            let relocs = self.data_relocations.get(id).unwrap();

            for entry in relocs.iter() {
                let state = modify::StartFnModifyContext {
                    data_segment: &mut data.data,
                    relocate: RelocateState {
                        input_module: self.info.source,
                        main_module: main_module,
                        emit_module: self,
                        global_id_mapper: Box::new(move |global_id: GlobalId| {
                            Some(
                                (global_id.as_raw_index() + num_new_global_imports)
                                    as OutputGlobalId, // currently just increase global_id
                            )
                        }),
                    },
                };
                state.apply_relocation(entry)?;
            }
            section.segment(data);
        }

        output_module.section(&section);
        Ok(())
    }
    fn generate_target_features_section(
        &self,
        output_module: &mut wasm_encoder::Module,
    ) -> Result<()> {
        let mut features = self.info.source.target_features.clone();
        features.features.extended_const = true;
        output_module.section(&features.encode_custom_section());
        Ok(())
    }
    // linking| names

    fn generate_compiler_tools_sections(
        &self,
        output_module: &mut wasm_encoder::Module,
        shifted_code_relocs: Vec<RelocationEntry>,
        shifted_data_relocs: Vec<RelocationEntry>,
    ) -> Result<()> {
        // let names = wasm_encoder::CustomSection{
        //     name: "name".into(),
        //     data: self.info.source.names.encode().into(),

        let mut functions = wasm_encoder::NameMap::new();
        for output_id in self.functions.iter_all_ids() {
            let input_id = self._get_input_func_id(output_id);
            let name = self.info.source.names.functions.get(input_id).cloned();
            let tmp;
            let name = match name {
                Some(name) => name,
                None => {
                    tmp = format!("func_{output_id}");
                    &tmp
                }
            };

            functions.append(output_id.as_raw_index() as u32, name);
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
        for custom in &self.info.source.custom_sections {
            match &*custom.name {
                "__wasm_bindgen_unstable" => {
                    if !self.is_main() {
                        continue; // print only on main module
                    }
                }
                _ => {
                    log::error!(
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
    pub table_entries: Vec<InputFuncId>,
    pub function_table_index: HashMap<InputFuncId, usize>,
}

impl IndirectFunctionEmitInfo {
    fn new(table_entries: Vec<InputFuncId>) -> Self {
        let function_table_index: HashMap<_, _> = table_entries
            .iter()
            .enumerate()
            .map(|(i, func_id)| (*func_id, i + 1))
            .collect();

        Self {
            table_entries,
            function_table_index,
        }
    }

    fn is_indirect_function_reloc(ty: RelocationType) -> bool {
        use RelocationType::*;
        match ty {
            TableIndexSleb | TableIndexI32 | TableIndexRelSleb | TableIndexSleb64
            | TableIndexI64 | TableIndexRelSleb64 => true,
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
                if Self::is_indirect_function_reloc(entry.ty) {
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
}
#[derive(Debug)]
pub struct EmitInfo<'src> {
    // All relocations, ordered by offset, which are relative to the start of
    // the file rather than the start of the section.
    // TODO: move to analysis
    pub all_relocations: Vec<RelocationEntry>,

    pub src_data_segments: IdVec<DataSegment<'src>>,

    // Imports (corresponding to split points) to exclude from all modules.
    pub split_point_imports: HashSet<ImportId>,
}

impl<'src> EmitInfo<'src> {
    fn new(
        module: &analysis::ModuleInfo<'_, 'src>,
        program_info: &SplitProgramInfo,
    ) -> Result<Self> {
        let all_relocations = Self::all_relocations(module.source)?;
        let mut split_point_imports = HashSet::<ImportId>::new();
        for (_, output_module) in program_info.output_modules.iter() {
            for split_point in output_module.split_points.iter() {
                split_point_imports.insert(split_point.import);
            }
        }

        // re-build data_segments (using only available symbols)
        let data_segments_symbols = module
            .data_symbols
            .chunk_by(|left, right| left.segment_index == right.segment_index)
            .collect::<Vec<_>>();
        let data_segments: IdVec<DataSegment<'src>> = module
            .source
            .data
            .section_payload
            .data_segments
            .iter()
            .enumerate()
            .map(|(data_segment, data)| {
                let data_relocs =
                    ModuleEmitState::get_relocations_for_range(&all_relocations, &data.range);
                let data_symbols = data_segments_symbols
                    .get(data_segment)
                    .cloned()
                    .expect("Symbols for data segment not found");
                let segment_info = module.source.linking.segments_info[data_segment].clone();

                DataSegment::new_inner(data.clone(), segment_info, data_symbols, data_relocs)
            })
            .collect::<Result<IdVec<DataSegment<'src>>>>()?;
        log::debug!("Data segments with symbols: {:#?}", data_segments);

        let mut print_data_format = String::new();
        for (i, segment) in data_segments.iter() {
            for symbol in segment._data_symbols_iter() {
                let (chunk_hex, chunk_utf8) = match symbol.symbol_relation() {
                    SymbolRelation::Regular { chunk, .. } => (
                        hex::encode(chunk),
                        String::from_utf8_lossy(chunk).to_string(),
                    ),
                    SymbolRelation::BoundToPrevious { .. } => {
                        ("<bound to previous>".to_string(), "".to_string())
                    }
                };
                print_data_format.push_str(&format!(
                    "Data symbol {i}.{index}: {name} [{chunk_hex}] [{chunk_utf8}]\n",
                    i = i,
                    index = symbol.index(),
                    name = symbol.name(),
                    chunk_utf8 = chunk_utf8.escape_debug(),
                ));
            }
        }
        log::warn!("Data segments: {print_data_format}");
        Ok(EmitInfo {
            all_relocations,
            split_point_imports,
            src_data_segments: data_segments,
        })
    }

    pub fn all_relocations(module: &InputModule<'_>) -> Result<Vec<RelocationEntry>> {
        let mut all_relocations = Vec::new();
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
        Ok(all_relocations)
    }
}

pub fn emit_modules<'a, 'src>(
    module: &'a analysis::ModuleInfo<'a, 'src>,
    program_info: &SplitProgramInfo,
    emit_fn: &dyn Fn(&SplitModuleIdentifier, &[u8]) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let emit_info = EmitInfo::new(&module, program_info)?;
    let modules_ids_iter = program_info.output_modules.iter().enumerate().filter_map(
        |(output_module_index, (id, _))| {
            // Skip shared modules, they are processed inside modules.
            SplitModuleIdentifier::as_single(id).map(|id| (output_module_index, id.clone()))
        },
    );

    for (id, output_module) in program_info.output_modules.iter() {
        let SplitModuleIdentifier::Shared(_) = id else {
            continue;
        };
        log::debug!("Shared_modules_info {id:?}: {output_module:?}");
    }

    let linkage_type = LinkageType::OriginalLayout;

    let mut main_module = modules_ids_iter
        .clone()
        .find(|(_, id)| matches!(id, ModuleIdentifier::Main))
        .map(|(output_module_index, _)| {
            ModuleEmitState::produce_state(
                module,
                &emit_info,
                program_info,
                output_module_index,
                None,
                linkage_type,
            )
        })
        .expect("Main module not found");

    let output_sub_modules = modules_ids_iter
        .into_iter()
        .filter(|(_, id)| !matches!(id, ModuleIdentifier::Main))
        .map(|(output_module_index, id)| {
            (
                ModuleEmitState::produce_state(
                    module,
                    &emit_info,
                    program_info,
                    output_module_index,
                    Some(&main_module),
                    linkage_type,
                ),
                id,
            )
        })
        .collect::<Vec<_>>();

    for (state, identifier) in output_sub_modules
        .iter()
        .map(|(state, id)| (state, id))
        .chain(Some((&main_module, &ModuleIdentifier::Main)))
    {
        let mut encoder = wasm_encoder::Module::new();
        state
            .generate(&main_module, &mut encoder)
            .with_context(|| format!("Error generating {:?}", identifier))?;

        emit_fn(
            &SplitModuleIdentifier::Single(identifier.clone()),
            encoder.as_slice(),
        )
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
