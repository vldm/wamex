//! Convert absolute memory address to GOT-relative address in code segment.
//! The same for table entries (indirect function table).
//!
//! Uses relocation entries to find place in code and symbols that need to make relocatable.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
};

use anyhow::{Result, bail, ensure};
use cranelift_entity::{EntityRef, packed_option::ReservedValue};
use wasmparser::{GlobalType, Operator};

use super::{Cursor, HandleReloc, ModificationEntry, ModifyOrReloc, RelocationEntry};
use crate::{
    SVec,
    emit::{
        modify::{OutputEntityRef, OutputRelocationEntry, Rewrite, wasm_emitter::MemArgOffsets},
        relocation::EntityLocation,
    },
    index::GappedMap,
    linkage::reloc::{Encoding, Relative, RelocationWidth, SymbolType},
    typed::{
        DefinedGlobal, EntityBody, FileId, GlobalRef, common_index::EntityKind,
        data::SpecificLocation,
    },
};

#[derive(Debug, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub enum StoreType {
    I32,
    I64,
    F32,
    F64,
}

pub fn init_each_store_var() -> Vec<(StoreType, wasmparser::ValType)> {
    vec![
        (StoreType::I32, wasmparser::ValType::I32),
        (StoreType::I64, wasmparser::ValType::I64),
        (StoreType::F32, wasmparser::ValType::F32),
        (StoreType::F64, wasmparser::ValType::F64),
    ]
}

pub fn global_init_tmp(val_type: wasmparser::ValType) -> SVec<u8, 32> {
    use wasm_encoder::Encode;

    let res = match val_type {
        wasmparser::ValType::I32 => wasm_encoder::ConstExpr::i32_const(0),
        wasmparser::ValType::I64 => wasm_encoder::ConstExpr::i64_const(0),
        wasmparser::ValType::F32 => wasm_encoder::ConstExpr::f32_const(0.0.into()),
        wasmparser::ValType::F64 => wasm_encoder::ConstExpr::f64_const(0.0.into()),
        _ => panic!("Unsupported global type for tmp init"),
    };
    let mut buffer = Vec::new();
    res.encode(&mut buffer);
    SVec::from(buffer)
}

#[derive(Debug, Clone)]
pub struct GotInfo {
    memory_base: GlobalRef,
    table_base: GlobalRef,
}

impl ReservedValue for GotInfo {
    fn reserved_value() -> Self {
        Self {
            memory_base: GlobalRef::reserved_value(),
            table_base: GlobalRef::reserved_value(),
        }
    }
    fn is_reserved_value(&self) -> bool {
        self.memory_base.is_reserved_value() && self.table_base.is_reserved_value()
    }
}

#[derive(Debug)]
pub struct CodeRelocationHandler {
    /// Information about external modules GOTs (if any)
    pub import_module_got: GappedMap<FileId, GotInfo>,
    /// Defines where to search for symbols.
    /// It might be our local
    pub symbols_location: BTreeMap<EntityLocation, FileId>,
    // Temporary globals for constant extraction
    pub global_tmps: BTreeMap<StoreType, GlobalRef>,
    // Symbols that need to be always treated as static (not converted to GOT-relative)
    // pub always_static_symbols: BTreeSet<EntityKind>,
}

impl CodeRelocationHandler {
    pub fn new(always_static_symbols: &BTreeSet<EntityKind>) -> Self {
        todo!()
        // Self {
        //     memory_base: None,
        //     global_tmps: BTreeMap::new(),
        //     always_static_symbols: always_static_symbols.clone(),
        // }
    }
    pub fn is_dyn_symbol(&self, entry: &RelocationEntry) -> bool {
        todo!()
        // self.memory_base.is_some()
        //     && !self
        //         .always_static_symbols
        //         .contains(&entry.symbol_id.combine(entry.symbol_type))
    }
}

