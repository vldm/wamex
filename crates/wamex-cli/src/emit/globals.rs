use std::borrow::Cow;

use crate::{emit::ImportedEntity, index::InputGlobalId};

#[derive(Debug)]
pub enum DefinedGlobal<'a> {
    PlainCopy {
        input_global_id: InputGlobalId,
        global: wasmparser::Global<'a>,
    },
    WithConstructor(wasm_encoder::GlobalType),
}

#[derive(Debug)]
pub enum GlobalImport<'a> {
    Existing {
        global_name: &'a str,
        module_name: &'a str,
        input_global_id: InputGlobalId,
        global_type: wasm_encoder::GlobalType,
    },
    New {
        global_name: Cow<'a, str>,
        input_global_id: Option<InputGlobalId>,
        global_type: wasm_encoder::GlobalType,
    },
}

impl ImportedEntity for GlobalImport<'_> {
    fn module_name(&self) -> Cow<'_, str> {
        match self {
            GlobalImport::Existing { module_name, .. } => (*module_name).into(),
            GlobalImport::New { .. } => "__wasm_split".into(),
        }
    }
    fn import_name(&self) -> Cow<'_, str> {
        match self {
            GlobalImport::Existing { global_name, .. } => (*global_name).into(),
            GlobalImport::New { global_name, .. } => global_name.clone(),
        }
    }
}

impl GlobalImport<'_> {
    pub fn global_type(&self) -> &wasm_encoder::GlobalType {
        match self {
            GlobalImport::Existing { global_type, .. } => global_type,
            GlobalImport::New { global_type, .. } => global_type,
        }
    }
}

pub fn global_init_tmp(val_type: wasm_encoder::ValType) -> wasm_encoder::ConstExpr {
    match val_type {
        wasm_encoder::ValType::I32 => wasm_encoder::ConstExpr::i32_const(0),
        wasm_encoder::ValType::I64 => wasm_encoder::ConstExpr::i64_const(0),
        wasm_encoder::ValType::F32 => wasm_encoder::ConstExpr::f32_const(0.0.into()),
        wasm_encoder::ValType::F64 => wasm_encoder::ConstExpr::f64_const(0.0.into()),
        _ => panic!("Unsupported global type for tmp init"),
    }
}
