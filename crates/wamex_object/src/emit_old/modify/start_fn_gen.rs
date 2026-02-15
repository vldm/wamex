//! Generate start function for sub module.
//! This function is used to initialize offsets in data segments, and optionally globals.
//!

use std::ops::Range;

use anyhow::{Result, bail};
use wasm_encoder::{InstructionSink, MemArg};

use crate::{
    emit::modify::{
        CustomModify, DataModifyEntry, ModifyEntry, RelocateState, RelocationContext, SymbolOffset,
        SymbolOp, SymbolUOffset,
        relocation::{DataSymbolTag, FunctionIndexTag, encode},
    },
    read::raw::DataSegmentId,
    symbols::{
        SymbolId,
        reloc::{
            AnyRelocationEntry, Encoding, Relative, RelocationEntry, RelocationWidth, SymbolType,
        },
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]

pub struct DataSymbolWithOffset {
    pub storage_segment_id: DataSegmentId,
    pub storage_symbol_id: SymbolId,
    pub storage_offset_in_data: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
            if !matches!(data_entry.relocation.encoding, Encoding::Fixed) {
                bail!(
                    "Unsupported data relocation encoding {:?} in entry {:?}",
                    data_entry.relocation.encoding,
                    data_entry
                );
            }
            let relocated_symbol_offset = match data_entry.relocation.symbol_type {
                SymbolType::MemoryAddr => relocate
                    .get_entry_symbol_op::<DataSymbolTag>(&data_entry.relocation)
                    .map(|v| v.map(|v| v.try_into().unwrap())),
                SymbolType::TableIndex => {
                    relocate.get_entry_symbol_op::<FunctionIndexTag>(&data_entry.relocation)
                }
                index => panic!("Unsupported relocation type {index:?}",),
            }?;
            let storage = relocate
                .get_data_symbol_op(
                    data_entry.storage.storage_segment_id,
                    data_entry.storage.storage_symbol_id,
                )?
                .map(|offset| offset + data_entry.storage.storage_offset_in_data as i64);

            log::trace!(
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
        instr.global_get(dst_got.as_u32());
        instr.i32_const(dst_offset.try_into().unwrap());
        instr.i32_add();

        match data_entry.relocated_symbol_offset {
            SymbolOp::GotBased {
                got: src_got,
                value: src_offset,
            } => {
                instr.global_get(src_got.as_u32());
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
        entry: &AnyRelocationEntry,
        context: &RelocationContext,
    ) -> Result<Option<Self>>
    where
        Self: Sized,
    {
        Ok(match entry {
            AnyRelocationEntry::Linkage(entry) => {
                Self::check_whitelisted_data_relocation(entry)?;
                match entry.symbol_type {
                    SymbolType::MemoryAddr | SymbolType::TableIndex if context.dyn_base => {
                        let storage = context.containing_symbol.as_ref().unwrap().clone(); // storage should always exist in dyn relocation mode
                        let relocation = entry.clone();
                        Some(Self {
                            storage,
                            relocation,
                        })
                    }
                    _ => None,
                }
            }
            AnyRelocationEntry::Type(_) => {
                bail!("Type relocations are not supported for start function generation");
            }
        })
    }

    fn range(&self) -> Range<usize> {
        // relocation store absolute offset in data segment
        let start = self.relocation.offset.try_into().unwrap();
        start..(start + 4)
    }
    fn try_apply(&self, ctx: Self::Context<'_, '_>) -> Result<()> {
        const DUMMY_ADDR: u32 = 0xdeadbeefu32.to_be(); // Dead Beef in little endian
        let relocation_range = self.range();
        let target = &mut ctx.data_segment[relocation_range];

        // Main initialisation is in `StartFnGen`
        // Set placeholder to make debugging easier
        encode::encode_u32(DUMMY_ADDR, target.try_into().unwrap());
        Ok(())
    }
}

impl DataEntry {
    fn check_whitelisted_data_relocation(entry: &RelocationEntry) -> Result<()> {
        if matches!(entry.width, RelocationWidth::Bits64) {
            bail!("U64 memory pointers is currently not supported")
        }
        if !matches!(entry.encoding, Encoding::Fixed) {
            bail!("Only fixed encoding cannot be found in data segment")
        }
        if !matches!(entry.relation, Relative::None) {
            bail!("Relocation memory pointers is currently not supported")
        }
        Ok(())
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
