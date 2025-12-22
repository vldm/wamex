use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    fmt::{Debug, Display},
};

use cranelift_entity::{EntityRef, SecondaryMap};
use wamex_types::{BumpVersion, map_vec::MiniSet};
use wasm_encoder::{GlobalType, reencode::Reencode};

use crate::{
    InputObject,
    emit::{
        CommonEmitInfo, DefinedFunction, DefinedFunctionKind, GotBase, ImportFunctionKind,
        ImportedFunction, IndirectFunctionEmitInfo, LinkageType, ModuleEmitState, SegmentLayout,
        SubModuleExtra,
        builder::{BuilderContextToBeRemoved, ObjectBuilder},
        globals::{DefinedGlobal, GlobalImport},
        index_safety::OutputGlobalId,
        modify::{self, init_each_store_var},
    },
    read::{
        ImportOrDefined,
        typed::{FunctionRef, GlobalRef},
    },
    symbols::{SymbolId, SymbolKind},
};

pub const WAMEX_ENTRY_PREFIX: &str = "__wamex_00";
pub const SPLIT_IMPORT_POSTFIX: &str = "00_import_";
pub const SPLIT_EXPORT_POSTFIX: &str = "00_export_";

pub fn parser<'a>(name: &'a str, prefix: &str, postfix: &str) -> Option<(&'a str, &'a str)> {
    if !name.starts_with(prefix) {
        return None;
    }
    let name = &name[prefix.len()..];
    let postfix_index = name.find(postfix)?;
    let module_name = &name[..postfix_index];
    let fn_name = &name[postfix_index + postfix.len()..];

    Some((module_name, fn_name))
}

pub fn parse_wamex_entry_name(name: &str) -> Option<(&str, &str)> {
    if let Some(v) = parser(name, WAMEX_ENTRY_PREFIX, SPLIT_IMPORT_POSTFIX) {
        return Some(v);
    }
    parser(name, WAMEX_ENTRY_PREFIX, SPLIT_EXPORT_POSTFIX)
}

/// Split-point entrypoint pair (import stub + export impl).
///
/// Note: the algorithm that discovers split points lives in `wamex-cli`.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct SplitPoint {
    pub module_name: String,
    pub unique_id: String,
    pub import_func: FunctionRef,
    pub export_func: FunctionRef,
}

impl SplitPoint {
    pub fn import_func(&self) -> FunctionRef {
        self.import_func
    }
    pub fn export_func(&self) -> FunctionRef {
        self.export_func
    }
}

/// Content plan for a single emitted module.
#[derive(Default, Clone)]
pub struct OutputModuleInfo {
    pub defined_symbols: BTreeSet<SymbolId>,
    pub imports: MiniSet<SymbolId>,
    pub exports: MiniSet<SymbolId>,
    pub split_points: Vec<SplitPoint>,
}

impl Debug for OutputModuleInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "OutputModuleInfo {{")?;
        Self::fmt_table(f, "defined_symbols", self.defined_symbols.iter())?;
        Self::fmt_table(f, "imports", self.imports.iter())?;
        Self::fmt_table(f, "exports", self.exports.iter())?;
        writeln!(f, "  split_points: {:?},", self.split_points)?;
        write!(f, "}}")
    }
}

impl OutputModuleInfo {
    pub fn need_export(&self, symbol: &SymbolId, input_func_id: FunctionRef) -> bool {
        // if any module linked to current function
        let static_export = self.exports.contains(symbol);
        // Or it is linked indirectly via split points
        let lazy_export = self
            .split_points
            .iter()
            .any(|split_point| split_point.export_func() == input_func_id);
        static_export || lazy_export
    }

    fn fmt_table<'a, T, I>(
        f: &mut std::fmt::Formatter<'_>,
        label: &str,
        items: I,
    ) -> std::fmt::Result
    where
        T: Debug,
        I: IntoIterator<Item = T>,
    {
        writeln!(f, "  {label}:")?;
        let rendered: Vec<String> = items.into_iter().map(|item| format!("{item:?}")).collect();
        if rendered.is_empty() {
            writeln!(f, "    <empty>")?;
            return Ok(());
        }

        let width = rendered.iter().map(|item| item.len()).max().unwrap_or(0);
        for chunk in rendered.chunks(10) {
            write!(f, "    ")?;
            for (idx, item) in chunk.iter().enumerate() {
                if idx > 0 {
                    write!(f, " ")?;
                }
                write!(f, "{item:<width$}")?;
            }
            writeln!(f)?;
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone)]
pub enum ModuleIdentifier {
    Main,
    Split(String),
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone)]
pub struct SharedModuleIdentifier(pub Vec<ModuleIdentifier>);

