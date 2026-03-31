//! Patch code to use GOT-relative addresses instead of absolute for memory and indiret_function_table accesses.
//!
//! Uses relocation entries to find place in code and symbols that need to make relocatable.

use std::{collections::BTreeMap, io::Write, ops::Range};

use anyhow::{Result, bail, ensure};
use cranelift_entity::EntityRef;
use wasmparser::{GlobalType, Operator};

use super::FixupFromRelocs;
use crate::{
    SVec,
    emit::{
        modify::{
            Cursor, OutputEntityRef, OutputRelocationEntry, Rewrite,
            blacklist::{Blacklist, IsSet},
            wasm_emitter::{self, MemArgOffsets},
        },
        plan::{AddressingMode, OutputModuleCopyPlan},
    },
    index::Temp,
    linkage::reloc::{
        Encoding, EntityAddressMode, EntityRelocationEntry, Relative, RelocationWidth,
    },
    typed::{
        DefinedGlobal, EntityBody, EntityKind, ExportNames, FileId, FunctionRef, GlobalRef,
        snapshot::EntitiesSnapshot,
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

#[derive(derive_more::Debug)]
pub struct CodeAbsToGot<F>
where
    Blacklist<F>: IsSet,
{
    // Temporary globals for constant extraction
    pub global_tmps: BTreeMap<StoreType, GlobalRef>,
    // Symbols (in input space) that need to be always treated as static (not converted to GOT-relative)
    pub static_symbols: Blacklist<F>,
}

impl<F> CodeAbsToGot<F>
where
    Blacklist<F>: IsSet,
{
    pub fn is_dyn_symbol(&self, input_snapshot: &EntitiesSnapshot, sym: &EntityKind) -> bool {
        let sym = input_snapshot.pack_ref(*sym);
        // 1. For main - there should be no imported deps. (CodeRelocationHandler shouldn't be constructed for main module)
        // 2. for other modules - static symbols can be refered as-is, other should be converted to GOT-relative.
        !self.static_symbols.contains(sym)
    }

    /// Module related configuration.
    fn setup_module(&mut self, builder: &mut crate::typed::ModuleBuilder<'_>) -> Result<()> {
        // let main_vs = builder.phase.mem_layout.main_vs();
        // let memory_base = main_vs.and_then(|vs| vs.location.global_ref());

        // if memory_base.is_none() {
        //     return Ok(());
        // }
        debug_assert!(self.global_tmps.is_empty());
        // TODO: ensure_global_got_base exist
        for (store_type, content_type) in init_each_store_var() {
            //TODO: Don't assert that this id won't shift
            let global_id =
                builder.globals.imports_iter().len() + builder.globals.defined_iter().len();
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
                export_as: ExportNames::default(),
                name: None,
            });
        }

        Ok(())
    }
}

impl<'src, F> FixupFromRelocs<'src> for CodeAbsToGot<F>
where
    Blacklist<F>: IsSet,
{
    type ExtraData = ();
    type SetupData = Blacklist<F>;
    type EntityRef = FunctionRef;

    fn setup(
        shared: Self::SetupData,
        plan: &OutputModuleCopyPlan,
        module: &mut crate::typed::ModuleBuilder<'src>,
    ) -> Result<Option<Self>>
    where
        Self: Sized,
    {
        if matches!(plan.addressing, AddressingMode::Static) {
            // No need to create handler if we won't convert any symbol to got-based.
            return Ok(None);
        }
        let mut instance = Self {
            global_tmps: BTreeMap::new(),
            static_symbols: shared,
        };
        instance.setup_module(module).unwrap();
        Ok(Some(instance))
    }

    fn create_entry(
        &self,
        _entity_ref: Temp<Self::EntityRef>,
        buffer: Cursor<'src>,
        (_, input_snapshot): (FileId, &EntitiesSnapshot),
        entry: EntityRelocationEntry,
    ) -> Result<Option<(Rewrite, Self::ExtraData)>> {
        match entry.symbol_id {
            EntityKind::DataSymbol(_) | EntityKind::Function(_)
                if entry.symbol_op == EntityAddressMode::RuntimeAddr
                    && self.is_dyn_symbol(input_snapshot, &entry.symbol_id) =>
            {
                Self::check_whitelisted_code_relocation(&entry)?;
                return self.new_entry(buffer, entry);
            }
            _ => {}
        }

        Ok(None)
    }
}

