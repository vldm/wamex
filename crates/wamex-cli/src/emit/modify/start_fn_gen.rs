//! Generate start function for sub module.
//! This function is used to initialize offsets in data segments, and optionally globals.
//!

use std::ops::Range;

use anyhow::{bail, Result};
use wasm_encoder::{InstructionSink, MemArg};
use wasmparser::{RelocationEntry, RelocationType};

use crate::{
    emit::modify::{
        relocation::{encode, DataSymbolTag, FunctionIndexTag},
        CustomModify, DataModifyEntry, ModifyEntry, RelocateState, RelocationContext, SymbolOffset,
        SymbolOp, SymbolUOffset,
    },
    index::{AnySymbolId, DataSegmentId, DataSymbolId, InputFuncId},
    read::linking::SymbolIndex,
};

#[derive(Debug, Clone)]

pub struct DataSymbolWithOffset {
    pub storage_segment_id: DataSegmentId,
    pub storage_symbol_id: DataSymbolId,
    pub storage_offset_in_data: i64,
}

#[derive(Debug, Clone)]
pub struct DataEntry {
    // lvalue where to store address of source part in original data segment
    pub storage: DataSymbolWithOffset,

    // rvalue - what to store
    pub relocation: RelocationEntry,
}

#[derive(Debug, Clone)]
pub struct DataEntryWithOffsets {
    storage: SymbolOffset,
    relocated_symbol_offset: SymbolUOffset,
}

pub struct StartFnGen {
    memory_index: u32,
    data_inits: Vec<DataEntryWithOffsets>,
}
// 1. Fn(AnySymbolId) -> (got, offset) its from relocation module
// 2. Fn(DataSegmentId, DataSymbolId) -> (got, offset)

impl StartFnGen {
    pub fn new<'a>(
        relocate: RelocateState<'a, '_>,
        memory_index: u32,
        modify_entries: impl IntoIterator<Item = &'a ModifyEntry<DataEntry>>,
    ) -> Result<Self> {
        let mut data_inits = vec![];

        for entry in modify_entries {
            let ModifyEntry::Custom(data_entry) = entry else {
                continue;
            };
            let relocated_symbol_offset = match data_entry.relocation.ty {
                RelocationType::MemoryAddrI32 => relocate
                    .get_entry_symbol_op::<DataSymbolTag>(&data_entry.relocation)
                    .map(|v| v.map(|v| v.try_into().unwrap())),
                RelocationType::TableIndexI32 => {
                    relocate.get_entry_symbol_op::<FunctionIndexTag>(&data_entry.relocation)
                }
                _ => panic!("Unsupported relocation type {:?}", data_entry.relocation.ty),
            }?;
            let storage = relocate
                .get_data_symbol_op(
                    data_entry.storage.storage_segment_id,
                    data_entry.storage.storage_symbol_id,
                )?
                .map(|v| v + data_entry.storage.storage_offset_in_data);

            log::warn!(
                "Data symbol storage: {:?}, entry: {:?}",
                storage,
                data_entry
            );
            data_inits.push(DataEntryWithOffsets {
                storage,

                relocated_symbol_offset,
            });
        }
        Ok(Self {
            memory_index,
            data_inits,
        })
    }

    pub fn generate_fn(&self) -> wasm_encoder::Function {
        let mut func = wasm_encoder::Function::new([]);

        let mut instr = func.instructions();
        for data_entry in &self.data_inits {
            self.push_init(&mut instr, data_entry, self.memory_index);
        }
        instr.end();
        func
    }

    fn push_init(
        &self,
        instr: &mut InstructionSink<'_>,
        // Offset in data segment where to store the address of global var
        data_entry: &DataEntryWithOffsets,
        memory_index: u32,
    ) {
        // store pointer in specific data symbol
        // value = (GOT+src) | src
        // *(GOT+dst) = value

        let SymbolOp::GotBased {
            got: dst_got,
            value: dst_offset,
        } = data_entry.storage
        else {
            panic!("Data relocation storage should always be in GOT")
        };
        instr.global_get(dst_got.as_raw_index() as u32);
        instr.i32_const(dst_offset.try_into().unwrap());
        instr.i32_add();

        match data_entry.relocated_symbol_offset {
            SymbolOp::GotBased {
                got: src_got,
                value: src_offset,
            } => {
                instr.global_get(src_got.as_raw_index() as u32);
                instr.i32_const(src_offset.try_into().unwrap());
                instr.i32_add();
            }
            SymbolOp::StaticOffset { value: src_offset } => {
                instr.i32_const(src_offset.try_into().unwrap());
            }
        };

        instr.i32_store(MemArg {
            offset: 0,
            align: 1,
            memory_index,
        });
    }
}

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
            RelocationType::MemoryAddrI32 | RelocationType::TableIndexI32
                if context.dyn_relocate =>
            {
                let storage = context.containing_symbol.as_ref().unwrap().clone(); // storage should always exist in dyn relocation mode

                Some(Self {
                    storage,
                    relocation: entry.clone(),
                })
            }

            _ => return Ok(None),
        })
    }

    fn range(&self) -> Range<usize> {
        // relocation store absolute offset in data segment
        let start = self.relocation.offset.try_into().unwrap();
        start..(start + 4)
    }
    fn try_apply(&self, ctx: Self::Context<'_, '_>) -> Result<()> {
        const DUMMY_ADDR: u32 = 0xefbeadde; // Dead Beef in little endian
        let relocation_range = self.range();
        let target = &mut ctx.data_segment[relocation_range];

        // Main initialisation is in `StartFnGen`
        // Set placeholder to make debugging easier
        encode::encode_u32(DUMMY_ADDR, target.try_into().unwrap());
        Ok(())
    }
}

impl DataEntry {
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