impl Display for SharedModuleIdentifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut components = self.0.iter();
        let Some(first) = components.next() else {
            return Ok(());
        };

        write!(f, "{}", first)?;
        components.try_for_each(|n| write!(f, "_{}", n))?;
        Ok(())
    }
}

impl SharedModuleIdentifier {
    pub fn contains(&self, module: &ModuleIdentifier) -> bool {
        self.0.iter().any(|m| m == module)
    }

    pub fn remove(&mut self, module: &ModuleIdentifier) -> bool {
        let original_len = self.0.len();
        self.0.retain(|m| m != module);
        original_len != self.0.len()
    }

    pub fn includes(&self, other: &SplitModuleIdentifier) -> bool {
        match other {
            SplitModuleIdentifier::Single(name) => self.contains(name),
            SplitModuleIdentifier::Shared(shared) => {
                shared.0.iter().all(|name| self.contains(name))
            }
        }
    }
}

impl PartialEq<SplitModuleIdentifier> for SharedModuleIdentifier {
    fn eq(&self, other: &SplitModuleIdentifier) -> bool {
        match other {
            SplitModuleIdentifier::Single(_) => false,
            SplitModuleIdentifier::Shared(shared) => shared == self,
        }
    }
}

impl<'a> IntoIterator for &'a SharedModuleIdentifier {
    type Item = &'a ModuleIdentifier;
    type IntoIter = std::slice::Iter<'a, ModuleIdentifier>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone)]
pub enum SplitModuleIdentifier {
    Single(ModuleIdentifier),
    Shared(SharedModuleIdentifier),
}

impl Default for SplitModuleIdentifier {
    fn default() -> Self {
        Self::Single(ModuleIdentifier::Main)
    }
}

impl Display for ModuleIdentifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Main => write!(f, "main"),
            Self::Split(name) => write!(f, "{}", name),
        }
    }
}

impl Display for SplitModuleIdentifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Single(name) => Display::fmt(name, f),
            Self::Shared(name) => Display::fmt(name, f),
        }
    }
}

impl SplitModuleIdentifier {
    pub fn as_single(&self) -> Option<&ModuleIdentifier> {
        match self {
            Self::Single(name) => Some(name),
            Self::Shared(_) => None,
        }
    }

    pub fn as_shared(&self) -> Option<&SharedModuleIdentifier> {
        match self {
            Self::Single(_) => None,
            Self::Shared(name) => Some(name),
        }
    }

    pub fn is_shared(&self) -> bool {
        matches!(self, Self::Shared(_))
    }

    pub fn is_main(&self) -> bool {
        matches!(self, Self::Single(ModuleIdentifier::Main))
    }

    pub fn is_part_of(&self, other: &SharedModuleIdentifier) -> bool {
        match self {
            Self::Single(name) => other.contains(name),
            Self::Shared(shared) => shared.0.iter().all(|name| other.contains(name)),
        }
    }

    pub fn collect_deps(
        &self,
        shared_modules: &[SharedModuleIdentifier],
    ) -> Vec<SharedModuleIdentifier> {
        let mut result = Vec::new();
        for shared_module in shared_modules {
            if matches!(&self, SplitModuleIdentifier::Shared(our_module) if shared_module == our_module)
            {
                continue;
            }
            if self.is_part_of(shared_module) {
                result.push(shared_module.clone());
            }
        }
        result
    }
}

/// Emission plan for a full split program.
///
/// Note: the algorithm that computes this structure lives in `wamex-cli`.
#[derive(Debug, Default)]
pub struct SplitProgramInfo {
    pub output_modules: Vec<(SplitModuleIdentifier, OutputModuleInfo)>,
    pub symbol_output_module: SecondaryMap<SymbolId, usize>,
}

/// Helpers used by `wamex-cli` diff/incremental logic.
#[derive(Clone, Debug)]
pub struct ModuleSnapshot {
    pub defined_symbols: BTreeSet<SymbolId>,
    pub imports: MiniSet<SymbolId>,
    pub exports: MiniSet<SymbolId>,
    pub split_points: Vec<SplitPoint>,
}

