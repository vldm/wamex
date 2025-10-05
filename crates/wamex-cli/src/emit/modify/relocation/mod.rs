pub mod encode;

use std::fmt::Debug;

use anyhow::{anyhow, bail, Result};
use wasmparser::RelocationEntry;

use crate::{
    emit::{
        index_safety::{OutputFuncId, OutputGlobalId},
        modify::SymbolOp,
        ModuleEmitState,
    },
    index::{AnySymbolId, DataSegmentId, DataSymbolId, Id, InputFuncId, InputGlobalId},
    read::{linking::SymbolIndex, InputModule},
};

pub trait EntryTypeTag {
    type OutputValue;
    // Index or offset of symbol in corresponding module
    fn get_mapped_value(
        input_module: &InputModule<'_>,
        state: &ModuleEmitState,
        src_symbol: AnySymbolId,
    ) -> Option<Self::OutputValue>;
    fn get_got(state: &ModuleEmitState) -> Option<OutputGlobalId>;
}

pub enum FunctionId {}
pub enum FunctionTableIndex {}
pub enum DataSymbolTag {}

impl FunctionId {
    fn get_input_function_id(
        input_module: &InputModule<'_>,
        src_symbol: AnySymbolId,
    ) -> Option<InputFuncId> {
        let Some(SymbolIndex::Func(input_func_id)) = input_module
            .linking
            .linking_symbols
            .original_indexes
            .get(src_symbol as usize)
        else {
            return None;
        };
        Some(*input_func_id)
    }
}

impl EntryTypeTag for FunctionId {
    type OutputValue = OutputFuncId;
    fn get_mapped_value(
        input_module: &InputModule<'_>,
        state: &ModuleEmitState,
        src_symbol: AnySymbolId,
    ) -> Option<Self::OutputValue> {
        let input_func_id = FunctionId::get_input_function_id(input_module, src_symbol)?;
        let Some(output_func_id) = state.functions.get_output_id(input_func_id) else {
            return None;
        };
        Some(output_func_id)
    }
    fn get_got(state: &ModuleEmitState) -> Option<OutputGlobalId> {
        state.sub_module_extra.as_ref().map(|e| e.lib_base_id)
    }
}
impl EntryTypeTag for FunctionTableIndex {
    type OutputValue = usize;
    fn get_mapped_value(
        input_module: &InputModule<'_>,
        state: &ModuleEmitState,
        src_symbol: AnySymbolId,
    ) -> Option<Self::OutputValue> {
        let input_func_id = FunctionId::get_input_function_id(input_module, src_symbol)?;
        let Some(&table_index) = state
            .indirect_functions
            .function_table_index
            .get(&input_func_id)
        else {
            return None;
        };
        Some(table_index)
    }
    fn get_got(state: &ModuleEmitState) -> Option<OutputGlobalId> {
        FunctionId::get_got(state)
    }
}

impl DataSymbolTag {
    fn get_symbol_offset(
        state: &ModuleEmitState,
        segment_id: &DataSegmentId,
        data_symbol_id: &DataSymbolId,
    ) -> Option<<DataSymbolTag as EntryTypeTag>::OutputValue> {
        let (segment_id, data_index): &(DataSegmentId, usize) = state
            .input_data_to_output_id
            .get(&(*segment_id, *data_symbol_id))?;
        let segment = state.data.get(*segment_id)?;
        let data = segment.symbols().get(*data_index)?;
        if !segment.is_active() {
            return None;
        }
        Some(segment.memory_offset() as i64 + data.data_offset as i64)
    }
}
impl EntryTypeTag for DataSymbolTag {
    type OutputValue = i64;
    fn get_mapped_value(
        input_module: &InputModule<'_>,
        state: &ModuleEmitState,
        src_symbol: AnySymbolId,
    ) -> Option<Self::OutputValue> {
        let Some(SymbolIndex::DataDefined(segment_id, data_index)) = input_module
            .linking
            .linking_symbols
            .original_indexes
            .get(src_symbol as usize)
        else {
            return None;
        };

        DataSymbolTag::get_symbol_offset(state, segment_id, data_index)
    }
    fn get_got(state: &ModuleEmitState) -> Option<OutputGlobalId> {
        state.sub_module_extra.as_ref().map(|e| e.lib_base_id)
    }
}

#[derive(Clone)]
pub struct RelocateState<'any, 'src> {
    pub input_module: &'any InputModule<'src>,
    pub main_module: &'any ModuleEmitState<'any, 'src>,
    pub global_id_mapper: &'any dyn Fn(InputGlobalId) -> Option<OutputGlobalId>,
    pub emit_module: &'any ModuleEmitState<'any, 'src>,
}

impl Debug for RelocateState<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelocateState")
            .field("input_module", &"InputModule { ... }")
            .field("main_module", &"ModuleEmitState { ... }")
            .field("emit_module", &"ModuleEmitState { ... }")
            .finish()
    }
}

impl RelocateState<'_, '_> {
    pub fn get_entry_symbol_op<T: EntryTypeTag>(
        &self,
        relocation: &RelocationEntry,
    ) -> Result<SymbolOp<T::OutputValue>> {
        // TODO: reverse order (dyn then static)?
        if let Some(value) = T::get_mapped_value(
            &self.input_module,
            &self.main_module,
            relocation.index as AnySymbolId,
        ) {
            return Ok(SymbolOp::StaticOffset { value });
        }

        let Some(value) = T::get_mapped_value(
            self.input_module,
            &self.emit_module,
            relocation.index as AnySymbolId,
        ) else {
            bail!(
                "Symbol within relocation {relocation:?} not found in either main or emit module"
            );
        };
        Ok(SymbolOp::GotBased {
            got: T::get_got(&self.emit_module).ok_or_else(|| {
                anyhow!(
                    "No GOT global for symbol {src:?} in emit module",
                    src = relocation.index
                )
            })?,
            value,
        })
    }

