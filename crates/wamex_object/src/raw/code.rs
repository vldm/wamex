use anyhow::{Result, bail, ensure};
use wasmparser::FuncType;
pub use wasmparser::FunctionBody;

use super::{Ind, indexes::FuncTypeId};
use crate::{index::IdVec, typed::FunctionRef};

#[derive(Debug)]
pub enum InputFunction<'a> {
    Import {},
    Defined(FunctionBody<'a>),
}
#[derive(Debug, Clone)]
pub struct FunctionWithBody<'a> {
    pub func_type: FuncType,
    pub body: FunctionBody<'a>,
}

#[derive(Debug, Default)]
pub struct CodeSection<'a> {
    pub start_func: Option<FunctionRef>,
    // function (CodeSectionEntry)
    pub defined_funcs: IdVec<FunctionWithBody<'a>>,
}
impl<'a> CodeSection<'a> {
    pub fn new(
        start: Option<FunctionRef>,
        funcs: Vec<FunctionBody<'a>>,
        func_type_ids: Vec<FuncTypeId>,
        func_types: &IdVec<FuncType>,
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
            count as usize == func_type_ids.len(),
            "Function type ids count mismatch: {} != {}",
            count,
            func_type_ids.len()
        );
        Ok(Ind {
            starting_offset: code_start,
            section_index,
            section_payload: CodeSection {
                start_func: start,
                defined_funcs: funcs
                    .into_iter()
                    .zip(&func_type_ids)
                    .map(|(body, type_id)| FunctionWithBody {
                        func_type: func_types[*type_id].clone(),
                        body,
                    })
                    .collect(),
            },
        })
    }
}
