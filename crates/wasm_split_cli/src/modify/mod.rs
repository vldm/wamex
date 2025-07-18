//! Methods to apply relocations.
//! This module contains two type of relocation methods:
//! - convenient methods, that can be found at: https://github.com/WebAssembly/tool-conventions/blob/main/Linking.md
//!    it is primary used for function relocation.
//! - constant replacement technique, that swaps all usage of constant offsets embeded in code with a global variable binding.
//!
//!

mod constant_extraction;
mod relocate_function;

use std::{collections::HashMap, iter::Peekable, ops::Range};

use anyhow::{bail, Result};
use wasm_encoder::Encode;
use wasmparser::{BinaryReader, FunctionBody, RelocationType};

use crate::{
    analysis::ModuleInfo,
    emit::ModuleEmitState,
    index::{GlobalId, SymbolId},
    modify::relocate_function::RelocateFunctionInfo,
};
use constant_extraction::ConstantExtractionEntry;

#[derive(Debug)]
pub struct ModifyContext<'a> {
    pub function_name: &'a str,
    pub global_tmps: &'a HashMap<StoreType, GlobalId>,
    pub instruction: wasmparser::Operator<'a>,
    pub writer: &'a mut Vec<u8>,
}
impl<'a> ModifyContext<'a> {
    pub fn emit_code_with_changes(
        module_emit: &ModuleEmitState<'a>,
        num_new_global_imports: u32,
        defined_function_id: GlobalId,
        entries: &[ModifyEntry],
    ) -> Result<Vec<u8>> {
        let (function_name, src_body) = {
            let func_id =
                defined_function_id + module_emit.info.import_funcs_info.imported_funcs.len();
            let defined_func =
                &module_emit.info.source.code.section_payload.defined_funcs[defined_function_id];
            let name = module_emit
                .info
                .source
                .names
                .functions
                .get(func_id)
                .copied()
                .unwrap_or_else(|| "__undefined_function");
            println!("original func_range: {:?}", defined_func.body.range());
            (name, defined_func.body.clone())
        };
        log::debug!(
            "processing function: {function_name}[{defined_function_id}] for [{range:?}]",
            range = src_body.range()
        );

        log::debug!("start_body {:?}", src_body.as_bytes());
        // recreate binary reader to use function related offset rather than module related.
        let func_body = FunctionBody::new(BinaryReader::new(src_body.as_bytes(), 0));
        let mut locals = vec![];
        for local in func_body.get_locals_reader()? {
            let local = local?;
            let val_type = match local.1 {
                wasmparser::ValType::I32 => wasm_encoder::ValType::I32,
                wasmparser::ValType::I64 => wasm_encoder::ValType::I64,
                wasmparser::ValType::F32 => wasm_encoder::ValType::F32,
                wasmparser::ValType::F64 => wasm_encoder::ValType::F64,
                wasmparser::ValType::V128 => wasm_encoder::ValType::V128,
                wasmparser::ValType::Ref(v) => wasm_encoder::ValType::Ref(wasm_encoder::RefType {
                    nullable: v.is_nullable(),
                    heap_type: match v.heap_type() {
                        wasmparser::HeapType::Abstract { shared, ty } => {
                            wasm_encoder::HeapType::Abstract {
                                shared,
                                ty: ty.into(),
                            }
                        }
                        wasmparser::HeapType::Concrete(c) => {
                            // TODO: remove unsafe
                            wasm_encoder::HeapType::Concrete(unsafe {
                                std::mem::transmute::<_, u32>(c.pack().unwrap())
                            })
                        }
                    },
                }),
            };
            locals.push((local.0, val_type));
        }
        let mut result = wasm_encoder::Function::new(locals).into_raw_body();

        let mut entries_iter = entries.iter().peekable();
        let Some(mut entry) = entries_iter.next() else {
            // no modifications, just copy the original function body
            log::debug!("no modifications, copying original function body");
            result.extend_from_slice(func_body.as_bytes());
            return Ok(result);
        };
        let source = func_body.as_bytes();

        let mut other_relocations = vec![];

        let mut instr_iter = func_body.get_operators_reader()?;
        'instr: while !instr_iter.eof() {
            let start = instr_iter.original_position();
            let instr = instr_iter.read()?;
            let end = instr_iter.original_position();

            let instr_range = start..end;
            let entry_range = entry.range();

            let shift = (result.len() as isize - instr_range.start as isize);
            let ctx = ModifyContext {
                function_name,
                global_tmps: &module_emit.global_tmp_store,
                instruction: instr.clone(),
                writer: &mut result,
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
                ModifyEntry::Data(data) => match data.relocation_type {
                    RelocationType::MemoryAddrLeb => {
                        data.replace_memory_offset_with_global_get(ctx)?
                    }
                    RelocationType::MemoryAddrSleb => {
                        data.replace_const_get_with_global_get(ctx)?
                    }
                    _ => {
                        bail!("Unsupported relocation type")
                    }
                },
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
                log::debug!("no more entries, copying rest of the function body");

                result.extend_from_slice(&source[instr_range.end..]);
                break 'instr;
            };
            entry = next_entry;
        }

        let reloc_info = RelocateFunctionInfo {
            input_module: &module_emit.info.source,
            emit_info: &module_emit.emit_info,
            input_function_output_id: &module_emit.input_function_output_id,
        };