    pub fn get_data_symbol_op(
        &self,
        segment_id: DataSegmentId,
        data_symbol_id: DataSymbolId,
    ) -> Result<SymbolOp<<DataSymbolTag as EntryTypeTag>::OutputValue>> {
        if let Some(value) =
            DataSymbolTag::get_symbol_offset(self.main_module, &segment_id, &data_symbol_id)
        {
            return Ok(SymbolOp::StaticOffset { value });
        }
        let Some(value) =
            DataSymbolTag::get_symbol_offset(self.emit_module, &segment_id, &data_symbol_id)
        else {
            bail!(
                "Symbol {segment_id:?}: {data_symbol_id:?} not found in either main or emit module"
            )
        };
        Ok(SymbolOp::GotBased {
            got: DataSymbolTag::get_got(&self.emit_module).ok_or_else(|| {
                anyhow!("No GOT global for symbol {segment_id:?}: {data_symbol_id:?}")
            })?,
            value,
        })
    }

    fn get_relocated_function_index(&self, relocation: &RelocationEntry) -> Result<usize> {
        let result = self.get_entry_symbol_op::<FunctionId>(relocation)?;
        Ok(result
            .as_static()
            .expect("Relocation should only process static symbols")
            .as_raw_index())
    }

    fn get_relocated_function_table_index(&self, relocation: &RelocationEntry) -> Result<usize> {
        let result = self.get_entry_symbol_op::<FunctionTableIndex>(relocation)?;
        Ok(*result
            .as_static()
            .expect("Relocation should only process static symbols"))
    }

    fn get_relocated_memory_offset(&self, relocation: &RelocationEntry) -> Result<usize> {
        let mut offset = *self
            .get_entry_symbol_op::<DataSymbolTag>(relocation)?
            .as_static()
            .expect("Relocation should only process static symbols");
        if relocation.addend < 0 {
            log::warn!("Relocation {relocation:?} has negative addend");
        }
        offset += relocation.addend;

        Ok(offset as usize)
    }

    fn get_global_id(&self, relocation: &RelocationEntry) -> Result<usize> {
        let Some(SymbolIndex::Global(original_global_id)) = self
            .input_module
            .linking
            .linking_symbols
            .original_indexes
            .get(relocation.index as usize)
        else {
            bail!("Relocation {relocation:?} does not refer to a valid global");
        };
        let global_id = (self.global_id_mapper)(*original_global_id)
            .ok_or_else(|| {
                anyhow!(
                    "Dependency analysis error: No output global for input global {original_global_id} referenced by relocation {relocation:?}"
                )
            })?;
        Ok(global_id.as_raw_index())
    }

    pub fn apply_relocation(&self, data: &mut [u8], relocation: &RelocationEntry) -> Result<()> {
        let relocation_range = relocation.relocation_range();
        let target = &mut data[relocation_range];
        use encode::*;
        use wasmparser::RelocationType::*;
        match relocation.ty {
            FunctionIndexLeb => {
                encode_leb128_u32_5byte(
                    self.get_relocated_function_index(relocation)? as u32,
                    target.try_into().unwrap(),
                );
            }
            TableIndexSleb => {
                encode_leb128_i32_5byte(
                    self.get_relocated_function_table_index(relocation)? as i32,
                    target.try_into().unwrap(),
                );
            }
            TypeIndexLeb => {
                // we keep types from input module, so we can ignore this relocation for now
                // TODO: Implement relocation.
            }
            TableNumberLeb => {
                // Table number also is only 1 <indirect function table>
            }
            TableIndexI32 => {
                encode_u32(
                    self.get_relocated_function_table_index(relocation)? as u32,
                    target.try_into().unwrap(),
                );
            }
            // 64-bit wasm disabled for now
            // TableIndexSleb64 => {
            //     encode_leb128_i64_10byte(
            //         self.get_relocated_function_table_index(relocation)? as i64,
            //         target.try_into().unwrap(),
            //     );
            // }
            // TableIndexI64 => {
            //     encode_u64(
            //         self.get_relocated_function_table_index(relocation)? as u64,
            //         target.try_into().unwrap(),
            //     );
            // }
            FunctionIndexI32 => {
                encode_u32(
                    self.get_relocated_function_index(relocation)? as u32,
                    target.try_into().unwrap(),
                );
            }
            MemoryAddrLeb => {
                encode_leb128_u32_5byte(
                    self.get_relocated_memory_offset(relocation)? as u32,
                    target.try_into().unwrap(),
                );
            }
            MemoryAddrSleb => {
                encode_leb128_i32_5byte(
                    self.get_relocated_memory_offset(relocation)? as i32,
                    target.try_into().unwrap(),
                );
            }
            MemoryAddrI32 => {
                encode_u32(
                    self.get_relocated_memory_offset(relocation)? as u32,
                    target.try_into().unwrap(),
                );
            }
            GlobalIndexLeb => {
                encode_leb128_u32_5byte(
                    self.get_global_id(relocation)? as u32,
                    target.try_into().unwrap(),
                );
            }

            _ => {
                panic!("Unsupported relocation type {relocation:?}");
            }
        }

        Ok(())
    }
}
