//! Methods to apply relocations and code replacements.
//! This module contains two type of methods:
//! - convenient relocation methods, that can be found at: https://github.com/WebAssembly/tool-conventions/blob/main/Linking.md
//! - constant replacement technique, that swaps all usage of constant offsets embeded in code with a global
//!   variable binding (something simmilar precalculated GOT + offset).
//!
//!

mod constant_extracton;
mod relocation;
mod start_fn_gen;

use std::{collections::BTreeMap, ops::Range};

use anyhow::{Result, bail};
use constant_extracton::ConstantExtractionEntry;
pub use relocation::RelocateState;
pub use start_fn_gen::{DataSymbolWithOffset, StartFnGen, StartFnModifyContext};
use wasm_encoder::reencode::Reencode;
use wasmparser::{BinaryReader, FunctionBody};

use crate::{
    emit::{ComputedModules, ModuleEmitState, index_safety::OutputGlobalId},
    index::{DefinedFuncId, InputFuncId, InputGlobalId},
};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum SymbolOp<V> {
    // Is symbol relative to GOT base (which is stored in global variable)
    GotBased { got: OutputGlobalId, value: V },
    // Symbol is on static offset (main module)
    StaticOffset { value: V },
}

impl<V> SymbolOp<V> {
    pub fn as_static(&self) -> Option<&V> {
        match self {
            SymbolOp::StaticOffset { value } => Some(value),
            SymbolOp::GotBased { .. } => None,
        }
    }
    pub fn map<U, F: FnOnce(V) -> U>(self, f: F) -> SymbolOp<U> {
        match self {
            SymbolOp::GotBased { got, value } => SymbolOp::GotBased {
                got,
                value: f(value),
            },
            SymbolOp::StaticOffset { value } => SymbolOp::StaticOffset { value: f(value) },
        }
    }
}

type SymbolOffset = SymbolOp<i64>;
type SymbolUOffset = SymbolOp<usize>;

#[derive(Debug)]
pub struct ModifyContext<'any, 'src> {
    // Function name for debug purposes
    pub function_name: &'any str,
    // Temporary globals for constant extraction
    pub global_tmps: &'any BTreeMap<StoreType, OutputGlobalId>,
    relocation_state: RelocateState<'any, 'src>,
    // Current instruction
    pub instruction: wasmparser::Operator<'any>,

    // TODO: remove
    // // Submodule GOT and Table base globals
    // pub lib_base_id: Option<OutputGlobalId>,
    // pub table_base_id: Option<OutputGlobalId>,

