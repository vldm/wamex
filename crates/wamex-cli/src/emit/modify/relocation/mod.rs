pub mod encode;

use std::fmt::Debug;

use anyhow::{anyhow, bail, Result};
use wasmparser::RelocationEntry;

use crate::{
    analysis::{self, symbols::SymbolKind, ModuleInfo},
    emit::{
        index_safety::OutputGlobalId, modify::SymbolOp, ComputedModules, GotBase, ModuleEmitState,
    },
    index::{DataSegmentId, Id, InputFuncId, InputGlobalId, SymbolId},
    read::InputModule,
};

pub trait EntryTypeTag {
    type OutputValue;
    // Index or offset of symbol in corresponding module
    fn get_mapped_value(
        input: &analysis::ModuleInfo<'_>,
        state: &ModuleEmitState,
        src_symbol: SymbolId,
    ) -> Option<Self::OutputValue>;
    fn get_got(got_base: &GotBase) -> OutputGlobalId;
}

pub enum FunctionIndexTag {}
pub enum DataSymbolTag {}

impl FunctionIndexTag {
    fn get_input_function_id(
        input: &analysis::ModuleInfo<'_>,
        src_symbol: SymbolId,
    ) -> Option<InputFuncId> {
        let SymbolKind::Func { input_id } = input.symbols.get(src_symbol)?.kind else {
            return None;
        };
        Some(input_id)
    }
}

impl EntryTypeTag for FunctionIndexTag {
    type OutputValue = usize;
    fn get_mapped_value(
        input: &analysis::ModuleInfo<'_>,
        state: &ModuleEmitState,
        src_symbol: SymbolId,
    ) -> Option<Self::OutputValue> {
        let input_func_id = FunctionIndexTag::get_input_function_id(input, src_symbol)?;
        state
            .indirect_functions
            .function_table_index
            .get(&input_func_id)
            .copied()
    }
    fn get_got(got_base: &GotBase) -> OutputGlobalId {
        got_base.table_base_id
    }
}

impl DataSymbolTag {
    fn get_symbol_offset(
        state: &ModuleEmitState,
        segment_id: DataSegmentId,
        data_symbol_id: SymbolId,
    ) -> Option<<DataSymbolTag as EntryTypeTag>::OutputValue> {
        let segment = state.data.get(segment_id)?;
        let symbol = segment.symbols().get(&data_symbol_id);
        let Some(symbol) = symbol else {
            return None;
        };

        Some(segment.memory_offset() as i64 + symbol.data_mem_offset as i64)
    }
}
impl EntryTypeTag for DataSymbolTag {
    type OutputValue = i64;
    fn get_mapped_value(
        input: &ModuleInfo<'_>,
        state: &ModuleEmitState,
        src_symbol: SymbolId,
    ) -> Option<Self::OutputValue> {
        let SymbolKind::DataDefined { segment_id, .. } = input.symbols.get(src_symbol)?.kind else {
            return None;
        };

        DataSymbolTag::get_symbol_offset(state, segment_id, src_symbol)
    }
    fn get_got(got_base: &GotBase) -> OutputGlobalId {
        got_base.lib_base_id
    }
}

#[derive(Clone)]
pub struct RelocateState<'any, 'src> {
    pub input_module: &'any analysis::ModuleInfo<'src>,
    pub computed_modules: &'any ComputedModules<'any, 'src>,
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
    fn _get_symbol_op<T: EntryTypeTag, U>(
        &self,
        getter: impl Fn(&ModuleEmitState) -> Option<U>,
        not_found: impl FnOnce() -> anyhow::Error,
        no_got: impl FnOnce() -> anyhow::Error,
    ) -> Result<SymbolOp<U>> {
        if let Some(value) = getter(&self.computed_modules.main_module) {
            return Ok(SymbolOp::StaticOffset { value });
        }
        if let Some(value) = getter(self.emit_module) {
            return Ok(SymbolOp::GotBased {
                value,
                got: self
                    .emit_module
                    .get_submodule_extra(None)
                    .map(|extra| T::get_got(extra))
                    .ok_or_else(no_got)?,
            });
        }

        for (id, module) in &self.computed_modules.shared_modules {
            if let Some(value) = getter(module) {
                return Ok(SymbolOp::GotBased {
                    value,

                    got: self
                        .emit_module
                        .get_submodule_extra(Some(id))
                        .map(|extra| T::get_got(extra))
                        .ok_or_else(no_got)?,
                });
            }
        }

        Err(not_found())
    }

