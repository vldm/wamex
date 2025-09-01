//! Extract data symbols constant access to GOT access.
//!
//! Replace constants that were used to refer to this data symbols in form of `i32.const` to GOT form `i32.add(global.get lib_base, <offset>)`.
//! The most of the data are kept in "main" module, we extract only module related data.
//!
//! The same approach is used for `indirect_function_table` access.

use std::ops::Range;

use anyhow::{bail, ensure, Result};
use wasm_encoder::{Encode, Instruction};
use wasmparser::{Operator, RelocationType};

use super::{ModifyContext, StoreType};
use crate::{
    emit::modify::{CustomModify, GlobalSymbolOp, RelocationContext},
    helpers::RangeExt,
    index::AnySymbolId,
};

// Represents a data relocation entry with additional information about global variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstantExtractionEntry {
    pub(super) relocation_type: RelocationType,
    pub(super) got_offset: GlobalSymbolOp,
    /// Addend to add to the address, or `0` if not applicable. The value must
    /// be consistent with the `self.ty.addend_kind()`.
    pub(super) addend: i64,
    pub(super) range: Range<usize>,
}

impl ConstantExtractionEntry {
    // Simple replace of i32.const with global.get + i32.add
    // Retuns size of the replacement
    pub fn replace_const_get_with_global_get(
        &self,
        global_index: u32,
        ctx: ModifyContext<'_>,
    ) -> Result<()> {
        ensure!(
            matches!(ctx.instruction, Operator::I32Const { .. }),
            "Unsupported relocation operand"
        );

        let GlobalSymbolOp::GotOffset { offset } = self.got_offset else {
            log::trace!("Skipping replace to GOT for func:{}", ctx.function_name);
            <Instruction<'_> as TryFrom<_>>::try_from(ctx.instruction)?.encode(ctx.writer);
            return Ok(());
        };
        let result_ix = Instruction::GlobalGet(global_index);
        log::trace!(
            "Replacing func[{name}:{range:?}] {src_ix:?} with {result_ix:?}, addend {addend}",
            addend = self.addend,
            range = self.range,
            name = ctx.function_name,
            src_ix = ctx.instruction
        );
        result_ix.encode(ctx.writer);
        let offset = offset as i32 + self.addend as i32;

        Instruction::I32Const(offset).encode(ctx.writer);
        Instruction::I32Add.encode(ctx.writer);

        // TODO: Return new list of relocations to GlobalGet and I32Const (GlobalIndexLeb + MemoryAddrLeb | TableIndexLeb)
        Ok(())
    }

    // replaces *.store and *.load family with multiple operations with `global` variable:
    // example for store:
    // > f32.store ($local_var + offset)    :stack[dyn_offset -> value]
    // <
    // < global.set $f32_global_index       :stack[dyn_offset]                  // $f32_global_index = value
    // < global.get $lib_base               :stack[dyn_offset -> $lib_base]
    // < i32.add :stack[modified_offset]                                        // dyn_offset += $lib_base
    // < global.get $f32_global_index       :stack[modified_offset -> value]    // return back <value> to stack
    // < f32.store <extra_offset>                                               // original f32.store with <offset of data symbol in memory>
    // For load:
    // > i32.load_u8 ($local_var + offset)  :stack[dyn_offset]
    // < global.get $global_var_index       :stack[dyn_offset -> $global_var_index]
    // < i32.add                            :stack[modified_offset]             // dyn_offset += $global_var_index
    // < i32.load_u8 <extra_offset>                                             // original i32.load_u8 with <offset of data symbol in memory>

    /// Returns size of the replacement.
    pub fn replace_memory_offset_with_global_get(&self, ctx: ModifyContext<'_>) -> Result<()> {
        let GlobalSymbolOp::GotOffset { offset } = self.got_offset else {
            log::trace!("Skipping replace to GOT for func:{}", ctx.function_name);
            <Instruction<'_> as TryFrom<_>>::try_from(ctx.instruction)?.encode(ctx.writer);
            return Ok(());
        };
        let offset = (offset as i64 + self.addend) as u64;

        let fix_offset = |memarg: wasmparser::MemArg| wasm_encoder::MemArg {
            align: memarg.align as u32,
            memory_index: memarg.memory,
            offset,
        };
        let Some(lib_base_id) = ctx.lib_base_id else {
            bail!("replace_memory_offset_with_global_get for main module is not supported");
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

        log::trace!(
            "Replacing func[{name}:{range:?}] {ix:?} with {store:?} ix",
            name = ctx.function_name,
            range = self.range,
        );

        // TODO: Replace with local?
        let save_value = store.as_ref().map(|store_type| {
            Instruction::GlobalSet(*ctx.global_tmps.get(store_type).unwrap() as u32)
        });
        let restore_value = store.as_ref().map(|store_type| {
            Instruction::GlobalGet(*ctx.global_tmps.get(store_type).unwrap() as u32)
        });

        save_value.map(|v| v.encode(ctx.writer)); // Get <value> from stack to temp storage
        Instruction::GlobalGet(lib_base_id).encode(ctx.writer);
        Instruction::I32Add.encode(ctx.writer); // add offset from global_index variable to the dyn_offset part of instruction
        restore_value.map(|v| v.encode(ctx.writer)); // Recover back <value> to stack
        ix.encode(ctx.writer); // And now push modified original instruction

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
        entry: &wasmparser::RelocationEntry,
        context: &RelocationContext,
    ) -> Result<Option<Self>>
    {
        Self::check_whitelisted_code_relocation(entry)?;
        Ok(match entry.ty {
            RelocationType::MemoryAddrLeb
            | RelocationType::MemoryAddrSleb
            | RelocationType::TableIndexSleb
                if context.dyn_relocate => 
            {
                Some(Self {
                    relocation_type: entry.ty,
                    got_offset: context.referenced_symbol
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "Relocation {entry:?} does not refer to a valid data symbol"
                            )
                        })?,
                    
                    addend: entry.addend,
                    range: entry.relocation_range(),
                })
            }
            RelocationType::TableIndexI32  // in instruction Sleb or Leb are used I32 is used only in data segment ?
            | RelocationType::MemoryAddrI32
            => {
                panic!("BUG: Relocation type {entry:?} not supported")
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
            RelocationType::MemoryAddrSleb => {
                let Some(lib_base_id) = ctx.lib_base_id else {
                    bail!("replace_const_get_with_global_get for main module is not supported");
                };
                self.replace_const_get_with_global_get(lib_base_id, ctx)?
            }
            RelocationType::TableIndexSleb => {
                let Some(table_base_id) = ctx.table_base_id else {
                    bail!("replace_const_get_with_global_get for main module is not supported");
                };
                self.replace_const_get_with_global_get(table_base_id, ctx)?
            }
            _ => {
                bail!("Unsupported relocation type")
            }
        };
        Ok(())
    }
}