    // output writer
    pub writer: &'any mut Vec<u8>,
}
impl<'any, 'src> ModifyContext<'any, 'src> {
    pub fn emit_code_with_changes(
        module_emit: &'any ModuleEmitState<'any, 'src>,
        computed_modules: &'any ComputedModules<'any, 'src>,
        global_id_mapper: impl Fn(InputGlobalId) -> Option<OutputGlobalId>,
        defined_function_id: DefinedFuncId,
        input_function_id: InputFuncId, // debug purposes
        entries: &[CodeModifyEntry],
    ) -> Result<(Vec<u8>, Vec<wasmparser::RelocationEntry>)> {
        let reloc_info = RelocateState {
            input_module: &module_emit.src,
            computed_modules,
            emit_module: module_emit,
            global_id_mapper: &global_id_mapper,
        };

        let (function_name, src_body) = {
            let func_id = InputFuncId::from_index(
                defined_function_id.as_raw_index()
                    + module_emit.src.import_info.imported_funcs.len(),
            );
            let defined_func =
                &module_emit.src.wasm.code.section_payload.defined_funcs[defined_function_id];
            let name = module_emit
                .src
                .wasm
                .names
                .functions
                .get(func_id)
                .copied()
                .unwrap_or("__undefined_function");
            (name, defined_func.body.clone())
        };

        log::debug!(
            "processing function: {function_name}[{input_function_id}] for [{range:?}], entries: {debug_entries}",
            range = src_body.range(),
            debug_entries = Self::format_entries(entries)
        );

        log::trace!("src_body {:?}", src_body.as_bytes());
        let mut entries_iter = entries.iter().peekable();
        let Some(mut entry) = entries_iter.next() else {
            // no modifications, just copy the original function body
            log::trace!("no modifications, copying original function body");
            return Ok((src_body.as_bytes().to_vec(), Vec::new()));
        };
        // recreate binary reader to use function related offset rather than module related.
        let func_body = FunctionBody::new(BinaryReader::new(src_body.as_bytes(), 0));
        let mut locals = vec![];
        for local in func_body.get_locals_reader()? {
            let local = local?;
            let val_type = wasm_encoder::reencode::RoundtripReencoder.val_type(local.1)?;
            locals.push((local.0, val_type));
        }
        let mut result = wasm_encoder::Function::new(locals).into_raw_body();

        log::trace!("result: {:?}", result);

        let source = func_body.as_bytes();

        let mut other_relocations = vec![];

        let mut instr_iter = func_body.get_operators_reader()?;
        'instr: while !instr_iter.eof() {
            let start = instr_iter.original_position();
            let instr = instr_iter.read()?;
            let end = instr_iter.original_position();

            let instr_range = start..end;
            let entry_range = entry.range();

            let shift = result.len() as isize - instr_range.start as isize;
            let ctx = ModifyContext {
                function_name,
                global_tmps: &module_emit.global_tmp_store,
                instruction: instr.clone(),
                writer: &mut result,
                relocation_state: reloc_info.clone(),
            };

            log::trace!(
                "processing instruction: {instr:?}[{range:?}], entry: {entry:?}[{entry_range:?}]",
                instr = ctx.instruction,
                range = instr_range,
                entry = entry,
                entry_range = entry_range
            );

            // 1) if the entry ends before this instruction even began,
            //    that means the *previous* instruction should have caught it
            if entry_range.end <= instr_range.start {
                bail!(
                    "BUG: entry [{:?}] ended before this instruction [{start}..{end}]; \
                            it should have been handled already",
                    entry
                );
            }

            // 2) if the entry starts after this instruction ends, it simply
            //    doesn't belong here – move on to the next operator
            if entry_range.start >= instr_range.end {
                // copy original instruction
                ctx.writer.extend_from_slice(&source[instr_range.clone()]);
                continue;
            }

            // 3) if the entry now spans past the end of this instruction,
            //    it overlaps two instructions – that’s invalid
            if entry_range.end > instr_range.end {
                bail!(
                    "BUG: entry [{:?}] overlaps two instructions [{start}..{end}]",
                    entry
                );
            }

            // 4) at this point we know `entry_range.start >= instr_start`
            //    and `entry_range.end <= instr_end` – the entry is completely
            //    contained in this instruction
            //    → do your replacement here

            match entry {
                ModifyEntry::Custom(data) => data.try_apply(ctx)?,
                ModifyEntry::Other(other) => {
                    log::trace!("skiping modify entry {other:?} ");
                    other_relocations.push(wasmparser::RelocationEntry {
                        ty: other.ty,
                        index: other.index,
                        addend: other.addend,
                        offset: (other.offset as isize + shift).try_into()?,
                    });
                    ctx.writer.extend_from_slice(&source[instr_range.clone()]);

                    while let Some(ModifyEntry::Other(next_entry)) = entries_iter.peek() {
                        if next_entry.relocation_range().start >= instr_range.end {
                            break;
                        }
                        log::trace!("skiping modify entry {next_entry:?} ");
                        entries_iter.next();
                        other_relocations.push(wasmparser::RelocationEntry {
                            ty: next_entry.ty,
                            index: next_entry.index,
                            addend: next_entry.addend,
                            offset: (other.offset as isize + shift).try_into()?,
                        });
                    }
                }
            }

            let Some(next_entry) = entries_iter.next() else {
                // no more entries, just copy the rest of the function body
                log::trace!("no more entries, copying rest of the function body");

                result.extend_from_slice(&source[instr_range.end..]);
                break 'instr;
            };
            entry = next_entry;
        }

