use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::identity;
use std::ops::Range;

use anyhow::{anyhow, bail, Context, Result};
use globals::GlobalConstructor;
use wasm_encoder::{EntityType, GlobalType};
use wasmparser::{ConstExpr, RelocationEntry, RelocationType};

use crate::analysis;
use crate::analysis::split_point::SplitModuleIdentifier;
use crate::emit::data_segments::DataSegment;
use crate::index::GlobalId;
use crate::read::linking::SymbolType;
use modify::{init_each_store_var, ModifyContext, StoreType};

use crate::{
    analysis::dep_graph::DepNode,
    analysis::split_point::{OutputModuleInfo, SplitProgramInfo},
    index::{ImportId, InputFuncId, SymbolIndex},
    read::InputModule,
};
use modify::GlobalVar;

mod data_segments;
mod globals;

mod modify;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Copy)]
pub enum FnResolveType {
    // Exports from shared fns are stored in indirect_function_table which are used by dependant modules.
    IndirectFunctionTable,
    // Main module exports all shared functions, this functions are used as imports in other modules.
    MainExportSubImport,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Copy)]
enum OutputFunctionKind {
    // Extract import function from source, or add import of defined source defined function.
    Import,
    // Extract defined function from source
    Define { export: bool },
    // Replace defined function with stub that call imported function.
    CreateIndirectStub,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OutputFunction {
    kind: OutputFunctionKind,
    input_func_id: InputFuncId,
    // List of modifications that should be applied to this function.
    modification_list: Vec<modify::ModifyEntry>,
}

// ignore relocations field in order
impl Ord for OutputFunction {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.kind.cmp(&other.kind) {
            std::cmp::Ordering::Equal => self.input_func_id.cmp(&other.input_func_id),
            ord => return ord,
        }
    }
}
impl PartialOrd for OutputFunction {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

// #[derive(Debug)]
// struct EmitState<'a> {
//     // indirect_functions: IndirectFunctionEmitInfo,
//     // All relocations, ordered by offset, which are relative to the start of
//     // the file rather than the start of the section.
//     all_relocations: Vec<relocation::RelocationEntry>,

//     // Imports (corresponding to split points) to exclude from all modules.
//     split_point_imports: HashSet<ImportId>,
// }

enum Global<'a> {
    PlainCopy(wasmparser::Global<'a>),
    WithConstructor(GlobalConstructor),
}
struct ExtraImportGlobal {
    global_id: u32,
    global_type: wasm_encoder::GlobalType,
}

pub struct ModuleEmitState<'a> {
    // Data Section
    // Global variables:
    // - existing globals from src module
    // - lib_base_id for library base address (import?)
    // - "store" globals for `modify::constant_extractions`
    // - globals for data segments (lib_base_id + offset)
    globals: Vec<Global<'a>>,
    pub global_tmp_store: HashMap<StoreType, GlobalId>,
    data: Vec<data_segments::DataSegmentOutput>,
    output_functions: Vec<OutputFunction>,

    // src module
    pub info: &'a analysis::ModuleInfo<'a>,

    // extra imports that should be emitted for lib
    // Not available for main module.
    lib_base_import: Option<ExtraImportGlobal>,
    pub emit_info: &'a EmitInfo,
    pub input_function_output_id: HashMap<InputFuncId, usize>,
    pub indirect_function_table_range: Range<usize>,
}

impl<'a> ModuleEmitState<'a> {
    pub fn produce_state(
        module_info: &'a analysis::ModuleInfo<'a>,
        emit_info: &'a EmitInfo,
        program_info: &SplitProgramInfo,
        dep_graph: &analysis::dep_graph::DepGraph,
        output_module_index: usize,
        resolve_type: FnResolveType,
    ) -> ModuleEmitState<'a> {
        let output_module_info = &program_info.output_modules[output_module_index].1;

        log::debug!("output_module_info: {output_module_info:#?}");
        // We need to include definitions for all of the `included_symbols`.
        let mut funcs_to_define = HashSet::<(InputFuncId, OutputFunctionKind)>::new();
        funcs_to_define.extend(output_module_info.included_symbols.iter().filter_map(
            |dep| match dep {
                DepNode::Function(func_id) => {
                    let kind = if *func_id < module_info.import_funcs_info.imported_funcs.len() {
                        OutputFunctionKind::Import
                    } else {
                        OutputFunctionKind::Define { export: false }
                    };
                    Some((*func_id, kind))
                }
                _ => None,
            },
        ));

        let main_export = resolve_type == FnResolveType::MainExportSubImport;

