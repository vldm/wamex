//! Methods to apply relocations.
//! This method contains two type of relocation methods:
//! - convenient methods, that can be found at: https://github.com/WebAssembly/tool-conventions/blob/main/Linking.md
//!    it is primary used for function relocation.
//! - constant replacement technique, that swaps all usage of constant offsets embeded in code with a global variable binding.
//!
//!

mod encode;
mod function;

use std::{
    collections::HashMap,
    ops::{Deref, DerefMut, Range},
};

use anyhow::{bail, Result};
use wasm_encoder::Encode;
use wasmparser::RelocationType;

use crate::index::{GlobalId, SymbolId};

#[derive(Debug, Clone, PartialEq, Eq)]
struct DataRelocationEntry {
    relocation_type: RelocationType,
    global_index: u32,
    /// Addend to add to the address, or `0` if not applicable. The value must
    /// be consistent with the `self.ty.addend_kind()`.
    addend: i64,
    range: Range<usize>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct RelocationEntryWithRange {
    entry: wasmparser::RelocationEntry,
    range: Range<usize>,
}

#[derive(Debug, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub enum StoreType {
    I32Store,
    I64Store,
    F32Store,
    F64Store,
}
#[derive(Debug)]
struct RelocationContext<'a> {
    function_name: &'a str,
    src_body: &'a [u8],
    writter: &'a mut Vec<u8>,
    global_tmps: &'a HashMap<StoreType, GlobalId>,
}
impl<'a> RelocationContext<'a> {
    fn reref<'b>(&'b mut self) -> RelocationContext<'b> {
        RelocationContext {
            function_name: self.function_name,
            src_body: self.src_body,
            writter: self.writter,
            global_tmps: self.global_tmps,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelocationEntry {
    Data(DataRelocationEntry),
    Other(RelocationEntryWithRange),
}

impl RelocationEntry {
    fn shift_range(range: Range<usize>, start_offset: usize) -> Range<usize> {
        (range.start - start_offset)..(range.end - start_offset)
    }

    fn from_relocation_entry(
        global_getter: impl Fn(SymbolId) -> Result<GlobalId>,
        entry: wasmparser::RelocationEntry,
        start_offset: usize,
    ) -> Result<RelocationEntry> {
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
            | RelocationType::MemoryAddrI32 => RelocationEntry::Data(DataRelocationEntry {
                relocation_type: entry.ty,
                global_index: global_getter(entry.index as SymbolId)? as u32,
                addend: entry.addend,
                range: Self::shift_range(entry.relocation_range(), start_offset),
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
            | RelocationType::SectionOffsetI32 => {
                RelocationEntry::Other(RelocationEntryWithRange {
                    entry,
                    range: Self::shift_range(entry.relocation_range(), start_offset),
                })
            }
        })
    }
    fn range(&self) -> &Range<usize> {
        match self {
            RelocationEntry::Data(data) => &data.range,
            RelocationEntry::Other(other) => &other.range,
        }
    }
}

fn emit_code_with_relocation(
    mut ctx: RelocationContext<'_>,
    entries: &[RelocationEntry],
) -> Result<()> {
    let mut last_range = 0..0;
    for entry in entries {
        if entry.range().start > last_range.end {
            bail!("Data relocations out of order");
        }
        if entry.range().end > ctx.src_body.len() {
            bail!("Data relocations out of bounds");
        }
        ctx.writter
            .extend_from_slice(&ctx.src_body[last_range.end..entry.range().start]);

        match entry {
            RelocationEntry::Data(data) => match data.relocation_type {
                RelocationType::MemoryAddrLeb => {
                    replace_memory_offset_with_global_get(ctx.reref(), &data)?
                }
                RelocationType::MemoryAddrSleb => {
                    replace_const_get_with_global_get(ctx.reref(), &data)?
                }
                _ => {
                    bail!("Unsupported relocation type")
                }
            },
            RelocationEntry::Other(other) => {
                ctx.writter
                    .extend_from_slice(&ctx.src_body[other.range.start..other.range.end]);
                todo!()
            }
        }

        last_range = entry.range().clone();
    }

    Ok(())
}

fn init_each_store_var() -> Vec<(StoreType, wasm_encoder::ValType)> {
    vec![
        (StoreType::I32Store, wasm_encoder::ValType::I32),
        (StoreType::I64Store, wasm_encoder::ValType::I64),
        (StoreType::F32Store, wasm_encoder::ValType::F32),
        (StoreType::F64Store, wasm_encoder::ValType::F64),
    ]
}

// Global get with 5 bytes argument
fn encode_global_get(global_index: u32) -> [u8; 6] {
    let mut result = [0; 6];
    result[0] = 0x23; // global.get opcode
    result[1..].copy_from_slice(&leb128fmt::encode_fixed_u32(global_index).unwrap());
    result
}

fn replace_const_get_with_global_get(
    ctx: RelocationContext<'_>,
    entry: &DataRelocationEntry,
) -> Result<()> {
    use wasm_encoder::Instruction;
    use wasmparser::Operator;

    let range = entry.range.start - 2..entry.range.end;
    let ix_data = &ctx.src_body[range.clone()];

    let operand = wasmparser::BinaryReader::new(&*ix_data, 0).read_operator()?;
    let ix = match operand {
        Operator::I32Const { value } => Instruction::I32Const(value),
        // Operator::I64Const { value } => Instruction::I64Const(value),
        _ => {
            bail!("Unsupported relocation operand: {operand:?}")
        }
    };
    if entry.addend != 0 {
        bail!("Unsupported relocation addend");
    }

    let result_ix: [u8; 6] = encode_global_get(entry.global_index);
    log::trace!(
        "Replacing func[{name}:{range:?}] {ix:?} with {result_ix:?}",
        name = ctx.function_name
    );
    debug_assert_eq!(result_ix.len(), ix_data.len());
    ctx.writter.extend_from_slice(&result_ix);

    Ok(())
}
fn replace_memory_offset_with_global_get(
    ctx: RelocationContext<'_>,
    entry: &DataRelocationEntry,
) -> Result<()> {
    let range = entry.range.start - 1..entry.range.end;
    let ix_data = &ctx.src_body[range.clone()];

    use wasm_encoder::Instruction;
    use wasmparser::Operator;
    let fix_offset = |memarg: wasmparser::MemArg| -> wasm_encoder::MemArg {
        let mut memargs: wasm_encoder::MemArg = memarg.into();
        memargs.offset = entry.addend as u64;
        memargs
    };

    let operand = wasmparser::BinaryReader::new(&*ix_data, 0).read_operator()?;
    let (store, ix) = match operand {
        Operator::F32Load { memarg } => (None, Instruction::F32Load(fix_offset(memarg.into()))),
        Operator::F64Load { memarg } => (None, Instruction::F64Load(fix_offset(memarg.into()))),
        Operator::I32Load { memarg } => (None, Instruction::I32Load(fix_offset(memarg.into()))),
        Operator::I64Load { memarg } => (None, Instruction::I64Load(fix_offset(memarg.into()))),
        Operator::I32Load8U { memarg } => (None, Instruction::I32Load8U(fix_offset(memarg.into()))),
        Operator::I32Load8S { memarg } => (None, Instruction::I32Load8S(fix_offset(memarg.into()))),
        Operator::I32Load16U { memarg } => {
            (None, Instruction::I32Load16U(fix_offset(memarg.into())))
        }
        Operator::I32Load16S { memarg } => {
            (None, Instruction::I32Load16S(fix_offset(memarg.into())))
        }
        Operator::I64Load8U { memarg } => (None, Instruction::I64Load8U(fix_offset(memarg.into()))),
        Operator::I64Load8S { memarg } => (None, Instruction::I64Load8S(fix_offset(memarg.into()))),
        Operator::I64Load16U { memarg } => {
            (None, Instruction::I64Load16U(fix_offset(memarg.into())))
        }
        Operator::I64Load16S { memarg } => {
            (None, Instruction::I64Load16S(fix_offset(memarg.into())))
        }
        Operator::I64Load32U { memarg } => {
            (None, Instruction::I64Load32U(fix_offset(memarg.into())))
        }
        Operator::I64Load32S { memarg } => {
            (None, Instruction::I64Load32S(fix_offset(memarg.into())))
        }

        Operator::I32Store { memarg } => (
            Some(StoreType::I32Store),
            Instruction::I32Store(fix_offset(memarg.into())),
        ),
        Operator::I64Store { memarg } => (
            Some(StoreType::I64Store),
            Instruction::I64Store(fix_offset(memarg.into())),
        ),
        Operator::I32Store8 { memarg } => (
            Some(StoreType::I32Store),
            Instruction::I32Store8(fix_offset(memarg.into())),
        ),
        Operator::I32Store16 { memarg } => (
            Some(StoreType::I32Store),
            Instruction::I32Store16(fix_offset(memarg.into())),
        ),
        Operator::I64Store8 { memarg } => (
            Some(StoreType::I64Store),
            Instruction::I64Store8(fix_offset(memarg.into())),
        ),
        Operator::I64Store16 { memarg } => (
            Some(StoreType::I64Store),
            Instruction::I64Store16(fix_offset(memarg.into())),
        ),
        Operator::I64Store32 { memarg } => (
            Some(StoreType::I64Store),
            Instruction::I64Store32(fix_offset(memarg.into())),
        ),

        _ => {
            bail!("Unsupported relocation operand: {operand:?}")
        }
    };

    // Get value to temp storage
    // TODO: Replace with local?
    match &store {
        None => {}
        Some(store_type) => {
            Instruction::GlobalSet(*ctx.global_tmps.get(store_type).unwrap() as u32)
                .encode(ctx.writter);
        }
    }
    Instruction::Drop.encode(ctx.writter); // remove i32.const(0) from stack
    Instruction::GlobalGet(entry.global_index as u32).encode(ctx.writter); // replace it with global variable

    // Recover back global variable
    match &store {
        None => {}
        Some(store_type) => {
            Instruction::GlobalGet(*ctx.global_tmps.get(store_type).unwrap() as u32)
                .encode(ctx.writter);
        }
    }
    // And now push original instruction with only append left in offset
    ix.encode(ctx.writter);

    log::trace!(
        "Replacing func[{name}:{range:?}] {ix:?} with {store:?} ix",
        name = ctx.function_name
    );

    Ok(())
}
