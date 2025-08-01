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
        CustomModify, DataModifyEntry, GlobalVar, ModifyEntry,
    },
    helpers::RangeExt,
    index::{GlobalId, InputFuncId, OutputGlobalId, SymbolId},
    read::linking::SymbolIndex,
};

#[derive(Debug, Clone)]
pub enum DataEntry {
    DataOffsetCalculator {
        // Location of source part in original data segment
        range: Range<usize>,
        dep_data_id: GlobalVar,
    },
    TableIndex {
        // Location of source part in original data segment
        range: Range<usize>,
        /// Index in the symbol table contained in the linking section that
        /// corresponds to the value at `offset`.
        original_symbol: SymbolId,
    },
}

pub struct StartFnGen {
    memory_index: u32,
    lib_base_id: OutputGlobalId,
    data_inits: Vec<(u32, OutputGlobalId)>,
}

impl StartFnGen {
    pub fn new<'a>(
        memory_index: u32,
        lib_base_id: OutputGlobalId,
        modify_entries: impl IntoIterator<Item = &'a ModifyEntry<DataEntry>>,
    ) -> Result<Self> {
        let data_inits = modify_entries
            .into_iter()
            .filter_map(|entry| {
                if let ModifyEntry::Custom(DataEntry::DataOffsetCalculator { range, dep_data_id }) =
                    entry
                {
                    let GlobalVar::Extract(dep_data_id) = dep_data_id else {
                        log::trace!("Global var ignored in start_fn_generation in {entry:?}");
                        return None;
                    };
                    Some((range.start as u32, *dep_data_id))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        Ok(Self {
            memory_index,
            lib_base_id,
            data_inits,
        })
    }

    pub fn generate_fn(&self) -> wasm_encoder::Function {
        let mut func = wasm_encoder::Function::new([]);

        let mut instr = func.instructions();
        for (store_offset_at, global_var) in &self.data_inits {
            self.push_init(
                &mut instr,
                *store_offset_at,
                self.lib_base_id,
                *global_var,
                self.memory_index,
            );
        }
        instr.end();
        func
    }

    fn push_init(
        &self,
        instr: &mut InstructionSink<'_>,
        // Offset in data segment where to store the address of global var
        offset_to_store_at: u32,
        lib_base_id: OutputGlobalId,
        global_var: OutputGlobalId,
        memory_index: u32,
    ) {
        instr.global_get(lib_base_id as u32);
        instr.i32_const(offset_to_store_at as i32);
        instr.i32_add();
        instr.global_get(global_var as u32);
        instr.i32_store(MemArg {
            offset: 0,
            align: 2,
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
        mut global_getter: impl FnMut(SymbolId) -> Result<super::GlobalVar>,
        entry: &wasmparser::RelocationEntry,
        extract_const: bool,
        start_offset: usize,
    ) -> Result<Option<Self>>
    where
        Self: Sized,
    {
        Self::check_whitelisted_data_relocation(entry)?;
        Ok(match entry.ty {
            RelocationType::MemoryAddrI32 if extract_const => Some(Self::DataOffsetCalculator {
                range: entry.relocation_range().shift_left(start_offset),
                dep_data_id: global_getter(entry.index as SymbolId)?,
            }),
            RelocationType::TableIndexI32 => Some(Self::TableIndex {
                range: entry.relocation_range().shift_left(start_offset),
                original_symbol: entry.index as SymbolId,
            }),
            _ => return Ok(None),
        })
    }

    fn range(&self) -> Range<usize> {
        match self {
            Self::DataOffsetCalculator { range, .. } => range.clone(),
            Self::TableIndex { range, .. } => range.clone(),
        }
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
        index: SymbolId,
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
        index: SymbolId,
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
