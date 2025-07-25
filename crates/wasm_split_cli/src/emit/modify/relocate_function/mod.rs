pub mod encode;
use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};
use wasmparser::RelocationEntry;

use crate::{
    emit::EmitInfo,
    index::InputFuncId,
    read::{linking::SymbolType, InputModule},
};

pub struct RelocateFunctionInfo<'a> {
    pub input_module: &'a InputModule<'a>,
    pub emit_info: &'a EmitInfo,
    pub input_function_output_id: &'a HashMap<InputFuncId, usize>,
}

impl RelocateFunctionInfo<'_> {
    fn get_relocation_input_function_index(&self, relocation: &RelocationEntry) -> Result<usize> {
        let Some((input_func_id, SymbolType::Func)) = self
            .input_module
            .linking
            .linking_symbols
            .original_indexes
            .get(relocation.index as usize)
        else {
            bail!("Relocation {relocation:?} does not refer to a valid function");
        };
        Ok(*input_func_id as usize)
    }

    fn get_relocated_function_index(&self, relocation: &RelocationEntry) -> Result<usize> {
        let input_func_id = self.get_relocation_input_function_index(relocation)?;
        let Some(&output_func_id) = self.input_function_output_id.get(&input_func_id) else {
            bail!(
                "Dependency analysis error: \
                 No output function for input function {input_func_id} \
                 referenced by relocation {relocation:?}"
            );
        };
        Ok(output_func_id)
    }

    fn get_relocated_function_table_index(&self, relocation: &RelocationEntry) -> Result<usize> {
        let input_func_id = self.get_relocation_input_function_index(relocation)?;

        let Some(&table_index) = self
            .emit_info
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

    pub fn apply_relocation(
        &self,
        data: &mut [u8],
        data_offset: usize,
        relocation: &RelocationEntry,
    ) -> Result<()> {
        let relocation_range = relocation.relocation_range();
        let target =
            &mut data[(relocation_range.start - data_offset)..(relocation_range.end - data_offset)];
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
            TableIndexI32 => {
                encode_u32(
                    self.get_relocated_function_table_index(relocation)? as u32,
                    target.try_into().unwrap(),
                );
            }
            TableIndexSleb64 => {
                encode_leb128_i64_10byte(
                    self.get_relocated_function_table_index(relocation)? as i64,
                    target.try_into().unwrap(),
                );
            }
            TableIndexI64 => {
                encode_u64(
                    self.get_relocated_function_table_index(relocation)? as u64,
                    target.try_into().unwrap(),
                );
            }
            FunctionIndexI32 => {
                encode_u32(
                    self.get_relocated_function_index(relocation)? as u32,
                    target.try_into().unwrap(),
                );
            }
            FunctionOffsetI32 | SectionOffsetI32 | TableIndexRelSleb | FunctionOffsetI64
            | TableIndexRelSleb64 => {
                bail!("Unsupported relocation type {relocation:?}");
            }
            _ => {}
        }

        Ok(())
    }
}
