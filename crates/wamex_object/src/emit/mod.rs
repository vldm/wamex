use std::collections::HashMap;

use anyhow::Result;
use cranelift_entity::{EntityRef, PrimaryMap};

use crate::{
    raw::FuncTypeId,
    typed::{FunctionRef, Module},
};

pub mod modify;
pub mod relocation;

#[derive(Default, Debug, Clone, Copy)]
pub struct ModuleConfig {
    // Is this module is emitting as position-independent code
    pub dyn_base: bool,
}

impl<'src> Module<'src> {
    pub fn generate(&self, output_module: &mut wasm_encoder::Module) -> Result<()> {
        let fn_type_map = self.generate_type_section(output_module)?;
        todo!()
    }
    /// Generate type section, return map from FunctionId to type index.
    pub fn generate_type_section(
        &self,
        output_module: &mut wasm_encoder::Module,
    ) -> Result<PrimaryMap<FunctionRef, FuncTypeId>> {
        let mut function_types = PrimaryMap::new();
        let mut uniq_types = HashMap::<&wasmparser::FuncType, FuncTypeId>::new();

        // Collect unique types
        for (id, func) in self.functions.iter() {
            let func_type = func.get_type();
            let new_func_type_id = FuncTypeId::new(uniq_types.len());

            let func_type_id = uniq_types.entry(func_type).or_insert(new_func_type_id);
            function_types[id] = *func_type_id;
        }

        // build section based on collected types
        let mut section = wasm_encoder::TypeSection::new();

        let mut uniq_types: Vec<(_, _)> = uniq_types
            .into_iter()
            .map(|(k, v)| (k.clone(), v))
            .collect();
        uniq_types.sort_by_key(|v| v.1);

        for (func_type, _id) in uniq_types {
            let output_func_type: wasm_encoder::FuncType = func_type.clone().try_into().unwrap();
            section.ty().func_type(&output_func_type);
        }

        output_module.section(&section);
        // return map used to generate code section
        Ok(function_types)
    }
}