impl<'src> HandleReloc<'src> for CodeRelocationHandler {
    type ExtraData = ();
    fn setup(
        &mut self,
        builder: &mut crate::typed::ModuleBuilder<'src>,
        /* extra info ?*/
    ) -> Result<()> {
        let memory_base = match builder.mem_spec.mem_start {
            SpecificLocation::GotBased { global, .. } => Some(global),
            _ => None,
        };

        if memory_base.is_none() {
            return Ok(());
        }
        debug_assert!(self.global_tmps.is_empty());
        // TODO: ensure_global_got_base exist
        for (store_type, content_type) in init_each_store_var() {
            //TODO: Don't assert that this id won't shift
            let global_id =
                builder.globals.items.imports.len() + builder.globals.items.defined.len();
            self.global_tmps
                .insert(store_type, GlobalRef::new(global_id));

            builder.globals.push_defined(DefinedGlobal {
                entity_type: GlobalType {
                    content_type,
                    mutable: true,
                    shared: false,
                },
                body: EntityBody::New {
                    new_relocs: SVec::new(),
                    new_bytes: global_init_tmp(content_type),
                },
            });
        }

        todo!();
        // self.memory_base = memory_base;
        Ok(())
    }

    fn create_entry(
        &self,
        buffer: Cursor<'src>,
        entry: RelocationEntry,
    ) -> Result<ModifyOrReloc<Self::ExtraData>> {
        // Only apply if dynamic base is enabled
        // let Some(memory_base) = self.memory_base else {
        //     return Ok(ModifyOrReloc::OriginalReloc(entry));
        // };

        // TODO: move outside of this creation
        Self::check_whitelisted_code_relocation(&entry)?;

        todo!();
        // match entry.symbol_type {
        //     SymbolType::TableIndex | SymbolType::MemoryAddr if self.is_dyn_symbol(&entry) => {
        //         return self
        //             .new_entry(memory_base, buffer, entry)
        //             .map(ModifyOrReloc::Modify);
        //     }
        //     _ => {}
        // }

        Ok(ModifyOrReloc::OriginalReloc(entry))
    }
}