        // For the main module, we need to include all functions that are shared in other submodules
        if output_module_index == 0 {
            let shared_imports = program_info
                .output_modules
                .iter()
                .filter_map(|(id, output_module)| {
                    if let SplitModuleIdentifier::Chunk(_) = id {
                        Some(output_module.included_symbols.iter())
                    } else {
                        None
                    }
                })
                .flatten()
                .filter_map(|dep| match dep {
                    DepNode::Function(func_id) => Some(*func_id),
                    _ => None,
                });
            funcs_to_define.extend(shared_imports.map(|func_id| {
                let kind = {
                    if func_id < module_info.import_funcs_info.imported_funcs.len() {
                        OutputFunctionKind::Import
                    } else {
                        OutputFunctionKind::Define {
                            export: main_export,
                        }
                    }
                };
                (func_id, kind)
            }));
        } else {
            // submodule uses only needed shared imports
            funcs_to_define.extend(output_module_info.shared_imports.iter().map(|func_id| {
                let kind = {
                    // TODO: check that this is our chunk?
                    if main_export {
                        OutputFunctionKind::Import
                    } else {
                        OutputFunctionKind::CreateIndirectStub
                    }
                };
                (*func_id, kind)
            }));
        }

        let mut globals: Vec<_> = module_info
            .source
            .globals
            .iter()
            .map(|global| Global::PlainCopy(global.clone()))
            .collect();

        let mut num_global_imports = module_info
            .source
            .imports
            .iter()
            .filter(|v| matches!(v.ty, wasmparser::TypeRef::Global(_)))
            .count();
        dbg!(&num_global_imports);
        let lib_base_id = num_global_imports as u32;
        let lib_base_import = if true {
            //output_module_index != 0 {
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
            data_to_define.entry(i.0).or_insert_with(Vec::new).push(i.1);
        }

        // TODO: Data to reuse.
        dbg!(&data_to_define);
        let data_segments_symbols = dbg!(&module_info.data_symbols)
            .chunk_by(|left, right| left.segment_index == right.segment_index)
            .collect::<Vec<_>>();