impl<'src, F> CodeAbsToGot<F>
where
    Blacklist<F>: IsSet,
{
    fn new_entry(
        &self,
        mut buffer: Cursor<'src>,
        entry: EntityRelocationEntry,
    ) -> Result<Option<(Rewrite, ())>> {
        log::debug!("Creating modification entry for {:#?}", entry);
        assert!(matches!(
            (entry.symbol_id, entry.encoding),
            (EntityKind::Function(_), Encoding::Sleb)
                | (EntityKind::DataSymbol(_), Encoding::Sleb)
                | (EntityKind::DataSymbol(_), Encoding::Leb)
        ));
        assert!(matches!(entry.symbol_op, EntityAddressMode::RuntimeAddr));

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
        log::trace!(
            "INSTRUCTION {:#?}, green_range: {:?}, reloc_range: {:?}",
            instruction,
            buffer.green_range(),
            entry.relocation_range()
        );

        Ok(Some((
            self.generate_patch(entry, buffer.green_range(), instruction)?,
            (),
        )))
    }

    fn generate_patch(
        &self,
        entry: EntityRelocationEntry,
        ix_range: Range<usize>,
        instruction: wasmparser::Operator<'src>,
    ) -> Result<Rewrite> {
        let rewrite = match (entry.symbol_id, entry.encoding) {
            (EntityKind::DataSymbol(_), Encoding::Leb) => {
                self.replace_memory_offset_with_global_get(entry, ix_range, instruction)?
            }
            (EntityKind::DataSymbol(_), Encoding::Sleb) => {
                self.replace_const_get_with_global_get(entry, ix_range, instruction)?
            }
            (EntityKind::Function(_), Encoding::Sleb)
                if entry.symbol_op == EntityAddressMode::RuntimeAddr =>
            {
                self.replace_const_get_with_global_get(entry, ix_range, instruction)?
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
        old_entry: EntityRelocationEntry,
        ix_range: Range<usize>,
        instruction: Operator<'src>,
    ) -> Result<Rewrite> {
        ensure!(
            matches!(instruction, Operator::I32Const { .. }),
            "Unsupported relocation operand"
        );

        let got_offset = 0i32;
        let mut new_bytes = SVec::new();
        let mut new_relocs = SVec::new();

        let mut writer = wasm_emitter::Encoder::new(&mut new_bytes, 0);

        log::trace!(
            "Replacing {src_ix:?} with gapped entry (global.get <placeholder> + i32.const {offset})",
            src_ix = instruction,
            offset = got_offset
        );

        let got_rel_offset = writer.global_get_invalid()?;
        let const_rel_offset = writer.i32_const(got_offset)?;
        writer.i32_add()?;

        new_relocs.push(
            EntityRelocationEntry::index_base(got_rel_offset, old_entry.symbol_id)
                .into_from_input(),
        );
        let mut entry = old_entry.into_from_input();
        entry.relation = Relative::Got;
        entry.offset = const_rel_offset;

        new_relocs.push(entry);
        Ok(Rewrite {
            old_range: ix_range,
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
        old_entry: EntityRelocationEntry,
        ix_range: Range<usize>,
        instruction: wasmparser::Operator<'_>,
    ) -> Result<Rewrite> {
        let got_offset = 0;

        let mut new_bytes = SVec::new();
        let mut new_relocs = SVec::new();

        let mut writer = wasm_emitter::Encoder::new(&mut new_bytes, 0);
        let store = Self::store_type(&instruction)?;

        log::trace!(
            "Replacing {orig_ix:?} with [global.get <placeholder> + i32.const {got_offset:?} + ix] global_store:{store:?}",
            orig_ix = instruction,
        );
        // TODO: Replace with local?
        // store <value> to temp global
        if let Some(store_type) = &store {
            let global_id = *self.global_tmps.get(store_type).unwrap();
            let reloc_offset = writer.global_get(global_id.as_u32())?;
            new_relocs.push(OutputRelocationEntry {
                symbol_id: OutputEntityRef::resolved(global_id.into()),
                symbol_op: EntityAddressMode::StaticIndex,
                offset: reloc_offset,
                encoding: Encoding::Leb,
                width: RelocationWidth::Bits32,
                relation: Relative::None,
                addend: 0,
            })
        }
        // TODO: we can emit relocation for this global
        let got_offset = writer.global_get_invalid()?;
        // entity should have information about GOT they used, since there maybe more than one.
        new_relocs.push(
            EntityRelocationEntry::index_base(got_offset, old_entry.symbol_id).into_from_input(),
        );
        writer.i32_add()?;
        // add offset from global_index variable to the dyn_offset part of instruction
        // restore <value> from temp global
        if let Some(store_type) = &store {
            let global_id = *self.global_tmps.get(store_type).unwrap();
            let reloc_offset = writer.global_get(global_id.as_u32())?;
            new_relocs.push(OutputRelocationEntry {
                symbol_id: OutputEntityRef::resolved(global_id.into()),
                symbol_op: EntityAddressMode::StaticIndex,
                offset: reloc_offset,
                encoding: Encoding::Leb,
                width: RelocationWidth::Bits32,
                relation: Relative::None,
                addend: 0,
            })
        }
        let mem_offsets = self.encode_store_ix(&mut writer, &instruction)?; // And now write original instruction

        let mut entry = old_entry.into_from_input();
        entry.relation = Relative::Got;
        entry.offset = mem_offsets.offset;

        new_relocs.push(entry);

        Ok(Rewrite {
            new_bytes,
            new_relocs,
            old_range: ix_range,
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
        writer: &mut wasm_emitter::Encoder<W>,
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

    pub(crate) fn check_whitelisted_code_relocation(entry: &EntityRelocationEntry) -> Result<()> {
        if matches!(entry.width, RelocationWidth::Bits64) {
            bail!("U64 memory pointers is currently not supported")
        }
        if !matches!(entry.relation, Relative::None) {
            bail!("Relocation memory pointers is currently not supported")
        }
        if !matches!(entry.symbol_op, EntityAddressMode::RuntimeAddr) {
            bail!(
                "Non supported symbol type for code relocation {:?}",
                entry.symbol_op
            )
        }
        Ok(())
    }
}
