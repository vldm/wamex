//! Generate start function for sub module.
//! This function is used to initialize offsets in data segments, and optionally globals.
//!

use std::ops::Range;

use anyhow::{bail, Result};
use wasm_encoder::{InstructionSink, MemArg};
use wasmparser::RelocationType;

use crate::{
    emit::modify::{
        relocation::{self, encode},
        CustomModify, DataModifyEntry, GlobalSymbolOp, ModifyEntry, RelocationContext,
    },
    helpers::RangeExt,
    index::{AnySymbolId, GlobalId, InputFuncId, OutputGlobalId},
    read::linking::SymbolIndex,
};

#[derive(Debug, Clone)]
pub struct DataSymbolWithOffset {
    pub symbol: GlobalSymbolOp,
    pub addend: i64,
}

#[derive(Debug, Clone)]
pub enum DataEntry {
    DataOffsetCalculator {
        // lvalue where to store address of source part in original data segment
        storage: MixedOffset,

        relocated_data_symbol: MixedOffset,
    },
    TableIndex {
        // lvalue where to store address of source part in original data segment
        storage: MixedOffset,
        /// Index in the symbol table contained in the linking section that
        /// corresponds to the value at `offset`.
        original_symbol: AnySymbolId,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct MixedOffset {
    base: u32,
    addend: i64,
}
impl MixedOffset {
    fn from_parts(base: u32, addend: i64) -> Self {
        Self { base, addend }
    }

    fn calculate(&self) -> i32 {
        (self.base as i64 + self.addend).try_into().unwrap()
    }
}
struct DataInitEntry {
    storage: MixedOffset,
    offset_value: MixedOffset,
}

pub struct StartFnGen {
    memory_index: u32,
    lib_base_id: OutputGlobalId,
    data_inits: Vec<DataInitEntry>,
}

impl StartFnGen {
    pub fn new<'a>(
        memory_index: u32,
        lib_base_id: OutputGlobalId,
        modify_entries: impl IntoIterator<Item = &'a ModifyEntry<DataEntry>>,
    ) -> Self {
        let data_inits = modify_entries
            .into_iter()
            .filter_map(|entry| {
                if let ModifyEntry::Custom(DataEntry::DataOffsetCalculator {
                    storage,
                    relocated_data_symbol,
                }) = entry
                {
                    Some(DataInitEntry {
                        storage: *storage,
                        offset_value: *relocated_data_symbol,
                    })
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        Self {
            memory_index,
            lib_base_id,
            data_inits,
        }
    }

    pub fn generate_fn(&self) -> wasm_encoder::Function {
        let mut func = wasm_encoder::Function::new([]);

        let mut instr = func.instructions();
        for data_entry in &self.data_inits {
            self.push_init(&mut instr, data_entry, self.lib_base_id, self.memory_index);
        }
        instr.end();
        func
    }

    fn push_init(
        &self,
        instr: &mut InstructionSink<'_>,
        // Offset in data segment where to store the address of global var
        data_entry: &DataInitEntry,
        lib_base_id: OutputGlobalId,
        memory_index: u32,
    ) {
        // store pointer in specific data symbol
        // value = (GOT+src) | src
        // *(GOT+dst) = value

        let src_offset = data_entry.offset_value.calculate();

        let dst_offset = data_entry.storage.calculate();

        instr.global_get(lib_base_id as u32);
        instr.i32_const(src_offset);
        instr.i32_add();

        instr.global_get(lib_base_id as u32);
        instr.i32_const(dst_offset);
        instr.i32_add();

        instr.i32_store(MemArg {
            offset: 0,
            align: 1,
            memory_index,
        });
    }
}

type RelocateState<'any, 'src> =
    relocation::RelocateState<'any, 'src, Box<dyn Fn(GlobalId) -> Option<OutputGlobalId>>>;

pub struct StartFnModifyContext<'any, 'src> {
    // Sink of buffer where data segment is already stored
    pub data_segment: &'any mut [u8],
    pub relocate: RelocateState<'any, 'src>,
}

impl CustomModify for DataEntry {
    type Context<'any, 'src>
        = StartFnModifyContext<'any, 'src>
    where
        'src: 'any;
    fn try_from_entry(
        entry: &wasmparser::RelocationEntry,
        context: &RelocationContext,
    ) -> Result<Option<Self>>
    where
        Self: Sized,
    {
        Self::check_whitelisted_data_relocation(entry)?;
        Ok(match entry.ty {
            RelocationType::MemoryAddrI32 if context.dyn_relocate => {
                let containing_symbol = context.containing_symbol.as_ref().unwrap(); // storage should always exist in dyn relocation mode

                // and be in GOT mode
                let GlobalSymbolOp::GotOffset { offset: dst_offset } = containing_symbol.symbol
                else {
                    bail!("Relocation range {entry:?} does not refer to a valid global symbol inside current module")
                };
                let storage = MixedOffset::from_parts(dst_offset, containing_symbol.addend);

                let data_symbol = context.referenced_symbol.ok_or_else(|| {
                    anyhow::anyhow!("Relocation {entry:?} does not refer to a valid global symbol")
                })?; // source symbol should also be in map

                // But if it static offset - skip dyn relocation and make it static
                let GlobalSymbolOp::GotOffset { offset: src_offset } = data_symbol else {
                    log::trace!(
                        "SRC Global var from entry {entry:?} ignored in start fn generation"
                    );
                    return Ok(None);
                };

                Some(Self::DataOffsetCalculator {
                    storage,
                    relocated_data_symbol: MixedOffset::from_parts(src_offset, entry.addend),
                })
            }
            RelocationType::TableIndexI32 => {
                // return Ok(None);
                // todo!();
                let containing_symbol = context.containing_symbol.as_ref().unwrap(); // storage should always exist in dyn relocation mode

                // and be in GOT mode
                let GlobalSymbolOp::GotOffset { offset: dst_offset } = containing_symbol.symbol
                else {
                    log::trace!("Relocation range {entry:?} does not refer to a valid global symbol inside current module");

                    return Ok(None);
                };
                let storage = MixedOffset::from_parts(dst_offset, containing_symbol.addend);

                Some(Self::TableIndex {
                    storage,
                    original_symbol: entry.index as AnySymbolId,
                })
            }
            _ => return Ok(None),
        })
    }

    fn range(&self) -> Range<usize> {
        let start = match self {
            Self::DataOffsetCalculator { storage, .. } => storage.calculate(),
            Self::TableIndex { storage, .. } => storage.calculate(),
        } as usize;
        start..(start + 4)
    }
    fn try_apply(&self, ctx: Self::Context<'_, '_>) -> Result<()> {
        let relocation_range = self.range();
        let target = &mut ctx.data_segment[relocation_range];
        match self {
            Self::DataOffsetCalculator { .. } => {
                // We can place 0s of 0xffff but main initialisation is in `StartFnGen`
                encode::encode_u32(u32::MAX, target.try_into().unwrap());
                Ok(())
            }
            Self::TableIndex {
                original_symbol, ..
            } => {
                // Apply the table index relocation using the context
                encode::encode_u32(
                    Self::get_relocated_function_table_index(&ctx.relocate, *original_symbol)?
                        as u32,
                    target.try_into().unwrap(),
                );
                Ok(())
            }
        }
    }
}

impl DataEntry {
    fn _get_relocation_input_function_index(
        relocate: &RelocateState<'_, '_>,
        index: AnySymbolId,
    ) -> Result<InputFuncId> {
        let Some(SymbolIndex::Func(input_func_id)) = relocate
            .input_module
            .linking
            .linking_symbols
            .original_indexes
            .get(index)
        else {
            bail!("Relocation does not refer to a valid function");
        };
        Ok(*input_func_id)
    }
    fn get_relocated_function_table_index(
        relocate: &RelocateState<'_, '_>,
        index: AnySymbolId,
    ) -> Result<usize> {
        let input_func_id = DataEntry::_get_relocation_input_function_index(relocate, index)?;

        let Some(&table_index) = relocate
            .emit_module
            .indirect_functions
            .function_table_index
            .get(&input_func_id)
        else {
            bail!(
                "Dependency analysis error: \
                     No indirect function table index \
                     for input function {input_func_id} \
                     referenced by relocation with index {index}"
            )
        };
        Ok(table_index)
    }
    fn check_whitelisted_data_relocation(entry: &wasmparser::RelocationEntry) -> Result<()> {
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
            RelocationType::MemoryAddrI32 | RelocationType::TableIndexI32 => Ok(()),

            _ => bail!("Unsupported data relocation type"),
        }
    }
}

impl<'any> StartFnModifyContext<'any, '_> {
    pub fn apply_relocation(self, entry: &'any DataModifyEntry) -> Result<()> {
        match entry {
            DataModifyEntry::Custom(entry) => entry.try_apply(self)?,
            DataModifyEntry::Other(reloc_entry) => {
                log::debug!("Applying relocation entry: {reloc_entry:?}");
                self.relocate
                    .apply_relocation(self.data_segment, reloc_entry)?;
            }
        }
        Ok(())
    }
}