        log::trace!("end_body {:?}", result);
        log::trace!("other_relocations {:?}", other_relocations);
        // TODO: apply relocations
        for relocation in &other_relocations {
            log::trace!(
                "applying relocation {relocation:?} to function {function_name}",
                function_name = function_name
            );
            reloc_info.apply_relocation(&mut result, relocation)?;
        }
        Ok((result, other_relocations))
    }

    fn format_entries(entries: &[CodeModifyEntry]) -> String {
        use std::fmt::Write;
        let mut result = String::new();
        let mut new_line = false;
        for entry in entries {
            if new_line {
                writeln!(result, "").ok();
            }
            let reloc = match entry {
                ModifyEntry::Custom(c) => {
                    write!(result, "Custom {:?}", c).ok();
                    &c.entry
                }
                ModifyEntry::Other(o) => o,
            };
            write!(
                result,
                "Other {:?} index:{}, offset:{}, addend:{}",
                reloc.ty, reloc.index, reloc.offset, reloc.addend
            )
            .ok();
            new_line = true;
        }
        result
    }
    // Same as emit_code_with_changes, but avoid deserializing.
    pub fn emit_code_in_place(
        module_emit: &'any ModuleEmitState<'any, 'src>,
        computed_modules: &'any ComputedModules<'any, 'src>,
        global_id_mapper: impl Fn(InputGlobalId) -> Option<OutputGlobalId>,
        defined_function_id: DefinedFuncId,
        input_function_id: InputFuncId, // debug purposes
        entries: &[CodeModifyEntry],
    ) -> Result<(Vec<u8>, Vec<wasmparser::RelocationEntry>)> {
        let reloc_info = RelocateState {
            input_module: &module_emit.src,
            computed_modules,
            emit_module: module_emit,
            global_id_mapper: &global_id_mapper,
        };
        let (function_name, src_body) = {
            let func_id = InputFuncId::from_index(
                defined_function_id.as_raw_index()
                    + module_emit.src.import_info.imported_funcs.len(),
            );
            let defined_func =
                &module_emit.src.wasm.code.section_payload.defined_funcs[defined_function_id];
            let name = module_emit
                .src
                .wasm
                .names
                .functions
                .get(func_id)
                .copied()
                .unwrap_or("__undefined_function");
            (name, defined_func.body.clone())
        };
        log::debug!(
            "processing function: {function_name}[{input_function_id}] for [{range:?}], entries: {debug_entries}",
            range = src_body.range(),
            debug_entries = Self::format_entries(entries)
        );

        let src = src_body.as_bytes();
        log::trace!("src_body {:?}", src_body.as_bytes());

        let mut other_relocations = vec![];
        let mut result = Vec::new();

        let mut last_write = 0;
        for entry in entries {
            let shift = result.len() as isize - last_write as isize;

            let (range, data) = match entry {
                ModifyEntry::Custom(data) => {
                    let ix_size = match data.entry.ty {
                        wasmparser::RelocationType::MemoryAddrLeb => 2,
                        wasmparser::RelocationType::MemoryAddrSleb
                        | wasmparser::RelocationType::TableIndexSleb => 1,
                        _ => {
                            bail!("Unsupported relocation type")
                        }
                    };
                    let range = data.entry.relocation_range();
                    ((range.start - ix_size..range.end), data)
                }
                ModifyEntry::Other(other) => {
                    log::trace!("skiping modify entry {other:?} ");
                    other_relocations.push(wasmparser::RelocationEntry {
                        ty: other.ty,
                        index: other.index,
                        addend: other.addend,
                        offset: (other.offset as isize + shift).try_into()?,
                    });
                    continue;
                }
            };
            // copy remaining data before modification entry
            result.extend_from_slice(&src[last_write..range.start]);
            let instr = {
                let bin_reader = wasmparser::BinaryReader::new(&src[range.clone()], 0);
                let mut op_reader = wasmparser::OperatorsReader::new(bin_reader);
                op_reader.read()?
            };

            let ctx = ModifyContext {
                function_name,
                global_tmps: &module_emit.global_tmp_store,
                instruction: instr.clone(),
                writer: &mut result,
                relocation_state: reloc_info.clone(),
            };

            // process modification entry
            data.try_apply(ctx)?;
            last_write = range.end;
        }
        // copy remaining data after last modification entry
        result.extend_from_slice(&src[last_write..]);

        // process relocation
        for relocation in &other_relocations {
            log::trace!(
                "applying relocation {relocation:?} to function {function_name}",
                function_name = function_name
            );
            reloc_info.apply_relocation(&mut result, relocation)?;
        }
        Ok((result, other_relocations))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModifyEntry<C> {
    Custom(C),
    Other(wasmparser::RelocationEntry),
}

pub type CodeModifyEntry = ModifyEntry<ConstantExtractionEntry>;
pub type DataModifyEntry = ModifyEntry<start_fn_gen::DataEntry>;

pub struct RelocationContext {
    pub dyn_relocate: bool,
    // If relocation entry is in data segment - this is information about symbol
    pub containing_symbol: Option<DataSymbolWithOffset>,
}

pub trait CustomModify {
    type Context<'any, 'src>
    where
        'src: 'any;
    fn try_from_entry(
        entry: &wasmparser::RelocationEntry,
        context: &RelocationContext,
    ) -> Result<Option<Self>>
    where
        Self: Sized;

    fn range(&self) -> Range<usize>;

    fn try_apply(&self, ctx: Self::Context<'_, '_>) -> Result<()>;
}

impl<C: CustomModify> ModifyEntry<C> {
    pub fn from_relocation_entry(
        entry: &wasmparser::RelocationEntry,
        context: &RelocationContext,
    ) -> Result<Self> {
        C::try_from_entry(entry, context).map(|opt| match opt {
            Some(custom_entry) => ModifyEntry::Custom(custom_entry),
            None => ModifyEntry::Other(wasmparser::RelocationEntry {
                ty: entry.ty,
                index: entry.index,
                addend: entry.addend,
                offset: entry.offset,
            }),
        })
    }
    fn range(&self) -> Range<usize> {
        match self {
            ModifyEntry::Custom(data) => data.range(),
            ModifyEntry::Other(other) => other.relocation_range(),
        }
    }
}

#[derive(Debug, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub enum StoreType {
    I32,
    I64,
    F32,
    F64,
}

pub fn init_each_store_var() -> Vec<(StoreType, wasm_encoder::ValType)> {
    vec![
        (StoreType::I32, wasm_encoder::ValType::I32),
        (StoreType::I64, wasm_encoder::ValType::I64),
        (StoreType::F32, wasm_encoder::ValType::F32),
        (StoreType::F64, wasm_encoder::ValType::F64),
    ]
}
