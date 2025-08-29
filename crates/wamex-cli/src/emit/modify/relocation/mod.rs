pub mod encode;

use anyhow::{anyhow, bail, Result};
use wasmparser::RelocationEntry;

use crate::{
    emit::ModuleEmitState,
    index::{DataSegmentId, DataSymbolId, GlobalId, InputFuncId, OutputGlobalId},
    read::{linking::SymbolIndex, InputModule},
};

#[derive(Clone)]
pub struct RelocateState<'any, 'src, F> {
    pub input_module: &'any InputModule<'src>,
    pub main_module: &'any ModuleEmitState<'any, 'src>,
    pub global_id_mapper: F,
    pub emit_module: &'any ModuleEmitState<'any, 'src>,
}

impl<F> RelocateState<'_, '_, F>
where
    F: Fn(GlobalId) -> Option<OutputGlobalId>,
{
    fn _get_relocation_input_function_index(
        &self,
        relocation: &RelocationEntry,
    ) -> Result<InputFuncId> {
        let Some(SymbolIndex::Func(input_func_id)) = self
            .input_module
            .linking
            .linking_symbols
            .original_indexes
            .get(relocation.index as usize)
        else {
            bail!("Relocation {relocation:?} does not refer to a valid function");
        };
        Ok(*input_func_id)
    }

    fn get_relocated_function_index(&self, relocation: &RelocationEntry) -> Result<usize> {
        let input_func_id = self._get_relocation_input_function_index(relocation)?;
        let Some(output_func_id) = self.emit_module.functions.get_output_id(input_func_id) else {
            bail!(
                "Dependency analysis error: \
                 No output function for input function {input_func_id} \
                 referenced by relocation {relocation:?}"
            );
        };
        Ok(output_func_id.as_raw_index() as usize)
    }

    fn get_relocated_function_table_index(&self, relocation: &RelocationEntry) -> Result<usize> {
        let input_func_id = self._get_relocation_input_function_index(relocation)?;

        let Some(&table_index) = self
            .emit_module
            .indirect_functions
            .function_table_index
            .get(&input_func_id)
        else {
            bail!(
                "Dependency analysis error: \
                     No indirect function table index \
                     for input function {input_func_id} \
                     referenced by relocation {relocation:?}"
            )
        };
        Ok(table_index)
    }

    fn _get_relocation_memory_symbol(
        &self,
        relocation: &RelocationEntry,
    ) -> Result<(DataSegmentId, DataSymbolId)> {
        let Some(SymbolIndex::DataDefined(segment_id, data_index)) = self
            .input_module
            .linking
            .linking_symbols
            .original_indexes
            .get(relocation.index as usize)
        else {
            bail!("Relocation {relocation:?} does not refer to a valid memory");
        };
        Ok((*segment_id, *data_index))
    }

    fn get_relocated_memory_offset(&self, relocation: &RelocationEntry) -> Result<usize> {
        let (segment_id, data_index) = self._get_relocation_memory_symbol(relocation)?;
        let (segment_id, data_index): &(DataSegmentId, usize) = self
            .main_module
            .input_data_to_output_id
            .get(&(segment_id, data_index))
            .ok_or_else(|| {
                anyhow!(
                    "Dependency analysis error: No output data segment for input segment {segment_id} and data {data_index} referenced by relocation {relocation:?}"
                )
            })?;
        let Some(segment) = self.main_module.data.get(*segment_id) else {
            bail!("No data segment with id {segment_id} for relocation {relocation:?}");
        };
        let Some(data) = segment.globals().get(*data_index) else {
            bail!("No data with index {data_index} in segment {segment_id} for relocation {relocation:?}");
        };
        if !segment.is_active() {
            bail!("Relocation {relocation:?} refers to passive data segment {segment_id}");
        }

        Ok(segment.memory_offset() + data.data_offset + relocation.addend as usize)
    }

    fn get_global_id(&self, relocation: &RelocationEntry) -> Result<OutputGlobalId> {
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
        Ok(global_id)
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