        let datas = module_info
            .source
            .data
            .section_payload
            .data_segments
            .iter()
            .enumerate()
            .map(|(data_segment, data)| {
                let empty = Vec::new();
                let entries = data_to_define.get(&data_segment).unwrap_or(&empty);
                DataSegment::new_inner(
                    data.clone(),
                    data_segments_symbols
                        .get(data_segment)
                        .cloned()
                        .unwrap_or(&[] as &[analysis::DataSymbol<'_>]),
                )
                // TODO: reuse data_segments for all modules
                .map(|mut data_segment| {
                    data_segment.retain_symbols(entries);
                    data_segment
                })
            })
            .collect::<Result<Vec<_>>>()
            .unwrap();

        let mut global_tmp_store = HashMap::new();
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
        dbg!(&global_tmp_store);

        let mut globals_map = BTreeMap::new();
        let mut data_segments = vec![];
        let mut offset = 0;

        println!("Data segments: {:#?}", datas);
        for (segment_id, segment) in datas.into_iter().enumerate() {
            let out = segment.to_lib_output(lib_base_id, offset as i32);
            // TODO: apply relocations to data segment
            offset += out.as_raw().len();
            for constructor in out.globals() {
                globals_map.insert(
                    (segment_id, constructor.symbol_index),
                    (globals.len() + num_global_imports) as u32,
                );
                globals.push(Global::WithConstructor(GlobalConstructor::DataSymbol(
                    constructor.clone(),
                )));
            }
            data_segments.push(out);
        }

        let mut output_functions: Vec<_> = funcs_to_define
            .iter()
            .map(|&(func_id, kind)| {
                let modification_list = if matches!(kind, OutputFunctionKind::Define{..}) {
                    let defined_id = func_id
                        .checked_sub(module_info.import_funcs_info.imported_funcs.len())
                        .unwrap();
                    // Collect all relocation entries that modify something within this function.
                    let func_info =
                        &module_info.source.code.section_payload.defined_funcs[defined_id];
                    let range = func_info.body.range();
                    let func_relocs =
                        Self::get_relocations_for_range(&emit_info.all_relocations, &range);

                    let name = module_info
                        .source
                        .names
                        .functions
                        .get(func_id)
                        .map(|(name)| name.to_string())
                        .unwrap_or(format!("func_{func_id}"));
                    println!("func_name[{defined_id}:{func_id}]: {} {:?}, {:?}", &name, &range, &func_relocs);
                    func_relocs
                        .iter()
                        .map(|entry| {
                            // TODO: Filter shared symbols - don't relocate them
                            modify::ModifyEntry::from_relocation_entry(
                                |id| {
                                    let (data_id, SymbolType::DataDefined(segment_id)) =
                                        module_info.source.linking.linking_symbols.original_indexes
                                            [id] else {
                                        bail!(
                                            "Relocation {entry:?} does not refer to a valid data symbol"
                                        );
                                    };
                                    Ok(globals_map.get(dbg!(&(segment_id, data_id))).copied().map(GlobalVar::Extract)
                                        .unwrap_or_default()
                                    )
                                },
                                entry,
                                range.start,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()
                        .unwrap()
                } else {
                    vec![]
                };

                OutputFunction {
                    kind,
                    input_func_id: func_id,
                    modification_list,
                }
            })
            .collect();
        output_functions.sort();

        let mut input_function_output_id: HashMap<_, _> = output_functions
            .iter()
            .enumerate()
            .map(|(output_func_id, &OutputFunction { input_func_id, .. })| {
                (input_func_id, output_func_id)
            })
            .collect();

        dbg!(&input_function_output_id);
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
            output_functions,
            info: module_info,
            data: data_segments,
            globals,
            emit_info,
            input_function_output_id,
            indirect_function_table_range,
            lib_base_import,
            global_tmp_store,
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

    fn generate(&self, output_module: &mut wasm_encoder::Module) -> Result<()> {
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
        self.generate_code_section(output_module)?;
        self.generate_data_section(output_module)?;
        // self.generate_custom_sections(output_module)?;

        // self.generate_wasm_bindgen_sections(output_module);
        // self.generate_name_section(output_module)?;
        // self.generate_target_features_section(output_module);
        Ok(())
    }

    fn generate_type_section(&self, output_module: &mut wasm_encoder::Module) -> Result<()> {
        // Simply copy all types.  Unneeded types may be pruned by `wasm-opt`.
        let mut section = wasm_encoder::TypeSection::new();
        for input_func_type in self.info.source.types.iter() {
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
        let mut section = wasm_encoder::ImportSection::new();
        let original_imports = &self.info.import_funcs_info.imported_funcs;
        // Function imports
        for (func_id, &import_id) in original_imports.iter().enumerate() {
            // TODO: later avoid outer iter
            if !self.output_functions.iter().any(
                |OutputFunction {
                     input_func_id,
                     kind,
                     ..
                 }| {
                    input_func_id == &func_id && *kind == OutputFunctionKind::Import
                },
            ) {
                continue;
            }
            let import = &self.info.source.imports[import_id];
            let ty: wasm_encoder::EntityType = import.ty.clone().try_into().unwrap();
            section.import(import.module, import.name, ty);
        }

        // Copy all non-function imports from input.
        for import in self.info.source.imports.iter() {
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
            // Import indirect function table.

            section.import(
                "__wasm_split",
                "__indirect_function_table",
                self.get_indirect_function_table_type(),
            );

            // Import all globals defined by the input module.
            for (global_index, global) in self.info.source.globals.iter().enumerate() {
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
            for (memory_index, memory) in self.info.source.memories.iter().enumerate() {
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

    fn get_global_name(&self, index: usize) -> String {
        self.info
            .source
            .names
            .globals
            .get(index)
            .map(|name| name.to_string())
            .or_else(|| {
                self.info
                    .export_map
                    .get(&(wasmparser::ExternalKind::Global as isize, index))
                    .map(|(_, name)| name.to_string())
            })
            .unwrap_or_else(|| format!("__global_{index}"))
    }

    fn get_memory_name(&self, index: usize) -> String {
        self.info
            .source
            .names
            .memories
            .get(index)
            .map(|name| name.to_string())
            .or_else(|| {
                self.info
                    .export_map
                    .get(&(wasmparser::ExternalKind::Memory as isize, index))
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
        if !self.is_main() {
            return;
        }
        let mut section = wasm_encoder::ExportSection::new();
        let mut existing_exports = HashSet::<&str>::new();
        for export in self.info.source.exports.iter() {
            let mut index = export.index;
            if export.kind == wasmparser::ExternalKind::Func {
                let Some(&func_id) = self.input_function_output_id.get(&(index as InputFuncId))
                else {
                    continue;
                };
                index = func_id as u32;
            }
            section.export(export.name, export.kind.try_into().unwrap(), index);
            existing_exports.insert(export.name);
        }

        // Export table.
        if !existing_exports.contains("__indirect_function_table") {
            section.export(
                "__indirect_function_table",
                wasm_encoder::ExportKind::Table,
                0,
            );
        }

        // Export globals.
        for (global_index, global) in self.info.source.globals.iter().enumerate() {
            let name = self.get_global_name(global_index);
            if existing_exports.contains(name.as_str()) {
                continue;
            }
            if !global.ty.mutable {
                break;
            }
            section.export(
                name.as_str(),
                wasm_encoder::ExportKind::Global,
                global_index as u32,
            );
        }
        output_module.section(&section);
    }

    fn generate_function_section(&self, output_module: &mut wasm_encoder::Module) {
        let mut section = wasm_encoder::FunctionSection::new();
        for OutputFunction { input_func_id, .. } in self
            .output_functions
            .iter()
            .filter(|OutputFunction { kind, .. }| matches!(kind, OutputFunctionKind::Define { .. }))
        {
            section.function(self.info.source.defined_func_type_id(
                *input_func_id - self.info.import_funcs_info.imported_funcs.len(),
            ) as u32);
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
        for memory in self.info.source.memories.iter() {
            section.memory(memory.clone().try_into().unwrap());
        }
        output_module.section(&section);
    }

    fn generate_global_section(&self, output_module: &mut wasm_encoder::Module) -> Result<()> {
        let mut section = wasm_encoder::GlobalSection::new();
        //todo: init global lib_base_id imported
        for (id, global) in self.globals.iter().enumerate() {
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

    fn generate_code_section(&self, output_module: &mut wasm_encoder::Module) -> Result<()> {
        let mut section = wasm_encoder::CodeSection::new();
        for output_func in self.output_functions.iter() {
            match output_func.kind {
                OutputFunctionKind::Import => {
                    continue;
                }

                OutputFunctionKind::CreateIndirectStub => {
                    continue;
                    todo!()
                    // let indirect_index = self
                    //     .emit_state
                    //     .indirect_functions
                    //     .function_table_index
                    //     .get(&output_func.input_func_id)
                    //     .unwrap();
                    // let function = self.generate_indirect_stub(
                    //     *indirect_index,
                    //     self.input_module.func_type_id(output_func.input_func_id),
                    // );
                    // section.function(&function);
                    // continue;
                }
                OutputFunctionKind::Define { export: bool } => {
                    // main branch
                }
            }

            let defined_id = output_func
                .input_func_id
                .checked_sub(self.info.import_funcs_info.imported_funcs.len())
                .unwrap();

            let result = ModifyContext::emit_code_with_changes(
                &self,
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

        for out in self.data.iter() {
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
    //     fn get_global_name(&self, index: usize) -> String {
    //         self.input_module
    //             .names
    //             .globals
    //             .get(&index)
    //             .map(|name| name.to_string())
    //             .or_else(|| {
    //                 self.input_module
    //                     .export_map
    //                     .get(&(wasmparser::ExternalKind::Global as isize, index))
    //                     .map(|(_, name)| name.to_string())
    //             })
    //             .unwrap_or_else(|| format!("__global_{index}"))
    //     }

    //     fn generate_table_section(&mut self) {
    //         if !self.is_main() {
    //             return;
    //         }
    //         let mut section = wasm_encoder::TableSection::new();
    //         section.table(self.get_indirect_function_table_type());
    //         self.output_module.section(&section);
    //     }

    // fn get_memory_name(&self, index: usize) -> String {
    //     self.input_module
    //         .names
    //         .memories
    //         .get(&index)
    //         .map(|name| name.to_string())
    //         .or_else(|| {
    //             self.input_module
    //                 .export_map
    //                 .get(&(wasmparser::ExternalKind::Memory as isize, index))
    //                 .map(|(_, name)| name.to_string())
    //         })
    //         .unwrap_or_else(|| format!("__memory_{index}"))
    // }
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
                let (index, sym_type) =
                    &module.linking.linking_symbols.original_indexes[entry.index as usize];
                if *sym_type != SymbolType::Func {
                    bail!("Invalid symbol {sym_type:?} referenced by relocation {entry:?}");
                };
                funcs.insert(*index as usize);
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

        indirect_functions.extend(program_info.shared_funcs.iter());

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
        let mut split_point_imports = HashSet::<InputFuncId>::new();
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

    fn get_relocations_for_range(&self, range: &Range<usize>) -> &[RelocationEntry] {
        let start = self
            .all_relocations
            .binary_search_by_key(&range.start, |reloc| reloc.offset as usize)
            .map_or_else(identity, identity);
        let end = self
            .all_relocations
            .binary_search_by_key(&range.end, |reloc| reloc.offset as usize)
            .map_or_else(identity, identity);
        &self.all_relocations[start..end]
    }
}

pub fn emit_modules(
    module: &analysis::ModuleInfo<'_>,
    program_info: &SplitProgramInfo,
    deps: &analysis::dep_graph::DepGraph,
    resolve_type: FnResolveType,
    emit_fn: &dyn Fn(usize, &[u8]) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    // For now we will ignore data symbols because that simplifies things quite a bit.

    let emit_info = EmitInfo::new(&module.source, program_info)?;

    for (output_module_index, (id, _)) in program_info.output_modules.iter().enumerate() {
        if let SplitModuleIdentifier::Chunk(_) = id {
            continue; // Chunks are processed inside modules.
        }
        let module = ModuleEmitState::produce_state(
            module,
            &emit_info,
            program_info,
            deps,
            output_module_index,
            resolve_type,
        );
        let identifier = &program_info.output_modules[output_module_index].0;
        let mut encoder = wasm_encoder::Module::new();
        module
            .generate(&mut encoder)
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
