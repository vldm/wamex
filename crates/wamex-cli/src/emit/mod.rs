use std::{
    borrow::{self, Cow},
    collections::{BTreeMap, BTreeSet},
    convert::identity,
    ops::Range,
};

use anyhow::{anyhow, bail, Context, Result};
pub use data_segments::{DataSegment, DataSegmentOutput, NamedData, SymbolRelation};
use globals::GlobalConstructor;
use gxhash::{HashMap, HashMapExt, HashSet, HashSetExt};
use index_safety::OutputFuncId;
use modify::{init_each_store_var, ModifyContext, StoreType};
use wamex_metadata::{BumpVersion, DemangledName, ExportedSymbol};
use wasm_encoder::{reencode::Reencode, GlobalType};
use wasmparser::{RelocationEntry, TypeRef};

use crate::{
    analysis::{
        self,
        dep_graph::{self, DepGraph, DepNode},
        split_point::{
            ModuleIdentifier, SharedModuleIdentifier, SplitModuleIdentifier, SplitPoint,
            SplitProgramInfo,
        },
    },
    emit::{
        globals::{DefinedGlobal, GlobalImport},
        index_safety::OutputGlobalId,
        modify::{RelocateState, StartFnGen},
    },
    helpers::{encoding_size, iter_if, Hash, RangeExt},
    index::{
        AnySymbolId, DataId, DataSegmentId, FuncTypeId, Id, IdMap, IdVec, ImportId,
        ImportsOrDefined, Indexed, InputFuncId, InputGlobalId, MemoryId, OutputSymbolDataId,
        WithOriginalIndex,
    },
    read::{linking::SymbolIndex, InputModule},
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

trait ImportedEntity {
    fn import_name(&self) -> Cow<'_, str>;
    fn module_name(&self) -> Cow<'_, str>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DefinedFunctionKind {
    Copied {
        // List of modifications that should be applied to this function.
        modification_list: Vec<modify::CodeModifyEntry>,
    },
    IndirectTrampoline {
        /// Index of extra table entry after main module entries.
        table_index_offset: u32,
    },
    // Stub function generated for imported functions
    Trampoline {},
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinedFunction {
    export: bool,
    input_func_id: InputFuncId,
    kind: DefinedFunctionKind,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ImportedFunction<'a> {
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
impl ImportedEntity for ImportedFunction<'_> {
    fn import_name(&self) -> Cow<'_, str> {
        match self.kind {
            ImportFunctionKind::Existing {
                import_function_name,
                ..
            } => import_function_name.into(),
            ImportFunctionKind::New {
                mangled_function_name,
                ..
            } => format!("__wamex_{}", mangled_function_name).into(),
        }
    }

    fn module_name(&self) -> Cow<'_, str> {
        match self.kind {
            ImportFunctionKind::Existing { module_name, .. } => module_name.into(),
            ImportFunctionKind::New { .. } => {
                "__wasm_split".into()
                // format!("__wasm_split_link_{}", link_module)
            }
        }
    }
}

impl ImportedFunction<'_> {
    pub fn input_func_id(&self) -> InputFuncId {
        self.input_func_id
    }
}

// ignore relocations field in order
impl Ord for DefinedFunction {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let tag = match self.kind {
            DefinedFunctionKind::Copied { .. } => 0,
            DefinedFunctionKind::IndirectTrampoline { .. } => 1,
            DefinedFunctionKind::Trampoline { .. } => 2,
        };
        let other_tag = match other.kind {
            DefinedFunctionKind::Copied { .. } => 0,
            DefinedFunctionKind::IndirectTrampoline { .. } => 1,
            DefinedFunctionKind::Trampoline { .. } => 2,
        };