        log::debug!("result {:?}", result);
        // TODO: apply relocations
        for relocation in other_relocations {
            log::debug!(
                "applying relocation {relocation:?} to function {function_name}",
                function_name = function_name
            );
            match relocation.ty {
                RelocationType::MemoryAddrLeb
                | RelocationType::MemoryAddrSleb
                | RelocationType::MemoryAddrI32 => {
                    bail!("Unsupported relocation type: {relocation:?}");
                }
                RelocationType::GlobalIndexLeb => {
                    // encode_leb128_u32_5byte(
                    //     self.get_relocated_function_index(relocation)? as u32,
                    //     target.try_into().unwrap(),
                    // );
                    continue;
                }

                _ => {}
            }
            reloc_info.apply_relocation(&mut result, 0, &relocation)?;
        }
        Ok(result)
    }

    // Same as emit_code_with_changes, but avoid deserializing.
    fn emit_code_in_place(
        module_emit: &ModuleEmitState<'a>,
        num_new_global_imports: u32,
        defined_function_id: GlobalId,
        entries: &[ModifyEntry],
    ) -> Result<Vec<u8>> {
        todo!()
        // let mut last_range = 0..0;
        // for entry in entries {
        //     if entry.range().start > last_range.end {
        //         bail!("Data relocations out of order");
        //     }
        //     if entry.range().end > ctx.src_body.len() {
        //         bail!("Data relocations out of bounds");
        //     }
        //     ctx.writter
        //         .extend_from_slice(&ctx.src_body[last_range.end..entry.range().start]);

        //     match entry {
        //         RelocationEntry::Data(data) => match data.relocation_type {
        //             RelocationType::MemoryAddrLeb => {
        //                 replace_memory_offset_with_global_get(ctx.reref(), &data)?
        //             }
        //             RelocationType::MemoryAddrSleb => {
        //                 replace_const_get_with_global_get(ctx.reref(), &data)?
        //             }
        //             _ => {
        //                 bail!("Unsupported relocation type")
        //             }
        //         },
        //         RelocationEntry::Other(other) => {
        //             ctx.writter
        //                 .extend_from_slice(&ctx.src_body[other.range.start..other.range.end]);
        //             todo!()
        //         }
        //     }

        //     last_range = entry.range().clone();
        // }

        // Ok(())
    }
}

#[derive(Debug, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub enum StoreType {
    I32Store,
    I64Store,
    F32Store,
    F64Store,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModifyEntry {
    Data(ConstantExtractionEntry),
    Other(wasmparser::RelocationEntry),
}

impl ModifyEntry {
    fn shift_range_left(range: Range<usize>, start_offset: usize) -> Range<usize> {
        (range.start - start_offset)..(range.end - start_offset)
    }

    pub fn from_relocation_entry(
        mut global_getter: impl FnMut(SymbolId) -> Result<GlobalId>,
        entry: &wasmparser::RelocationEntry,
        start_offset: usize,
    ) -> Result<ModifyEntry> {
        Ok(match entry.ty {
            RelocationType::MemoryAddrLeb64
            | RelocationType::MemoryAddrSleb64
            | RelocationType::MemoryAddrI64
            | RelocationType::MemoryAddrRelSleb64
            | RelocationType::MemoryAddrTlsSleb64
            | RelocationType::TableIndexSleb64
            | RelocationType::TableIndexI64
            | RelocationType::FunctionOffsetI64
            | RelocationType::TableIndexRelSleb64 => {
                bail!("U64 memory pointers is currently not supported")
            }
            RelocationType::MemoryAddrTlsSleb
            | RelocationType::MemoryAddrRelSleb
            | RelocationType::MemoryAddrLocrelI32 => {
                bail!("Relocation memory pointers is currently not supported")
            }

            RelocationType::MemoryAddrLeb
            | RelocationType::MemoryAddrSleb
            | RelocationType::MemoryAddrI32 => ModifyEntry::Data(ConstantExtractionEntry {
                relocation_type: entry.ty,
                global_index: global_getter(entry.index as SymbolId)? as u32,
                addend: entry.addend,
                range: Self::shift_range_left(entry.relocation_range(), start_offset),
            }),
            RelocationType::EventIndexLeb
            | RelocationType::TypeIndexLeb
            | RelocationType::TableIndexSleb
            | RelocationType::TableIndexI32
            | RelocationType::TableNumberLeb
            | RelocationType::FunctionIndexLeb
            | RelocationType::FunctionIndexI32
            | RelocationType::FunctionOffsetI32
            | RelocationType::TableIndexRelSleb
            | RelocationType::GlobalIndexLeb
            | RelocationType::GlobalIndexI32
            | RelocationType::SectionOffsetI32 => ModifyEntry::Other(wasmparser::RelocationEntry {
                ty: entry.ty,
                index: entry.index,
                addend: entry.addend,
                offset: entry.offset - start_offset as u32,
            }),
        })
    }
    fn range(&self) -> Range<usize> {
        match self {
            ModifyEntry::Data(data) => data.range.clone(),
            ModifyEntry::Other(other) => other.relocation_range(),
        }
    }
}

pub fn init_each_store_var() -> Vec<(StoreType, wasm_encoder::ValType)> {
    vec![
        (StoreType::I32Store, wasm_encoder::ValType::I32),
        (StoreType::I64Store, wasm_encoder::ValType::I64),
        (StoreType::F32Store, wasm_encoder::ValType::F32),
        (StoreType::F64Store, wasm_encoder::ValType::F64),
    ]
}
