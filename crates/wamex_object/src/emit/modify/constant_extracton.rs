//! Extract data symbols constant access to GOT access.
//!
//! Replace constants that were used to refer to this data symbols in form of `i32.const` to GOT form `i32.add(global.get lib_base, <offset>)`.
//! The most of the data are kept in "main" module, we extract only module related data.
//!
//! The same approach is used for `indirect_function_table` access.

use std::ops::Range;

use anyhow::{Result, bail, ensure};
use cranelift_entity::EntityRef;
use wasm_encoder::{Encode, Instruction};
use wasmparser::Operator;

use super::{ModifyContext, StoreType};
use crate::{
    emit::modify::{
        CustomModify, RelocationContext, SymbolOffset, SymbolOp,
        relocation::{DataSymbolTag, FunctionIndexTag},
    },
    symbols::reloc::{
        AnyRelocationEntry, Encoding, Relative, RelocationEntry, RelocationWidth, SymbolType,
    },
};

// Represents a data relocation entry with additional information about global variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstantExtractionEntry {
    pub(super) entry: RelocationEntry,
}

impl ConstantExtractionEntry {
    // Simple replace of i32.const with global.get + i32.add
    // Retuns size of the replacement
    pub fn replace_const_get_with_global_get<V>(
        &self,
        symbol_offset: SymbolOp<V>,
        ctx: ModifyContext<'_, '_>,
    ) -> Result<()>
    where
        V: TryInto<usize>, // can be usize or i64 (both should be >= 0)
    {
        ensure!(
            matches!(ctx.instruction, Operator::I32Const { .. }),
            "Unsupported relocation operand"
        );

        let SymbolOp::GotBased {
            got: got_global_index,
            value: got_offset,
        } = symbol_offset
        else {
            bail!(
                "replace_const_get_with_global_get called with static offset symbol {:?}",
                self.entry
            );
        };
        let got_offset = got_offset
            .try_into()
            .map_err(|_| anyhow::anyhow!("Offset is too large to fit in usize"))?;

        let result_ix = Instruction::GlobalGet(got_global_index.index() as u32);
        log::trace!(
            "Replacing func[{name}:{range:?}] {src_ix:?} with {result_ix:?}",
            range = self.range(),
            name = ctx.function_name,
            src_ix = ctx.instruction
        );
        result_ix.encode(ctx.writer);
        let offset = got_offset as i32;

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
    pub fn replace_memory_offset_with_global_get(
        &self,
        symbol_offset: SymbolOffset,
        ctx: ModifyContext<'_, '_>,
    ) -> Result<()> {
        let SymbolOffset::GotBased {
            got: got_global_index,
            value: got_offset,
        } = symbol_offset
        else {
            bail!(
                "replace_memory_offset_with_global_get called with static offset symbol {:?}",
                self.entry
            );
        };

        let offset = got_offset as u64;

        let fix_offset = |memarg: wasmparser::MemArg| wasm_encoder::MemArg {
            align: memarg.align as u32,
            memory_index: memarg.memory,
            offset,
        };

        let (store, ix) = match ctx.instruction {
            Operator::F32Load { memarg } => (None, Instruction::F32Load(fix_offset(memarg))),
            Operator::F64Load { memarg } => (None, Instruction::F64Load(fix_offset(memarg))),
            Operator::I32Load { memarg } => (None, Instruction::I32Load(fix_offset(memarg))),
            Operator::I64Load { memarg } => (None, Instruction::I64Load(fix_offset(memarg))),
            Operator::I32Load8U { memarg } => (None, Instruction::I32Load8U(fix_offset(memarg))),
            Operator::I32Load8S { memarg } => (None, Instruction::I32Load8S(fix_offset(memarg))),
            Operator::I32Load16U { memarg } => (None, Instruction::I32Load16U(fix_offset(memarg))),
            Operator::I32Load16S { memarg } => (None, Instruction::I32Load16S(fix_offset(memarg))),
            Operator::I64Load8U { memarg } => (None, Instruction::I64Load8U(fix_offset(memarg))),
            Operator::I64Load8S { memarg } => (None, Instruction::I64Load8S(fix_offset(memarg))),
            Operator::I64Load16U { memarg } => (None, Instruction::I64Load16U(fix_offset(memarg))),
            Operator::I64Load16S { memarg } => (None, Instruction::I64Load16S(fix_offset(memarg))),
            Operator::I64Load32U { memarg } => (None, Instruction::I64Load32U(fix_offset(memarg))),
            Operator::I64Load32S { memarg } => (None, Instruction::I64Load32S(fix_offset(memarg))),

            Operator::I32Store { memarg } => (
                Some(StoreType::I32),
                Instruction::I32Store(fix_offset(memarg)),
            ),
            Operator::I64Store { memarg } => (
                Some(StoreType::I64),
                Instruction::I64Store(fix_offset(memarg)),
            ),
            Operator::I32Store8 { memarg } => (
                Some(StoreType::I32),
                Instruction::I32Store8(fix_offset(memarg)),
            ),
            Operator::I32Store16 { memarg } => (
                Some(StoreType::I32),
                Instruction::I32Store16(fix_offset(memarg)),
            ),
            Operator::I64Store8 { memarg } => (
                Some(StoreType::I64),
                Instruction::I64Store8(fix_offset(memarg)),
            ),
            Operator::I64Store16 { memarg } => (
                Some(StoreType::I64),
                Instruction::I64Store16(fix_offset(memarg)),
            ),
            Operator::I64Store32 { memarg } => (
                Some(StoreType::I64),
                Instruction::I64Store32(fix_offset(memarg)),
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
            range = self.range(),
        );

        // TODO: Replace with local?
        let save_value = store.as_ref().map(|store_type| {
            Instruction::GlobalSet(ctx.global_tmps.get(store_type).unwrap().as_u32())
        });
        let restore_value = store.as_ref().map(|store_type| {
            Instruction::GlobalGet(ctx.global_tmps.get(store_type).unwrap().as_u32())
        });

        if let Some(v) = save_value {
            v.encode(ctx.writer)
        } // Get <value> from stack to temp storage
        Instruction::GlobalGet(got_global_index.as_u32()).encode(ctx.writer);
        Instruction::I32Add.encode(ctx.writer); // add offset from global_index variable to the dyn_offset part of instruction
        if let Some(v) = restore_value {
            v.encode(ctx.writer)
        } // Recover back <value> to stack
        ix.encode(ctx.writer); // And now push modified original instruction

        Ok(())
    }

    // Check that relocation entry is supported
    fn check_whitelisted_code_relocation(entry: &RelocationEntry) -> Result<()> {
        if matches!(entry.width, RelocationWidth::Bits64) {
            bail!("U64 memory pointers is currently not supported")
        }
        if !matches!(entry.relation, Relative::None) {
            bail!("Relocation memory pointers is currently not supported")
        }
        if matches!(
            entry.symbol_type,
            SymbolType::SectionOffset | SymbolType::FunctionOffset | SymbolType::MemoryAddrLocrel
        ) {
            bail!("Non supported symbol type for code relocation")
        }
        Ok(())
    }
}

impl CustomModify for ConstantExtractionEntry {
    type Context<'any, 'src>
        = ModifyContext<'any, 'src>
    where
        'src: 'any;
    fn try_from_entry(
        entry: &AnyRelocationEntry,
        context: &RelocationContext,
    ) -> Result<Option<Self>> {
        Ok(match entry {
            AnyRelocationEntry::Linkage(entry) => {
                Self::check_whitelisted_code_relocation(entry)?;
                match (entry.symbol_type, entry.encoding) {
                    (SymbolType::TableIndex, Encoding::Sleb)
                    | (SymbolType::MemoryAddr, Encoding::Sleb)
                    | (SymbolType::MemoryAddr, Encoding::Leb)
                        if context.dyn_base =>
                    {
                        Some(Self { entry: *entry })
                    }
                    _ => None,
                }
            }
            AnyRelocationEntry::Type(_) => {
                // bail!("Type relocations are not supported for constant extraction");
                None
            }
        })
    }

    fn range(&self) -> Range<usize> {
        self.entry.relocation_range()
    }

    fn try_apply(&self, ctx: ModifyContext<'_, '_>) -> Result<()> {
        match (self.entry.symbol_type, self.entry.encoding) {
            (SymbolType::MemoryAddr, Encoding::Leb) => {
                let symbol_offset = ctx
                    .relocation_state
                    .get_entry_symbol_op::<DataSymbolTag>(&self.entry)?;
                self.replace_memory_offset_with_global_get(symbol_offset, ctx)?
            }
            (SymbolType::MemoryAddr, Encoding::Sleb) => {
                let symbol_offset = ctx
                    .relocation_state
                    .get_entry_symbol_op::<DataSymbolTag>(&self.entry)?;
                self.replace_const_get_with_global_get(symbol_offset, ctx)?
            }
            (SymbolType::TableIndex, Encoding::Sleb) => {
                let symbol_offset = ctx
                    .relocation_state
                    .get_entry_symbol_op::<FunctionIndexTag>(&self.entry)?;
                self.replace_const_get_with_global_get(symbol_offset, ctx)?
            }
            _ => {
                bail!("Unsupported relocation type")
            }
        };
        Ok(())
    }
}