    pub fn get_entry_symbol_op<T: EntryTypeTag>(
        &self,
        relocation: &RelocationEntry,
    ) -> Result<SymbolOp<T::OutputValue>> {
        self._get_symbol_op::<T, _>(
            |module| {
                T::get_mapped_value(self.input_module, module, Id::from_index(relocation.index))
            },
            || {
                anyhow!(
                "Symbol within relocation {relocation:?} not found in either main or emit module"
            )
            },
            || {
                anyhow!(
                    "No GOT global for symbol {src:?} in emit module",
                    src = relocation.index
                )
            },
        )
    }

    pub fn get_data_symbol_op(
        &self,
        segment_id: DataSegmentId,
        data_symbol_id: SymbolId,
    ) -> Result<SymbolOp<<DataSymbolTag as EntryTypeTag>::OutputValue>> {
        self._get_symbol_op::<DataSymbolTag, _>(
            |module| {
                DataSymbolTag::get_symbol_offset(module, segment_id, data_symbol_id)
            },
            || {
                anyhow!(
                    "Data symbol {segment_id:?}: {data_symbol_id:?} not found in either main or emit module"
                )
            },
            || {
                anyhow!(
                    "No GOT global for data symbol {segment_id:?}: {data_symbol_id:?} in emit module"
                )
            },
        )
    }

    fn get_relocated_function_index(&self, relocation: &RelocationEntry) -> Result<usize> {
        let Some(input_func_id) = FunctionIndexTag::get_input_function_id(
            self.input_module,
            Id::from_index(relocation.index),
        ) else {
            bail!("Relocation {relocation:?} does not refer to a valid function")
        };
        let Some(output_func_id) = self.emit_module.functions.get_output_id(input_func_id) else {
            bail!("Cannot find output function for input function {input_func_id} referenced by relocation {relocation:?}")
        };
        Ok(output_func_id.as_raw_index())
    }

    fn get_relocated_function_table_index(&self, relocation: &RelocationEntry) -> Result<usize> {
        let result = self.get_entry_symbol_op::<FunctionIndexTag>(relocation)?;
        Ok(*result
            .as_static()
            .unwrap_or_else(||panic!("Relocation should only process static symbols, got {result:?}, for entry {relocation:?}")))
    }

    fn get_relocated_memory_offset(&self, relocation: &RelocationEntry) -> Result<usize> {
        let result = self.get_entry_symbol_op::<DataSymbolTag>(relocation)?;
        let mut offset = *result
            .as_static()
            .unwrap_or_else(||panic!("Relocation should only process static symbols, got {result:?}, for entry {relocation:?}"));
        if relocation.addend < 0 {
            log::warn!("Relocation {relocation:?} has negative addend");
        }
        offset += relocation.addend;

        Ok(offset as usize)
    }

    fn get_global_id(&self, relocation: &RelocationEntry) -> Result<usize> {
        let symbol = self
            .input_module
            .symbols
            .get(Id::from_index(relocation.index))
            .ok_or_else(|| {
                anyhow!(
                    "Relocation {relocation:?} refers to invalid symbol id {}",
                    relocation.index
                )
            })?;
        let SymbolKind::Global(original_global_id) = symbol.kind else {
            bail!("Relocation {relocation:?} does not refer to a global symbol, instead got {symbol:?}");
        };

        let global_id = (self.global_id_mapper)(original_global_id)
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