impl<'src> CodeRelocationHandler {
    fn new_entry(
        &self,
        memory_base: GlobalRef,
        mut buffer: Cursor<'src>,
        entry: RelocationEntry,
    ) -> Result<ModificationEntry<()>> {
        assert!(matches!(
            (entry.symbol_type, entry.encoding),
            (SymbolType::TableIndex, Encoding::Sleb)
                | (SymbolType::MemoryAddr, Encoding::Sleb)
                | (SymbolType::MemoryAddr, Encoding::Leb)
        ));

        let ix_size = match entry.encoding {
            Encoding::Leb => 2,  // memoryaddr_leb
            Encoding::Sleb => 1, // memoryaddr_sleb | tableindex_sleb
            _ => {
                bail!("Unsupported relocation type")
            }
        };
        buffer.try_extend_before(ix_size)?;

        let instruction = {
            let bin_reader = wasmparser::BinaryReader::new(buffer.green_buf(), 0);
            let mut op_reader = wasmparser::OperatorsReader::new(bin_reader);
            let instr = op_reader.read()?;
            if !op_reader.eof() {
                bail!("Unexpected extra operators after reading instruction")
            }
            instr
        };

        Ok(ModificationEntry {
            rewrite: Some(self.generate_patch(memory_base, entry, instruction)?),
            original_reloc: entry,
            extra_info: (),
        })
    }

    fn generate_patch(
        &self,
        memory_base: GlobalRef,
        entry: RelocationEntry,
        instruction: wasmparser::Operator<'src>,
    ) -> Result<Rewrite> {
        let rewrite = match (entry.symbol_type, entry.encoding) {
            (SymbolType::MemoryAddr, Encoding::Leb) => {
                self.replace_memory_offset_with_global_get(memory_base, entry, instruction)?
            }
            (SymbolType::MemoryAddr, Encoding::Sleb) => {
                self.replace_const_get_with_global_get(memory_base, entry, instruction)?
            }
            (SymbolType::TableIndex, Encoding::Sleb) => {
                self.replace_const_get_with_global_get(memory_base, entry, instruction)?
            }
            _ => {
                bail!("Unsupported relocation type")
            }
        };

        Ok(rewrite)
    }

    // Simple replace of i32.const with global.get + i32.add
    // Retuns size of the replacement
    fn replace_const_get_with_global_get(
        &self,
        memory_base: GlobalRef,
        old_entry: RelocationEntry,
        instruction: Operator<'src>,
    ) -> Result<Rewrite> {
        ensure!(
            matches!(instruction, Operator::I32Const { .. }),
            "Unsupported relocation operand"
        );

        let got_offset = 0i32;
        let got_global_index = memory_base;
        let mut new_bytes = SVec::new();
        let mut new_relocs = SVec::new();

        let mut writer = super::wasm_emitter::Encoder::new(
            &mut new_bytes,
            old_entry.relocation_range().start as u32,
        );

        log::trace!(
            "Replacing {src_ix:?} with gapped entry (global.get {got_ix} + i32.const {offset})",
            src_ix = instruction,
            got_ix = got_global_index,
            offset = got_offset
        );

        let got_rel_offset = writer.global_get(got_global_index.as_u32())?;
        let const_rel_offset = writer.i32_const(got_offset)?;
        writer.i32_add()?;

        // TODO: Return new list of relocations to GlobalGet and I32Const (GlobalIndexLeb + MemoryAddrLeb | TableIndexLeb)
        new_relocs.push(OutputRelocationEntry {
            symbol_id: OutputEntityRef::resolved(got_global_index), // to symbol_id
            offset: got_rel_offset,
            encoding: Encoding::Leb,
            width: RelocationWidth::Bits32,
            relation: Relative::None,
            symbol_type: SymbolType::GlobalIndex,
            addend: 0,
        });

        new_relocs.push(OutputRelocationEntry {
            // TODO: Handle old memory index
            symbol_id: OutputEntityRef::from_input(old_entry.symbol_id),
            offset: const_rel_offset,
            relation: Relative::Got,

            encoding: old_entry.encoding,
            width: old_entry.width,
            symbol_type: old_entry.symbol_type,
            addend: old_entry.addend,
        });
        Ok(Rewrite {
            old_range: old_entry.relocation_range(),
            new_relocs,
            new_bytes,
        })
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
    fn replace_memory_offset_with_global_get(
        &self,
        memory_base: GlobalRef,
        entry: RelocationEntry,
        instruction: wasmparser::Operator<'_>,
    ) -> Result<Rewrite> {
        let got_offset = 0;
        let got_global_index = memory_base;

        let mut new_bytes = SVec::new();
        let mut new_relocs = SVec::new();

        let mut writer = super::wasm_emitter::Encoder::new(
            &mut new_bytes,
            entry.relocation_range().start as u32,
        );
        let store = Self::store_type(&instruction)?;

        log::trace!(
            "Replacing {orig_ix:?} with [global.get {got_global_index} + i32.const {got_offset:?} + ix] global_store:{store:?}",
            orig_ix = instruction,
        );

        // TODO: Replace with local?
        // store <value> to temp global
        if let Some(store_type) = &store {
            let global_id = *self.global_tmps.get(store_type).unwrap();
            let reloc_offset = writer.global_get(global_id.as_u32())?;
            new_relocs.push(OutputRelocationEntry {
                symbol_id: OutputEntityRef::resolved(global_id),
                offset: reloc_offset,
                encoding: Encoding::Leb,
                symbol_type: SymbolType::GlobalIndex,
                width: RelocationWidth::Bits32,
                relation: Relative::None,
                addend: 0,
            })
        }
        // TODO: we can emit relocation for this global
        let got_offset = writer.global_get(got_global_index.as_u32())?;
        writer.i32_add()?;
        // add offset from global_index variable to the dyn_offset part of instruction
        // restore <value> from temp global
        if let Some(store_type) = &store {
            let global_id = *self.global_tmps.get(store_type).unwrap();
            let reloc_offset = writer.global_get(global_id.as_u32())?;
            new_relocs.push(OutputRelocationEntry {
                symbol_id: OutputEntityRef::resolved(global_id),
                offset: reloc_offset,
                encoding: Encoding::Leb,
                symbol_type: SymbolType::GlobalIndex,
                width: RelocationWidth::Bits32,
                relation: Relative::None,
                addend: 0,
            })
        }
        let mem_offsets = self.encode_store_ix(&mut writer, &instruction)?; // And now write original instruction

        new_relocs.push(OutputRelocationEntry {
            //TODO: Convert to OutputSymbolId
            symbol_id: OutputEntityRef::from_input(entry.symbol_id),
            offset: mem_offsets.offset,
            relation: Relative::Got,

            encoding: entry.encoding,
            width: entry.width,
            symbol_type: entry.symbol_type,
            addend: entry.addend,
        });

        Ok(Rewrite {
            new_bytes,
            new_relocs,
            old_range: entry.relocation_range(),
        })
    }

    fn store_type(ix: &Operator) -> Result<Option<StoreType>> {
        Ok(match ix {
            Operator::F32Load { .. }
            | Operator::F64Load { .. }
            | Operator::I32Load { .. }
            | Operator::I64Load { .. }
            | Operator::I32Load8U { .. }
            | Operator::I32Load8S { .. }
            | Operator::I32Load16U { .. }
            | Operator::I32Load16S { .. }
            | Operator::I64Load8U { .. }
            | Operator::I64Load8S { .. }
            | Operator::I64Load16U { .. }
            | Operator::I64Load16S { .. }
            | Operator::I64Load32U { .. }
            | Operator::I64Load32S { .. } => None,

            Operator::I32Store { .. } => Some(StoreType::I32),
            Operator::I64Store { .. } => Some(StoreType::I64),
            Operator::I32Store8 { .. } => Some(StoreType::I32),
            Operator::I32Store16 { .. } => Some(StoreType::I32),
            Operator::I64Store8 { .. } => Some(StoreType::I64),
            Operator::I64Store16 { .. } => Some(StoreType::I64),
            Operator::I64Store32 { .. } => Some(StoreType::I64),
            _ => {
                bail!("Unsupported relocation instruction: {instr:?}", instr = ix);
            }
        })
    }

    fn encode_store_ix<W: Write>(
        &self,
        writer: &mut super::wasm_emitter::Encoder<W>,
        ix: &Operator,
    ) -> Result<MemArgOffsets> {
        let fix_offset = |memarg: &wasmparser::MemArg| wasm_encoder::MemArg {
            align: memarg.align as u32,
            memory_index: memarg.memory,
            offset: memarg.offset,
        };
        let res = match ix {
            Operator::F32Load { memarg } => writer.f32_load(fix_offset(memarg))?,
            Operator::F64Load { memarg } => writer.f64_load(fix_offset(memarg))?,
            Operator::I32Load { memarg } => writer.i32_load(fix_offset(memarg))?,
            Operator::I64Load { memarg } => writer.i64_load(fix_offset(memarg))?,
            Operator::I32Load8U { memarg } => writer.i32_load8_u(fix_offset(memarg))?,
            Operator::I32Load8S { memarg } => writer.i32_load8_s(fix_offset(memarg))?,
            Operator::I32Load16U { memarg } => writer.i32_load16_u(fix_offset(memarg))?,
            Operator::I32Load16S { memarg } => writer.i32_load16_s(fix_offset(memarg))?,
            Operator::I64Load8U { memarg } => writer.i64_load8_u(fix_offset(memarg))?,
            Operator::I64Load8S { memarg } => writer.i64_load8_s(fix_offset(memarg))?,
            Operator::I64Load16U { memarg } => writer.i64_load16_u(fix_offset(memarg))?,
            Operator::I64Load16S { memarg } => writer.i64_load16_s(fix_offset(memarg))?,
            Operator::I64Load32U { memarg } => writer.i64_load32_u(fix_offset(memarg))?,
            Operator::I64Load32S { memarg } => writer.i64_load32_s(fix_offset(memarg))?,

            Operator::I32Store { memarg } => writer.i32_store(fix_offset(memarg))?,
            Operator::I64Store { memarg } => writer.i64_store(fix_offset(memarg))?,
            Operator::I32Store8 { memarg } => writer.i32_store8(fix_offset(memarg))?,
            Operator::I32Store16 { memarg } => writer.i32_store16(fix_offset(memarg))?,
            Operator::I64Store8 { memarg } => writer.i64_store8(fix_offset(memarg))?,
            Operator::I64Store16 { memarg } => writer.i64_store16(fix_offset(memarg))?,
            Operator::I64Store32 { memarg } => writer.i64_store32(fix_offset(memarg))?,
            _ => {
                bail!("Unsupported relocation instruction: {instr:?}", instr = ix);
            }
        };
        Ok(res)
    }

    pub(crate) fn check_whitelisted_code_relocation(entry: &RelocationEntry) -> Result<()> {
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
