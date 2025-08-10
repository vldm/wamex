//! Replace constants that are used to refer to data in data segments with global variable.

//! The major data are kept in "main" module, so we extract module related data, and dynamically allocate it.
//! In order to allow "sub" modules to access to their data, we patch the code to use global variables instead of constant offsets.
//! Later, during "sub" module loading, this global variables will be initialized with the offsets relative to their starting point.

use std::ops::Range;

use anyhow::{bail, Result};
use wasm_encoder::Encode;
use wasmparser::RelocationType;

use super::{ModifyContext, StoreType};
use crate::{emit::modify::CustomModify, helpers::RangeExt, index::SymbolId};

// Represents a data relocation entry with additional information about global variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstantExtractionEntry {
    pub(super) relocation_type: RelocationType,
    pub(super) global_index: GlobalVar,
    /// Addend to add to the address, or `0` if not applicable. The value must
    /// be consistent with the `self.ty.addend_kind()`.
    pub(super) addend: i64,
    pub(super) range: Range<usize>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlobalVar {
    /// Data segment extracted to global variable
    Extract(u32),
    /// Keep original constant value untouched
    Untouched,
}

impl Default for GlobalVar {
    fn default() -> Self {
        GlobalVar::Untouched
    }
}

impl ConstantExtractionEntry {
    // Simple replace of i32.const with global.get + i32.add
    // Retuns size of the replacement
    pub fn replace_const_get_with_global_get(&self, ctx: ModifyContext<'_>) -> Result<()> {
        use wasm_encoder::Instruction;
        use wasmparser::Operator;

        let GlobalVar::Extract(global_index) = self.global_index else {
            log::trace!("Skipping global get for func:{}", ctx.function_name);
            <Instruction<'_> as TryFrom<_>>::try_from(ctx.instruction)?.encode(ctx.writer);
            return Ok(());
        };
        let ix = match ctx.instruction {
            Operator::I32Const { value } => Instruction::I32Const(value),
            // Operator::I64Const { value } => Instruction::I64Const(value),
            _ => {
                bail!("Unsupported relocation operand: {:?}", ctx.instruction)
            }
        };
        let result_ix = Instruction::GlobalGet(global_index);
        log::trace!(
            "Replacing func[{name}:{range:?}] {ix:?} with {result_ix:?}, append {addend}",
            addend = self.addend,
            range = self.range,
            name = ctx.function_name
        );
        result_ix.encode(ctx.writer);
        if self.addend != 0 {
            Instruction::I32Const(self.addend as i32).encode(ctx.writer);
            Instruction::I32Add.encode(ctx.writer);
        }

        Ok(())
    }

    // replaces *.store and *.load family with multiple operations with `global` variable:
    // example for store:
    // > f32.store ($local_var + offset)  :stack[dyn_offset -> value]
    // <
    // < global.set $f32_global_index :stack[dyn_offset] // $f32_global_index = value
    // < global.get $global_var_index :stack[dyn_offset -> $global_var_index]
    // < f32.add :stack[modified_offset] // offset += $global_var_index
    // < global.get $f32_global_index :stack[modified_offset -> value] // return back value to stack
    // < f32.store offset // original f32.store with 0 base_offset
    // For load:
    // > i32.load_u8 ($local_var + offset) :stack[dyn_offset]
    // < global.get $global_var_index :stack[dyn_offset -> $global_var_index]
    // < i32.add :stack[modified_offset] // offset += $global_var_index
    // < i32.load_u8 offset // original i32.load_u8 with 0 base_offset