impl From<&OutputModuleInfo> for ModuleSnapshot {
    fn from(value: &OutputModuleInfo) -> Self {
        Self {
            defined_symbols: value.defined_symbols.clone(),
            imports: value.imports.clone(),
            exports: value.exports.clone(),
            split_points: value.split_points.clone(),
        }
    }
}

pub fn merge_split_points_by_module_name<'a>(
    split_points: &'a [SplitPoint],
) -> BTreeMap<&'a str, Vec<&'a SplitPoint>> {
    let mut result = BTreeMap::<&'a str, Vec<&'a SplitPoint>>::new();
    for split_point in split_points {
        result
            .entry(split_point.module_name.as_str())
            .or_default()
            .push(split_point);
    }
    for results in result.values_mut() {
        results.sort_by_key(|sp| sp.unique_id.as_str());
    }
    result
}

struct SplitContext<'any, 'src> {
    module_info: &'any InputObject<'src>,
    emit_info: &'any CommonEmitInfo<'src>,
    static_symbols: &'any BTreeSet<SymbolId>,
    nonexported_symbols: &'any MiniSet<SymbolId>,
    main_module: bool,
}
impl<'src> SplitContext<'_, 'src> {
    fn is_nonexportable(&self, symbol: SymbolId) -> bool {
        self.nonexported_symbols.contains(&symbol)
    }
    fn is_static_symbol(&self, symbol: SymbolId) -> bool {
        self.static_symbols.contains(&symbol)
    }
    // TODO: part of element layout
    fn external_entrypoint_index(&self, input_func_id: FunctionRef) -> Option<u32> {
        self.emit_info.external_entrypoint_index(input_func_id)
    }
}

pub struct Split<S> {
    state: S,
}