        match (tag, self.input_func_id).cmp(&(other_tag, other.input_func_id)) {
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

pub(crate) struct GotBase {
    lib_base_id: OutputGlobalId,
    table_base_id: OutputGlobalId,
}

struct SubModuleExtra {
    self_base: GotBase,
    entrypoints: Vec<InputFuncId>,
    extern_modules: Vec<(SharedModuleIdentifier, GotBase)>,
    export_got_with_id: Option<SharedModuleIdentifier>,
}
impl SubModuleExtra {
    const MAIN_GLOBAL_EXPORTS: &[&str] = &["__stack_pointer"]; // "__data_end", "__heap_base" - i
    const MAIN_GLOBAL_EXPORTS_COUNT: u32 = Self::MAIN_GLOBAL_EXPORTS.len() as u32;
}

// 'any are used because associated types are invariant, and used in default impls for Indexed Vec/Map impls.
pub struct ModuleEmitState<'any, 'src> {
    functions: WithOriginalIndex<'src, DefinedFunction>,

    // Global variables:
    // - lib_base_id for library base address (import)
    // - existing globals from src module
    // - "store" globals for `modify::constant_extractions`
    // - globals for data segments (lib_base_id + offset)
    globals: WithOriginalIndex<'src, DefinedGlobal<'src>>,
    pub global_tmp_store: HashMap<StoreType, OutputGlobalId>,
    // extra imports that should be emitted for lib
    // Not available for main module.
    sub_module_extra: Option<SubModuleExtra>,

    // Data Section
    data: IdVec<data_segments::DataSegmentOutput, DataSegmentId>,
    data_relocations: IdMap<DataSegmentId, Vec<modify::DataModifyEntry>>,

    // src module
    pub src: &'any analysis::ModuleInfo<'any, 'src>,
    // Generated fields:
    // Fields that calculated from other fields, and should be updated after any change.
    pub input_data_to_output_id: HashMap<DataId, (DataSegmentId, OutputSymbolDataId)>,
    // Indirect function table Functions from original table that are used in this module.
    pub indirect_functions: IndirectFunctionEmitInfo,
    linkage_type: LinkageType,
    pub linked_modules: Vec<SharedModuleIdentifier>,
    pub incremental_version: BumpVersion,
}

const MEMORY_INDEX: u32 = 0; //TODO: Support multiple memories
impl<'any, 'src> ModuleEmitState<'any, 'src> {
    pub fn produce_state(
        module_info: &'any analysis::ModuleInfo<'any, 'src>,
        emit_info: &'any CommonEmitInfo,
        // Merge use only output_module instead of program_info and output_module_index
        program_info: &SplitProgramInfo,
        output_module_index: usize,

        // Main module with static layout of memory and table.
        // None if it is main module.
        static_main: Option<&Self>,
        // deps of current module
        shared_modules: &[SharedModuleIdentifier],
        linkage_type: LinkageType,
        is_nonexported_fn: impl Fn(InputFuncId) -> bool,
    ) -> ModuleEmitState<'any, 'src> {
        let (module_id, output_module_info) = &program_info.output_modules[output_module_index];

        log::debug!("output_module_info: {output_module_info:#?}");
        log::debug!("module_id: {module_id:#?}");
        log::debug!("shared_modules: {shared_modules:#?}");
        // We need to include definitions for all of the `defined_symbols`.
        let mut funcs_to_define = HashSet::<(InputFuncId, bool)>::new();
        let mut import_functions = Vec::new();

        let mut indirect_funcs_stubs = Vec::new();
        let mut import_funcs_stubs = Vec::new();

        let main_module = output_module_index == 0;
        assert_eq!(
            main_module,
            static_main.is_none(),
            "Static main module should be provided for sub modules",
        );

        let mut used_funcs = HashSet::new();
        for func_id in output_module_info
            .defined_symbols
            .iter()
            .filter_map(DepNode::as_function)
        {
            if used_funcs.contains(&func_id) {
                continue;
            }
            used_funcs.insert(func_id);

            let need_export = {
                // if any module linked to current function
                let static_export = output_module_info
                    .exports
                    .contains(&DepNode::Function(func_id));
                // Or it is linked indirectly via split points
                let lazy_export = output_module_info
                    .split_points
                    .iter()
                    .any(|split_point| split_point.export_func == func_id);
                static_export || lazy_export
            };

            if emit_info.is_entrypoint_import_func(&func_id) {
                indirect_funcs_stubs.push((func_id, need_export));
            } else if let Some(import_id) = module_info.get_function_import_id(func_id) {
                let import_fn = module_info.wasm.imports[import_id];

                import_functions.push(ImportedFunction {
                    input_func_id: func_id,
                    kind: ImportFunctionKind::Existing {
                        module_name: import_fn.module,
                        import_function_name: import_fn.name,
                    },
                });

                if need_export {
                    import_funcs_stubs.push(func_id);
                }
            } else {
                funcs_to_define.insert((func_id, need_export));
            }
        }

        if !main_module {
            // submodule imports needed function from main module.
            import_functions.extend(
                output_module_info
                    .imports
                    .iter()
                    .inspect(|symbol| {
                        debug_assert!(!output_module_info.defined_symbols.contains(*symbol))
                    })
                    .filter_map(DepNode::as_function)
                    .map(|func_id| ImportedFunction {
                        input_func_id: func_id,
                        kind: ImportFunctionKind::New {
                            // TODO: support multiple dep modules
                            link_module: 0,
                            output_function_index: 0,
                            mangled_function_name: module_info
                                .wasm
                                .names
                                .functions
                                .get(func_id)
                                .expect("Function name should be defined"),
                        },
                    }),
            );
        }

        let imported_globals = if main_module {
            module_info
                .wasm
                .imports
                .iter()
                .filter_map(|(_id, import)| {
                    if let TypeRef::Global(global_type) = &import.ty {
                        Some((
                            wasm_encoder::reencode::RoundtripReencoder
                                .global_type(*global_type)
                                .expect("failed to reencode global type"),
                            import,
                        ))
                    } else {
                        None
                    }
                })
                .enumerate()
                .map(|(i, (ty, import))| GlobalImport::Existing {
                    global_name: import.name,
                    module_name: import.module,
                    input_global_id: Id::from_index(i),
                    global_type: ty,
                })
                .collect::<Vec<_>>()
        } else {
            SubModuleExtra::MAIN_GLOBAL_EXPORTS
                .iter()
                .map(|&name| {
                    let input_global_id =
                        module_info.find_global_id_by_name(name).unwrap_or_else(|| {
                            panic!(
                                "Globals {:?} should be defined in main module, {} is missing",
                                SubModuleExtra::MAIN_GLOBAL_EXPORTS,
                                name
                            )
                        });
                    GlobalImport::New {
                        input_global_id: Some(input_global_id),
                        global_name: Cow::Borrowed(name),
                        global_type: GlobalType {
                            val_type: wasm_encoder::ValType::I32,
                            // TODO: only __stack_pointer should be mutable
                            mutable: true,
                            shared: false,
                        },
                    }
                })
                .collect::<Vec<_>>()
        };

        let defined_globals: Vec<_> = if main_module {
            module_info
                .wasm
                .globals
                .iter()
                .map(|(id, global)| DefinedGlobal::PlainCopy {
                    global: global.clone(),
                    input_global_id: id,
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut globals = ImportsOrDefined::new(imported_globals, defined_globals);

        let lib_base_import = (!main_module).then(|| {
            globals.push_import(GlobalImport::New {
                input_global_id: None,
                global_name: Cow::Borrowed("__lib_base"),
                global_type: GlobalType {
                    val_type: wasm_encoder::ValType::I32,
                    mutable: false,
                    shared: false,
                },
            })
        });

        let mut data_to_define = BTreeMap::new();
        for (data_segment_id, data_symbol_id) in output_module_info
            .defined_symbols
            .iter()
            .filter_map(DepNode::as_data_symbol)
        {
            data_to_define
                .entry(data_segment_id)
                .or_insert_with(HashSet::new)
                .insert(data_symbol_id);
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

        let mut data_segment_outputs = IdVec::new();

        let mem_start = if main_module {
            let first_segment = data_segments
                .iter()
                .next()
                .expect("There should be at least one data segment")
                .1;
            first_segment.memory_offset()
        } else {
            0
        };

        // offset of current segment.
        let mut segment_mem_offset = 0;
        log::trace!("Data segments for module: {:#?}", data_segments);
        for (segment_id, segment) in data_segments.iter() {
            let lib_base_global_id = lib_base_import.as_ref().map(|id| id.as_raw_index() as u32);

            let (new_segment_offset, out) =
                segment.to_lib_output(lib_base_global_id, mem_start, segment_mem_offset);
            // TODO: apply relocations to data segment
            if out.is_active() {
                segment_mem_offset = new_segment_offset + out.as_raw().len();
            }

            data_segment_outputs.push(out);
        }

        // TODO: replace with is_symbol_static (is it located in main module?)
        let is_static_symbol = |symbol: AnySymbolId| match module_info
            .wasm
            .linking
            .linking_symbols
            .original_indexes
            .get(symbol)
        {
            Some(SymbolIndex::Func(f)) => static_main
                .as_ref()
                .unwrap()
                .functions
                .get_output_id(*f)
                .is_some(),
            Some(SymbolIndex::DataDefined(segment_id, symbol_id)) => {
                let main = static_main.as_ref().unwrap();
                main.input_data_to_output_id
                    .get(&(*segment_id, *symbol_id))
                    .is_some()
            }
            _ => return false,
        };

        let mut data_relocations = IdMap::new();

        // TODO: move shift in previous (segment_id, segment) in data_segments.iter()
        for (segment_id, data_segment) in data_segment_outputs.iter() {
            let segment_relocs = data_segment
                .relocations()
                .into_iter()
                .map(|reloc| {
                    let relocation_context = modify::RelocationContext {
                        dyn_relocate: !main_module
                            && !is_static_symbol(reloc.entry.index as AnySymbolId),
                        containing_symbol: Some(modify::DataSymbolWithOffset {
                            storage_segment_id: segment_id,
                            storage_symbol_id: reloc.reloc_in_symbol_index,
                            storage_offset_in_data: reloc.offset,
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

        let mut defined_functions = vec![];

        for &(func_id, mut export) in &funcs_to_define {
            let defined_id = module_info.as_defined_function_id(func_id).unwrap();
            // Collect all relocation entries that modify something within this function.
            let func_info = &module_info.wasm.code.defined_funcs[defined_id];
            let range = func_info.body.range();
            let func_relocs = Self::get_relocations_for_range(&emit_info.all_relocations, &range);

            let modification_list = func_relocs
                .iter()
                .map(|entry| {
                    // TODO: Avoid cloning?
                    let entry = entry.shift_left(range.start);
                    let relocation_context = modify::RelocationContext {
                        dyn_relocate: !main_module && !is_static_symbol(entry.index as AnySymbolId),
                        containing_symbol: None,
                    };
                    modify::CodeModifyEntry::from_relocation_entry(&entry, &relocation_context)
                })
                .collect::<Result<Vec<_>, _>>()
                .unwrap();

            // TODO: Add trampoline for __wasm_bindgen_ functions that for some reasons exported.
            // For now there known to be only `wasm_bindgen::__rt::wbg_cast::breaks_if_inlined::`
            // special functions are exported trough trampolines
            if export && is_nonexported_fn(func_id) {
                defined_functions.push(DefinedFunction {
                    export: true,
                    input_func_id: func_id,
                    kind: DefinedFunctionKind::Trampoline {},
                });
                export = false
            }

            defined_functions.push(DefinedFunction {
                export,
                input_func_id: func_id,
                kind: DefinedFunctionKind::Copied { modification_list },
            });
        }

        defined_functions.extend(indirect_funcs_stubs.iter().map(
            |(input_func_id, need_export)| DefinedFunction {
                export: *need_export,
                input_func_id: *input_func_id,
                kind: DefinedFunctionKind::IndirectTrampoline {
                    table_index_offset: emit_info.entrypoint_index(input_func_id).unwrap(),
                },
            },
        ));

        defined_functions.extend(
            import_funcs_stubs
                .iter()
                .map(|input_func_id| DefinedFunction {
                    export: true,
                    input_func_id: *input_func_id,
                    kind: DefinedFunctionKind::Trampoline {},
                }),
        );

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

        let funcs = ImportsOrDefined::new(import_functions, defined_functions).lock();

        let indirect_function_table: Vec<_> = module_info
            .indirect_function_list
            .iter()
            .filter(|indirect_func_id| funcs.get_output_id(**indirect_func_id).is_some())
            .copied()
            .collect();

        let indirect_functions = IndirectFunctionEmitInfo::new(
            main_module.then(|| emit_info.num_entrypoints()),
            indirect_function_table,
        );

        let sub_module_extra = lib_base_import.map(|lib_base| {
            let table_base = globals.push_import(GlobalImport::New {
                input_global_id: None,
                global_name: Cow::Borrowed("__table_base"),
                global_type: wasm_encoder::GlobalType {
                    val_type: wasm_encoder::ValType::I32,
                    mutable: false,
                    shared: false,
                },
            });

            let entrypoints = output_module_info
                .split_points
                .iter()
                .map(|sp| sp.export_func)
                .collect::<Vec<_>>();

            let extern_modules = shared_modules
                .iter()
                .map(|module_id| {
                    let got_base = GotBase {
                        lib_base_id: globals.push_import(GlobalImport::New {
                            input_global_id: None,
                            global_name: Cow::Owned(format!(
                                "__{}_lib_base",
                                module_id.to_string()
                            )),
                            global_type: wasm_encoder::GlobalType {
                                val_type: wasm_encoder::ValType::I32,
                                mutable: false,
                                shared: false,
                            },
                        }),
                        table_base_id: globals.push_import(GlobalImport::New {
                            input_global_id: None,
                            global_name: Cow::Owned(format!(
                                "__{}_table_base",
                                module_id.to_string()
                            )),
                            global_type: wasm_encoder::GlobalType {
                                val_type: wasm_encoder::ValType::I32,
                                mutable: false,
                                shared: false,
                            },
                        }),
                    };
                    (module_id.clone(), got_base)
                })
                .collect::<Vec<_>>();

            let export_got_with_id = module_id.as_shared().cloned();
            SubModuleExtra {
                self_base: GotBase {
                    lib_base_id: lib_base,
                    table_base_id: table_base,
                },
                extern_modules,
                entrypoints,
                export_got_with_id,
            }
        });

        let mut global_tmp_store = HashMap::new();
        if !main_module {
            for (store_type, val_type) in init_each_store_var() {
                let global_id = globals.imports.len() + globals.defined.len();
                global_tmp_store.insert(store_type, OutputGlobalId::from_index(global_id));

                globals.defined.push(DefinedGlobal::WithConstructor(
                    GlobalConstructor::TempStore(GlobalType {
                        val_type,
                        mutable: true,
                        shared: false,
                    }),
                ));
            }
        }
        // dbg!(&globals);

        Self {
            src: module_info,
            data: data_segment_outputs,
            data_relocations,
            globals: globals.lock(),
            sub_module_extra,
            global_tmp_store,
            input_data_to_output_id,
            indirect_functions,
            functions: funcs,
            linkage_type,
            linked_modules: shared_modules.to_vec(),
            incremental_version: Default::default(),
        }
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
        self.sub_module_extra.is_none()
    }

    fn _num_extra_global_imports(&self) -> usize {
        (!self.is_main())
            .then(|| SubModuleExtra::MAIN_GLOBAL_EXPORTS_COUNT as usize + 2)
            .unwrap_or(0)
    }

    fn generate(
        &'any self,
        computed_modules: &'any ComputedModules<'any, 'src>,
        output_module: &mut wasm_encoder::Module,
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

        let code_relocs = self.generate_code_section(computed_modules, output_module)?;
        let data_relocs = self.generate_data_section(computed_modules, output_module)?;

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
        for (_id, input_func_type) in self.src.wasm.types.iter() {
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
            let ty = wasm_encoder::EntityType::Function(
                self.get_function_type(index).as_raw_index() as u32,
            );
            let fn_name = import_fn.import_name();
            let module_name = import_fn.module_name();
            section.import(&module_name, &fn_name, ty);
        }

        match &self.sub_module_extra {
            None => {
                // Copy all non-function imports from input.
                for (_id, import) in self.src.wasm.imports.iter() {
                    if matches!(
                        import.ty,
                        wasmparser::TypeRef::Func(_) | wasmparser::TypeRef::Global(_)
                    ) {
                        continue;
                    }
                    let ty: wasm_encoder::EntityType = import.ty.clone().try_into().unwrap();
                    section.import(import.module, import.name, ty);
                }
            }

            Some(sub_module_extra) => {
                // Import all globals that are exported from main module.
                for (id, item) in self.globals.imports() {
                    section.import(
                        item.module_name().as_ref(),
                        item.import_name().as_ref(),
                        *item.global_type(),
                    );
                }

                section.import(
                    "__wasm_split",
                    "__indirect_function_table",
                    computed_modules
                        .main_module
                        .indirect_functions
                        .calculate_indirect_function_table_type(),
                );

                // Import all memories defined by the input module.
                for (memory_index, memory) in self.src.wasm.memories.iter() {
                    let ty: wasm_encoder::MemoryType = memory.clone().try_into().unwrap();
                    section.import(
                        "__wasm_split",
                        self.get_memory_name(memory_index).as_str(),
                        ty,
                    );
                }
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

        self.src.get_function_type_id(input_func_id)
    }
    // Get name of output function by index.
    // Used in generating exports and names section.
    // TODO: Use for generating import sections as well?
    fn get_function_name(&self, index: OutputFuncId, exported: bool) -> Cow<'src, str> {
        let input_func_id = self._get_input_func_id(index);
        let mut name = self
            .src
            .wasm
            .names
            .functions
            .get(input_func_id)
            .map(|name| (*name).into())
            .unwrap_or_else(|| format!("func_{index}").into());

        let namespace = exported
            || matches!(
                self.functions
                    .get_defined_for_output_id(index)
                    .map(|def| &def.kind),
                // modify name for import stubs to avoid conflicts
                Some(DefinedFunctionKind::Trampoline { .. })
                    | Some(DefinedFunctionKind::IndirectTrampoline { .. })
            );

        if namespace {
            name = format!("__wamex_{}", name).into()
        }
        name
    }

    fn get_global_name(&self, index: InputGlobalId) -> Cow<'src, str> {
        self.src
            .wasm
            .names
            .globals
            .get(index)
            .map(|name| (*name).into())
            .or_else(|| {
                self.src
                    .export_map
                    .get(&(
                        wasmparser::ExternalKind::Global as isize,
                        index.as_raw_index(), // TODO: convert indexes?
                    ))
                    .map(|(_, name)| (*name).into())
            })
            .unwrap_or_else(|| format!("__global_{index}").into())
    }

    fn get_memory_name(&self, index: MemoryId) -> String {
        self.src
            .wasm
            .names
            .memories
            .get(index)
            .map(|name| name.to_string())
            .or_else(|| {
                self.src
                    .export_map
                    .get(&(
                        wasmparser::ExternalKind::Memory as isize,
                        index.as_raw_index(), // TODO: convert indexes?
                    ))
                    .map(|(_, name)| name.to_string())
            })
            .unwrap_or_else(|| format!("__memory_{index}"))
    }
    fn generate_export_section(&self, output_module: &mut wasm_encoder::Module) {
        let mut section = wasm_encoder::ExportSection::new();
        let mut existing_exports = HashSet::<borrow::Cow<'_, str>>::new();
        // left original exports as is (because this module should be drop-in replacement)
        if self.is_main() {
            for (_id, export) in self.src.wasm.exports.iter() {
                let mut index = export.index;
                if export.kind == wasmparser::ExternalKind::Func {
                    let Some(func_id) = self._get_output_func_id(InputFuncId::from_index(index))
                    else {
                        continue;
                    };
                    index = func_id.as_raw_index() as u32;
                }
                section.export(export.name, export.kind.try_into().unwrap(), index);
                existing_exports.insert(export.name.into());
            }
        }

        for (func_id, func) in self.functions.defined() {
            if !func.export {
                continue;
            }
            let mut name = self.get_function_name(func_id, true);

            if existing_exports.contains(&name) {
                continue;
            }
            section.export(
                &name,
                wasm_encoder::ExportKind::Func,
                func_id.as_raw_index() as u32,
            );
        }

        match &self.sub_module_extra {
            Some(extra) => {
                if let Some(export_got_with_id) = &extra.export_got_with_id {
                    let lib_base_name = format!("__{}_lib_base", export_got_with_id.to_string());
                    let table_base_name =
                        format!("__{}_table_base", export_got_with_id.to_string());
                    if existing_exports.contains(lib_base_name.as_str())
                        || existing_exports.contains(table_base_name.as_str())
                    {
                        panic!("GOT base globals {lib_base_name} or {table_base_name} already exist in exports");
                    }
                    // Export GOT base globals.
                    section.export(
                        &lib_base_name,
                        wasm_encoder::ExportKind::Global,
                        extra.self_base.lib_base_id.as_raw_index() as u32,
                    );
                    section.export(
                        &table_base_name,
                        wasm_encoder::ExportKind::Global,
                        extra.self_base.table_base_id.as_raw_index() as u32,
                    );
                    existing_exports.insert(lib_base_name.into());
                    existing_exports.insert(table_base_name.into());
                }
            }
            None => {
                // Export globals.
                let white_list = SubModuleExtra::MAIN_GLOBAL_EXPORTS;
                for (global_index, _) in self.src.wasm.globals.iter() {
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
                        global_index.as_raw_index() as u32,
                    );
                    existing_exports.insert(name.into());
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
        for (fn_id, fn_type) in self.src.wasm.types.iter() {
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
        if !self.is_main() {
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
                Ok(output_func_id.as_raw_index() as u32)
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
            log::error!(
                "table_base id: {}",
                sub_module_extra.self_base.table_base_id
            );
            log::error!("lib_base id: {}", sub_module_extra.self_base.lib_base_id);
            wasm_encoder::ConstExpr::global_get(
                sub_module_extra.self_base.table_base_id.as_raw_index() as u32,
            )
        } else {
            wasm_encoder::ConstExpr::i32_const(1 as i32) // skip empty entry at index 0 for main module
        };

        let func_ids = self._function_ids_for_element_section()?;
        Self::_generate_element_section_segment(&mut section, &element_start, func_ids);

        // generate empty entries for lazy entrypoints
        match &self.sub_module_extra {
            None => {
                let abort_fn_id = 0u32; // TODO: Place real abort function
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
                            output_func_id.as_raw_index() as u32
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
        if self.src.wasm.memories.is_empty() {
            return;
        }
        let mut section = wasm_encoder::MemorySection::new();
        for (_idx, memory) in self.src.wasm.memories.iter() {
            section.memory(memory.clone().try_into().unwrap());
        }
        output_module.section(&section);
    }

    fn generate_global_section(&self, output_module: &mut wasm_encoder::Module) -> Result<()> {
        let mut section = wasm_encoder::GlobalSection::new();
        for (id, global) in self.globals.defined() {
            match global {
                DefinedGlobal::PlainCopy { global, .. } => {
                    section.global(
                        global.ty.clone().try_into().unwrap(),
                        &global.init_expr.clone().try_into().unwrap(),
                    );
                }
                DefinedGlobal::WithConstructor(new) => {
                    if let Some(v) = &self.sub_module_extra {
                        section.global(
                            new.global_type(),
                            &new.global_init(v.self_base.lib_base_id.as_raw_index() as u32),
                        );
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
        input_func_id: InputFuncId,
        table_index: u32,
    ) -> Result<Vec<RelocationEntry>> {
        let func_type_id = &self.src.get_function_type_id(input_func_id);
        let func_type = &self.src.wasm.types[*func_type_id];

        let mut func = wasm_encoder::Function::new([]);
        for (param_i, _param_type) in func_type.params().iter().enumerate() {
            func.instruction(&wasm_encoder::Instruction::LocalGet(param_i as u32));
        }
        func.instruction(&wasm_encoder::Instruction::I32Const(table_index as i32));
        func.instruction(&wasm_encoder::Instruction::CallIndirect {
            type_index: func_type_id.as_raw_index() as u32,
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
        input_func_id: InputFuncId,
    ) -> Result<Vec<RelocationEntry>> {
        let func_type_id = &self.src.get_function_type_id(input_func_id);
        let func_type = &self.src.wasm.types[*func_type_id];

        let import_fn = self
            ._get_output_func_id(input_func_id)
            .expect("Imported function should have output id");

        let mut func = wasm_encoder::Function::new([]);
        for (param_i, _param_type) in func_type.params().iter().enumerate() {
            func.instruction(&wasm_encoder::Instruction::LocalGet(param_i as u32));
        }
        func.instruction(&wasm_encoder::Instruction::Call(
            import_fn.as_raw_index() as u32
        ));
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
        input_func_id: InputFuncId,
        modification_list: &[modify::CodeModifyEntry],
    ) -> Result<Vec<RelocationEntry>> {
        let mut code_relocs = Vec::new();
        let defined_id = self
            .src
            .as_defined_function_id(input_func_id)
            .expect("Defined function expected");

        let global_id_mapper = |global_id: InputGlobalId| self.globals.get_output_id(global_id);
        // TODO: collect relocations.

        let (result, modified_relocs) = ModifyContext::emit_code_with_changes(
            self,
            computed_modules,
            global_id_mapper,
            defined_id,
            input_func_id,
            &modification_list,
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
                DefinedFunctionKind::IndirectTrampoline { table_index_offset } => {
                    // +1 for empty first entry
                    let num_entrypoints =
                        self.indirect_functions.function_table_index.len() as u32 + 1;
                    // TODO: generate stubs for imported functions.
                    self._generate_indirect_stub_function(
                        &mut section,
                        output_func.input_func_id,
                        num_entrypoints + *table_index_offset,
                    )
                }
                DefinedFunctionKind::Copied { modification_list } => {
                    let function_start_offset =
                        encoding_size(defined_functions_count) + section.byte_len();
                    self._generate_defined_function(
                        &mut section,
                        computed_modules,
                        function_start_offset,
                        output_func.input_func_id,
                        modification_list,
                    )
                }
            };

            code_relocs.extend(relocs?);
        }

        if let Some(_) = &self.sub_module_extra {
            let relocate = RelocateState {
                input_module: self.src.wasm,
                computed_modules,
                emit_module: self,
                global_id_mapper: &|global_id: InputGlobalId| self.globals.get_output_id(global_id),
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
    ) -> Result<()> {
        // let num_new_global_imports = self._num_extra_global_imports();
        let mut section = wasm_encoder::DataSection::new();

        for (id, out) in self.data.iter() {
            let mut data = out.data_segment(MEMORY_INDEX);
            let relocs = self.data_relocations.get(id).unwrap();

            for entry in relocs.iter() {
                let state = modify::StartFnModifyContext {
                    data_segment: &mut data.data,
                    relocate: RelocateState {
                        input_module: self.src.wasm,
                        computed_modules,
                        emit_module: self,
                        global_id_mapper: &|global_id: InputGlobalId| {
                            self.globals.get_output_id(global_id)
                        },
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
        let mut features = self.src.wasm.target_features.clone();
        features.features.extended_const = true;
        output_module.section(&features.encode_custom_section());
        Ok(())
    }

    fn generate_dylink0_section(
        &'any self,
        output_module: &mut wasm_encoder::Module,
    ) -> Result<()> {
        if !self.is_main() {
            let data = wamex_metadata::dylink0::Dylink0Section {
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
        shifted_code_relocs: Vec<RelocationEntry>,
        shifted_data_relocs: Vec<RelocationEntry>,
    ) -> Result<()> {
        let wamex_version = wasm_encoder::CustomSection {
            name: "__wamex_version".into(),
            data: self.incremental_version.encode().to_vec().into(),
        };

        output_module.section(&wamex_version);

        let mut functions = wasm_encoder::NameMap::new();
        for output_id in self.functions.iter_all_ids() {
            let name = self.get_function_name(output_id, false);

            functions.append(output_id.as_raw_index() as u32, &name);
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
        for custom in &self.src.wasm.custom_sections {
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
    pub num_extra_stubs: u64,
}

impl IndirectFunctionEmitInfo {
    fn new(num_extra_stubs: Option<u64>, table_entries: Vec<InputFuncId>) -> Self {
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
        let indirect_table_size = self.table_entries.len() as u64 + 1 + &self.num_extra_stubs; // reserve space for stubs at start

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
    // All relocations, ordered by offset, which are relative to the start of
    // the file rather than the start of the section.
    // TODO: move to analysis
    pub all_relocations: Vec<RelocationEntry>,

    pub src_data_segments: IdVec<DataSegment<'src>>,

    // Imports (corresponding to split points) to exclude from all modules.
    pub split_point_imports: BTreeSet<InputFuncId>,
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
        self.modules_decl.get(&module_id).map(|r| {
            let start = stubs_start + r.split_points_offset;
            let end = stubs_start + r.split_points_offset + r.split_points.len() as u32;
            start..end
        })
    }
    fn entrypoint_index(&self, entrypoint_func: &InputFuncId) -> Option<u32> {
        self.modules_decl.values().find_map(|module| {
            module
                .split_points
                .iter()
                .position(|sp| &sp.import_func == entrypoint_func)
                .map(|pos| module.split_points_offset + pos as u32)
        })
    }

    fn is_entrypoint_import_func(&self, import_fn: &InputFuncId) -> bool {
        self.split_point_imports.contains(import_fn)
    }

    fn num_entrypoints(&self) -> u64 {
        self.split_point_imports.len() as u64
    }

    fn new(
        module: &analysis::ModuleInfo<'_, 'src>,
        program_info: &SplitProgramInfo,
    ) -> Result<Self> {
        let all_relocations = Self::all_relocations(module.wasm)?;
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
                split_point_imports.insert(split_point.import_func);
            }
        }

        // re-build data_segments (using only available symbols)
        let data_segments_symbols = module
            .data_symbols
            .chunk_by(|left, right| left.segment_index == right.segment_index)
            .collect::<Vec<_>>();
        let data_segments: IdVec<DataSegment<'src>> = module
            .wasm
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
                let segment_info = module.wasm.linking.segments_info[data_segment].clone();

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
        log::warn!("Data segments: \n {print_data_format}");
        Ok(CommonEmitInfo {
            all_relocations,
            split_point_imports,
            src_data_segments: data_segments,
            modules_decl,
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

const MAIN_ID: SplitModuleIdentifier = SplitModuleIdentifier::Single(ModuleIdentifier::Main);

struct ComputedModules<'a, 'src> {
    main_module: ModuleEmitState<'a, 'src>,
    shared_modules: BTreeMap<SharedModuleIdentifier, ModuleEmitState<'a, 'src>>,
    sub_modules: BTreeMap<ModuleIdentifier, ModuleEmitState<'a, 'src>>,
}

impl<'a, 'src> ComputedModules<'a, 'src> {
    pub fn produce_state(
        common_emit_info: &'a CommonEmitInfo<'src>,
        module: &'a analysis::ModuleInfo<'a, 'src>,
        program_info: &SplitProgramInfo,
        is_nonexported_fn: impl Fn(InputFuncId) -> bool + Copy,
    ) -> Result<Self> {
        let modules_ids_iter = program_info.output_modules.iter().enumerate().filter_map(
            |(output_module_index, (id, _))| {
                Some((output_module_index, id.clone()))
                // // Skip shared modules, they are processed inside modules.
                // SplitModuleIdentifier::as_single(id).map(|id| (output_module_index, id.clone()))
            },
        );

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
                log::debug!("Calculating module {id:?}");
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
                        &common_emit_info,
                        program_info,
                        output_module_index,
                        None,
                        &NO_DEPS,
                        linkage_type,
                        is_nonexported_fn,
                    ),
                    id,
                )
            })
            .expect("Main module not found");

        let all_sub_modules = modules_ids_iter
            .into_iter()
            .filter(|(_output_module_index, id)| *id != MAIN_ID)
            .map(|(output_module_index, id)| {
                log::debug!("Calculating module {id:?}");
                let stubs_start = main_module.0.indirect_functions.table_entries.len() + 1;

                let table_range = if let SplitModuleIdentifier::Single(id) = &id {
                    common_emit_info
                        .module_entrypoints_range_shifted(stubs_start as u32, &id)
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

                let module_deps = Self::find_module_deps(id.clone(), &all_shared_deps);
                (
                    ModuleEmitState::produce_state(
                        module,
                        &common_emit_info,
                        program_info,
                        output_module_index,
                        Some(&main_module.0),
                        &module_deps,
                        linkage_type,
                        is_nonexported_fn,
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

    fn find_module_deps(
        interested_module: SplitModuleIdentifier,
        shared_modules: &[SharedModuleIdentifier],
    ) -> Vec<SharedModuleIdentifier> {
        let mut result = Vec::new();
        for shared_module in shared_modules {
            if matches!(&interested_module, SplitModuleIdentifier::Shared(our_module) if shared_module == our_module)
            {
                continue; // skip self
            }
            if interested_module.is_part_of(shared_module) {
                result.push(shared_module.clone());
            }
        }

        result
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
        mut emit_fn: impl FnMut(&SplitModuleIdentifier, &[u8]) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        for (identifier, state) in self.iter_modules() {
            log::debug!("Generating module {identifier:?}");

            let mut encoder = wasm_encoder::Module::new();
            state
                .generate(&self, &mut encoder)
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
    let (before, main_module, after) = {
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
    let is_imported_by_other = |node: &DepNode| {
        before
            .iter()
            .chain(after.iter())
            .any(|(_, mod_state)| mod_state.imports.contains(node))
            || after
                .iter()
                .any(|(_, mod_state)| mod_state.imports.contains(node))
    };

    #[cfg(debug_assertions)]
    let mut check_imports = vec![];

    for (_, mut shared_module) in shared_with_main {
        debug_assert!(shared_module.split_points.is_empty());

        for node in &shared_module.exports {
            // it was exported in shared module, so on main side it had been imported.
            // remove from main link symbols.
            if !main_module.imports.remove(&node) {
                log::warn!("Shared module symbol not found in main: {node:?}");
            }
            // This was imported not only by main, so export is needed.
            if is_imported_by_other(&node) {
                main_module.exports.insert(node.clone());
            }
        }

        // imported modules should already be in main
        #[cfg(debug_assertions)]
        for node in &shared_module.imports {
            check_imports.push(node.clone());
        }

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

fn is_wasm_bindgen_descriptor(name: &str) -> bool {
    name == "__wbindgen_describe_closure" || name == "__wbindgen_describe"
}

// Process all functions that have dependencies on wasm-bindgen describe.
// Move their definition to main module.
// Returns set of moved functions. So main module can generate stubs for them.
pub fn hoist_wbg_deps_to_main<'a, 'src>(
    module: &'a analysis::ModuleInfo<'a, 'src>,
    graph: &DepGraph,
    program_info: &mut SplitProgramInfo,
) -> HashSet<InputFuncId> {
    let wbg_fns: HashSet<_> = module
        .wasm
        .names
        .functions
        .iter()
        .filter(|(_id, name)| is_wasm_bindgen_descriptor(name))
        .map(|(id, _name)| id)
        .collect();

    let mut to_move = HashSet::new();
    for func in wbg_fns {
        if !to_move.insert(func) {
            continue;
        }
        if let Some(parents) = graph.get_parents(&DepNode::Function(func)) {
            for parent in parents {
                let DepNode::Function(parent) = parent else {
                    panic!("Non-function parent for wbg function: {parent:?}");
                };
                to_move.insert(*parent);
            }
        }
    }

    for (id, output_module) in program_info
        .output_modules
        .iter_mut()
        .filter(|(id, _m)| *id != MAIN_ID)
    {
        for moved_fn in to_move.iter() {
            // definitions are moved to main
            if output_module
                .defined_symbols
                .remove(&DepNode::Function(*moved_fn))
            {
                let fn_name = module.wasm.names.functions.get(*moved_fn);
                log::debug!(
                    "Moving function {:?} ({}) from module {} to main module",
                    fn_name,
                    moved_fn,
                    id.name()
                );

                output_module.exports.remove(&DepNode::Function(*moved_fn));
                output_module.imports.insert(DepNode::Function(*moved_fn));
            }
        }
    }
    let main_module = &mut program_info
        .output_modules
        .iter_mut()
        .find(|(id, _)| *id == MAIN_ID)
        .expect("Main module not found")
        .1;

    // remove main linkage to moved functions (if any), since it is now defined in main
    for moved_fn in &to_move {
        main_module.imports.remove(&DepNode::Function(*moved_fn));
    }

    main_module
        .defined_symbols
        .extend(to_move.iter().map(|id| DepNode::Function(*id)));

    to_move
}

pub fn emit_modules<'a, 'src>(
    module: &'a analysis::ModuleInfo<'a, 'src>,
    program_info: &SplitProgramInfo,
    wbg_fns: &HashSet<InputFuncId>,
    emit_fn: impl FnMut(&SplitModuleIdentifier, &[u8]) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let emit_info = CommonEmitInfo::new(&module, program_info)?;
    let calculated = ComputedModules::produce_state(&emit_info, module, program_info, |func_id| {
        wbg_fns.contains(&func_id)
    })
    .context("Error calculating modules")?;
    calculated.emit_modules(emit_fn)?;
    Ok(())
}