    /// Returns size of the replacement.
    pub fn replace_memory_offset_with_global_get(&self, ctx: ModifyContext<'_>) -> Result<()> {
        use wasm_encoder::Instruction;
        use wasmparser::Operator;
        let fix_offset = |memarg: wasmparser::MemArg| -> wasm_encoder::MemArg {
            let mut memargs = wasm_encoder::MemArg {
                align: memarg.align as u32,
                offset: self.addend as u64,
                memory_index: memarg.memory,
            };
            memargs.offset = self.addend as u64;
            memargs
        };

        let GlobalVar::Extract(global_index) = self.global_index else {
            log::trace!("Skipping global get for func:{}", ctx.function_name);
            <Instruction<'_> as TryFrom<_>>::try_from(ctx.instruction)?.encode(ctx.writer);
            return Ok(());
        };

        let (store, ix) = match ctx.instruction {
            Operator::F32Load { memarg } => (None, Instruction::F32Load(fix_offset(memarg.into()))),
            Operator::F64Load { memarg } => (None, Instruction::F64Load(fix_offset(memarg.into()))),
            Operator::I32Load { memarg } => (None, Instruction::I32Load(fix_offset(memarg.into()))),
            Operator::I64Load { memarg } => (None, Instruction::I64Load(fix_offset(memarg.into()))),
            Operator::I32Load8U { memarg } => {
                (None, Instruction::I32Load8U(fix_offset(memarg.into())))
            }
            Operator::I32Load8S { memarg } => {
                (None, Instruction::I32Load8S(fix_offset(memarg.into())))
            }
            Operator::I32Load16U { memarg } => {
                (None, Instruction::I32Load16U(fix_offset(memarg.into())))
            }
            Operator::I32Load16S { memarg } => {
                (None, Instruction::I32Load16S(fix_offset(memarg.into())))
            }
            Operator::I64Load8U { memarg } => {
                (None, Instruction::I64Load8U(fix_offset(memarg.into())))
            }
            Operator::I64Load8S { memarg } => {
                (None, Instruction::I64Load8S(fix_offset(memarg.into())))
            }
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
                bail!(
                    "Unsupported relocation instruction: {instr:?}",
                    instr = ctx.instruction
                );
            }
        };

        // Get value to temp storage
        // TODO: Replace with local?
        match &store {
            None => {}
            Some(store_type) => {
                Instruction::GlobalSet(*ctx.global_tmps.get(store_type).unwrap() as u32)
                    .encode(ctx.writer);
            }
        }

        Instruction::GlobalGet(global_index).encode(ctx.writer);
        Instruction::I32Add.encode(ctx.writer); // add offset from global_index variable to the dyn_offset part of instruction

        // Recover back global variable
        match &store {
            None => {}
            Some(store_type) => {
                Instruction::GlobalGet(*ctx.global_tmps.get(store_type).unwrap() as u32)
                    .encode(ctx.writer);
            }
        }
        // And now push original instruction with only append left in offset
        ix.encode(ctx.writer);

        log::trace!(
            "Replacing func[{name}:{range:?}] {ix:?} with {store:?} ix",
            name = ctx.function_name,
            range = self.range,
        );

        Ok(())
    }

    // Check that relocation entry is supported
    fn check_whitelisted_code_relocation(entry: &wasmparser::RelocationEntry) -> Result<()> {
        match entry.ty {
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
            // Function offsets are not supported yet
            RelocationType::FunctionOffsetI32
            | RelocationType::SectionOffsetI32
            | RelocationType::TableIndexRelSleb => {
                bail!("Unsupported relocation type {entry:?}");
            }
            _ => {}
        }
        Ok(())
    }
}

impl CustomModify for ConstantExtractionEntry {
    type Context<'any, 'src>
        = ModifyContext<'any>
    where
        'src: 'any;
    fn try_from_entry(
        mut global_getter: impl FnMut(SymbolId) -> Result<GlobalVar>,
        entry: &wasmparser::RelocationEntry,
        extract_const: bool,
        start_offset: usize,
    ) -> Result<Option<Self>> {
        Self::check_whitelisted_code_relocation(entry)?;
        Ok(match entry.ty {
            RelocationType::MemoryAddrLeb
            | RelocationType::MemoryAddrSleb
            | RelocationType::MemoryAddrI32 // not sure how to process MemoryAddrI32?
                if extract_const =>
            {
                Some(Self {
                    relocation_type: entry.ty,
                    global_index: global_getter(entry.index as SymbolId)?,
                    addend: entry.addend,
                    range: entry.relocation_range().shift_left(start_offset),
                })
            }
            _ => return Ok(None),
        })
    }

    fn range(&self) -> Range<usize> {
        self.range.clone()
    }

    fn try_apply(&self, ctx: ModifyContext<'_>) -> Result<()> {
        match self.relocation_type {
            RelocationType::MemoryAddrLeb => self.replace_memory_offset_with_global_get(ctx)?,
            RelocationType::MemoryAddrSleb => self.replace_const_get_with_global_get(ctx)?,
            _ => {
                bail!("Unsupported relocation type")
            }
        };
        Ok(())
    }
}