impl<'src> Split<ObjectBuilder<'src>> {
    fn include_function_by_symbol(
        &mut self,
        ctx: &SplitContext<'_, 'src>,
        sym: SymbolId,
        input_func_id: FunctionRef,
        export: bool,
    ) {
        // This function entrypoint of external module.
        // Rewrite it's import to call specific indirect trampoline.
        if let Some(table_index_offset) = ctx.external_entrypoint_index(input_func_id) {
            self.state.add_defined_function(DefinedFunction {
                export,
                input_func_id,
                kind: DefinedFunctionKind::IndirectTrampoline { table_index_offset },
            });
            return;
        }
        match ctx.module_info.functions.items.get_entity(input_func_id) {
            // This is imported function
            // Currently only possible in main module
            ImportOrDefined::Import(import_info) => {
                debug_assert!(ctx.main_module);

                self.state.add_imported_function(ImportedFunction {
                    input_func_id,
                    kind: ImportFunctionKind::Existing(import_info.clone()),
                });

                // For reexport add trampoline that rename import under wamex namespace.
                // E.g. `import foo.bar` becomes `export __wamex__.bar`
                // TODO: remove this trampoline, since it is just a loader limitation.
                if export {
                    self.state.add_defined_function(DefinedFunction {
                        export: true,
                        input_func_id: input_func_id,
                        kind: DefinedFunctionKind::Trampoline {},
                    });
                }
            }
            ImportOrDefined::Defined(_) => {
                // Collect all relocation entries that modify something within this function.
                let func_relocs = &*ctx.module_info.symbols.get(sym).unwrap().relocs;

                let modification_list = func_relocs
                    .iter()
                    .map(|entry| {
                        let dyn_base = !ctx.main_module
                            && !ctx.is_static_symbol(SymbolId::from_u32(entry.index));
                        let relocation_context = modify::RelocationContext {
                            dyn_base,
                            containing_symbol: None,
                        };
                        modify::CodeModifyEntry::from_relocation_entry(&entry, &relocation_context)
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();

                // allow unsetting if trampoline is defined
                let mut need_export = export;

                // Add trampoline for __wasm_bindgen_ functions that are deps to other modules.
                // It is used for closures, see: `wasm_bindgen::__rt::wbg_cast::breaks_if_inlined::`
                // This special functions are exported trough trampolines
                if need_export && ctx.is_nonexportable(sym) {
                    self.state.add_defined_function(DefinedFunction {
                        export: true,
                        input_func_id,
                        kind: DefinedFunctionKind::Trampoline {},
                    });
                    need_export = false
                }

                self.state.add_defined_function(DefinedFunction {
                    export: need_export,
                    input_func_id,
                    kind: DefinedFunctionKind::Copied { modification_list },
                });
            }
        }
    }

    fn copy_src_globals(&mut self, ctx: &SplitContext<'_, 'src>) {
        debug_assert!(ctx.main_module);
        let mut input_global_id = GlobalRef::from_u32(0);
        for (_id, import) in ctx.module_info.wasm_reader.imports.iter() {
            let wasmparser::TypeRef::Global(global_type) = &import.ty else {
                continue;
            };

            let global = GlobalImport::Existing {
                global_name: import.name,
                module_name: import.module,
                input_global_id,
                global_type: wasm_encoder::reencode::RoundtripReencoder
                    .global_type(*global_type)
                    .expect("failed to reencode global type"),
            };

            self.state.add_imported_global(global);
            input_global_id = input_global_id.next();
        }
        for (id, global) in ctx.module_info.globals.defined_iter() {
            let global = DefinedGlobal::PlainCopy {
                global: global.clone(),
                // TODO: This is probably incorrect so i left panic in case if some global is imported - for now, and will fix with test
                input_global_id: id,
            };
            self.state.add_defined_global(global);
            #[cfg(debug_assertions)]
            {
                let imports_globals = ctx
                    .module_info
                    .wasm_reader
                    .imports
                    .iter()
                    .filter_map(|(_, import)| match &import.ty {
                        wasmparser::TypeRef::Global(_) => Some(()),
                        _ => None,
                    })
                    .count();
                assert!(
                    imports_globals == 0,
                    "Imported globals are not supported in main module yet"
                )
            }
        }
    }
    fn add_submodule_global_imports(&mut self, ctx: &SplitContext<'_, 'src>) {
        debug_assert!(!ctx.main_module);

        for name in SubModuleExtra::MAIN_GLOBAL_EXPORTS.iter() {
            let input_global_id =
                ctx.module_info
                    .find_global_id_by_name(name)
                    .unwrap_or_else(|| {
                        panic!(
                            "Globals {:?} should be defined in main module, {} is missing",
                            SubModuleExtra::MAIN_GLOBAL_EXPORTS,
                            name
                        )
                    });
            let global = GlobalImport::New {
                input_global_id: Some(input_global_id),
                global_name: Cow::Borrowed(name),
                global_type: GlobalType {
                    // TODO: real type
                    val_type: wasm_encoder::ValType::I32,
                    // TODO: only __stack_pointer should be mutable
                    mutable: true,
                    shared: false,
                },
            };
            self.state.add_imported_global(global);
        }
    }
}
impl Split<()> {
    pub fn build_split_object<'any, 'src>(
        module_info: &'any InputObject<'src>,
        verbose: bool,
        emit_info: &CommonEmitInfo<'src>,
        (module_id, output_module_info): &(SplitModuleIdentifier, OutputModuleInfo),
        // deps of current module
        shared_modules: &[SharedModuleIdentifier],
        linkage_type: LinkageType,
        static_symbols: &BTreeSet<SymbolId>,
        nonexported_symbols: &MiniSet<SymbolId>,
        version: BumpVersion,
    ) -> ModuleEmitState<'any, 'src> {
        log::debug!("output_module_info: {output_module_info:#?}");
        // log::debug!("module_id: {module_id:#?}");
        log::debug!("shared_modules: {shared_modules:#?}");
        let main_module = matches!(
            module_id,
            SplitModuleIdentifier::Single(ModuleIdentifier::Main)
        );

        let ctx = SplitContext {
            module_info,
            emit_info,
            main_module,
            static_symbols,
            nonexported_symbols,
        };

        let mut builder = Split {
            state: ObjectBuilder::new(),
        };

        // TODO: make it hard error, to do so, we need to resolve synonym symbols first
        let mut used_funcs = BTreeSet::new();
        let mut data_to_define = BTreeMap::new();

        for &sym in output_module_info.defined_symbols.iter() {
            let input_func_id = match module_info.symbols.get(sym).expect("Symbol not found").kind {
                SymbolKind::DataDefined { segment_id, .. } => {
                    // Process as "segment" chunk.
                    data_to_define
                        .entry(segment_id)
                        .or_insert_with(BTreeSet::new)
                        .insert(sym);
                    continue;
                }
                SymbolKind::Func { input_id } => input_id,
                _ => panic!("Unexpected symbol kind"),
            };
            if !used_funcs.insert(input_func_id) {
                // duplicate symbol for same function, skip
                continue;
            }
            let need_export = output_module_info.need_export(&sym, input_func_id);
            builder.include_function_by_symbol(&ctx, sym, input_func_id, need_export);
        }

        // submodule imports needed function from main module.
        if !main_module {
            for &symbol in output_module_info.imports.iter() {
                debug_assert!(!output_module_info.defined_symbols.contains(&symbol));
                let Some(input_func_id) = module_info.symbols.as_input_function(symbol) else {
                    //TODO: Check other imported symbols (globals, table, data)
                    continue;
                };

                let import_fn = ImportedFunction {
                    input_func_id,
                    kind: ImportFunctionKind::New {
                        // TODO: support multiple dep modules
                        link_module: 0,
                        output_function_index: 0,
                        mangled_function_name: module_info
                            .wasm_reader
                            .names
                            .functions
                            .get(input_func_id)
                            .expect("Function name should be defined"),
                    },
                };
                builder.state.add_imported_function(import_fn);
            }
        }

        for (segment_id, src_segment) in emit_info.src_data_segments.iter() {
            let symbols_in_segment =
                if let Some(symbols_in_segment) = data_to_define.get(&segment_id) {
                    symbols_in_segment
                } else {
                    &Default::default()
                };
            builder.state.add_prebuilt_data_segment(
                src_segment.clone().new_with_whitelist(symbols_in_segment),
            );
        }

        if verbose {
            SegmentLayout::debug_layout(
                &module_info.symbols,
                module_id.to_string(),
                &builder.state.data,
            );
        }

        if main_module {
            builder.copy_src_globals(&ctx);
        } else {
            builder.add_submodule_global_imports(&ctx);
        }

        let sub_module_extra = (!main_module).then(|| {
            let lib_base = builder.state.add_imported_global(GlobalImport::New {
                input_global_id: None,
                global_name: Cow::Borrowed("__lib_base"),
                global_type: GlobalType {
                    val_type: wasm_encoder::ValType::I32,
                    mutable: false,
                    shared: false,
                },
            });

            let table_base = builder.state.add_imported_global(GlobalImport::New {
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
                .map(|sp| sp.export_func())
                .collect::<Vec<_>>();

            let extern_modules = shared_modules
                .iter()
                .map(|module_id| {
                    let got_base = GotBase {
                        lib_base_id: builder.state.add_imported_global(GlobalImport::New {
                            input_global_id: None,
                            global_name: Cow::Owned(format!("__{}_lib_base", module_id)),
                            global_type: wasm_encoder::GlobalType {
                                val_type: wasm_encoder::ValType::I32,
                                mutable: false,
                                shared: false,
                            },
                        }),
                        table_base_id: builder.state.add_imported_global(GlobalImport::New {
                            input_global_id: None,
                            global_name: Cow::Owned(format!("__{}_table_base", module_id)),
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

        let mut global_tmp_store = BTreeMap::new();
        if !main_module {
            for (store_type, val_type) in init_each_store_var() {
                let global_id =
                    builder.state.globals.imports.len() + builder.state.globals.defined.len();
                global_tmp_store.insert(store_type, OutputGlobalId::new(global_id));

                builder
                    .state
                    .add_defined_global(DefinedGlobal::WithConstructor(GlobalType {
                        val_type,
                        mutable: true,
                        shared: false,
                    }));
            }
        }

        let ctx = BuilderContextToBeRemoved {
            module_info: &*module_info,
            sub_module_extra: &sub_module_extra,
            static_symbols: ctx.static_symbols,
        };
        // 2nd phase
        let builder = Split {
            state: builder.state.lock(ctx),
        };
        let indirect_function_table: Vec<_> = module_info
            .indirect_function_table
            .items
            .iter()
            .filter(|(_, indirect_func_id)| {
                builder
                    .state
                    .functions
                    .get_output_id(**indirect_func_id)
                    .is_some()
            })
            .map(|(_, &indirect_func_id)| indirect_func_id)
            .collect();

        let indirect_functions = IndirectFunctionEmitInfo::new(
            main_module.then(|| emit_info.num_entrypoints()),
            indirect_function_table,
        );

        ModuleEmitState {
            linked_modules: shared_modules.to_vec(),
            linkage_type,
            incremental_version: version,
            src: module_info,
            sub_module_extra,

            data: builder.state.data,
            data_relocations: builder.state.data_relocations,
            functions: builder.state.functions,
            globals: builder.state.globals,
            //TODO: should be a part of builder state
            global_tmp_store,
            indirect_functions,
        }
    }
}
