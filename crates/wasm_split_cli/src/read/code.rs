use anyhow::{bail, ensure, Result};

pub use std::ops::Range;
pub use wasmparser::FunctionBody;

use super::Ind;
use crate::index::{FuncTypeId, InputFuncId};

#[derive(Debug)]
pub struct Function<'a> {
    pub type_id: FuncTypeId,
    pub body: FunctionBody<'a>,
}

#[derive(Debug, Default)]
pub struct CodeSection<'a> {
    pub start_func: Option<InputFuncId>,
    // function (CodeSectionEntry)
    pub defined_funcs: Vec<Function<'a>>,

    pub(super) func_types: Vec<FuncTypeId>,
}
impl<'a> CodeSection<'a> {
    pub fn new(
        start: Option<InputFuncId>,
        funcs: Vec<FunctionBody<'a>>,
        func_types: Vec<FuncTypeId>,
        code_header: Option<(usize, usize, u32)>,
    ) -> Result<Ind<Self>> {
        let Some((code_start, section_index, count)) = code_header else {
            bail!("No code section start");
        };
        ensure!(
            count as usize == funcs.len(),
            "Function count mismatch: {} != {}",
            count,
            funcs.len()
        );
        ensure!(
            count as usize == func_types.len(),
            "Function types count mismatch: {} != {}",
            count,
            func_types.len()
        );
        Ok(Ind {
            starting_offset: code_start,
            section_index,
            section_payload: CodeSection {
                start_func: start,
                defined_funcs: funcs
                    .into_iter()
                    .zip(&func_types)
                    .map(|(body, ty)| Function { type_id: *ty, body })
                    .collect(),
                func_types,
            },
        })
    }
}
