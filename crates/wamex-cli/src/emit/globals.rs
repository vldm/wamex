use std::borrow::Cow;

use wasm_encoder::GlobalType;

use crate::{
    emit::ImportedEntity,
    index::{DataSymbolId, InputGlobalId},
};

// Init global variable that will replace all usage of DataSymbol.
#[derive(Clone, Debug)]
pub struct DataSymbol {
    pub data_offset: usize,
    pub symbol_index: DataSymbolId,
    pub type_info: wasm_encoder::GlobalType,
}

#[derive(Clone, Debug)]
pub enum GlobalConstructor {
    DataSymbol(DataSymbol),
    TempStore(wasm_encoder::GlobalType),
}

#[derive(Debug)]
pub enum DefinedGlobal<'a> {
    PlainCopy {
        input_global_id: InputGlobalId,
        global: wasmparser::Global<'a>,
    },
    WithConstructor(GlobalConstructor),
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

impl GlobalConstructor {
    pub const POINTER_TYPE: wasm_encoder::GlobalType = wasm_encoder::GlobalType {
        val_type: wasm_encoder::ValType::I32,
        mutable: false,
        shared: false,
    };
    // uses https://github.com/WebAssembly/extended-const proposal (not yet merged, but accepted)
    // so it is not supported by all runtimes/toolings but webkit/chromium/firefox support it.
    pub fn global_init(&self, lib_base_id: u32) -> wasm_encoder::ConstExpr {
        let symbol = match self {
            GlobalConstructor::TempStore(global_type) => {
                return Self::global_init_tmp(global_type.val_type);
            }
            GlobalConstructor::DataSymbol(symbol) => symbol,
        };
        wasm_encoder::ConstExpr::global_get(lib_base_id)
            .with_i32_const(symbol.data_offset.try_into().unwrap())
            .with_i32_add()
    }

    fn global_init_tmp(val_type: wasm_encoder::ValType) -> wasm_encoder::ConstExpr {
        match val_type {
            wasm_encoder::ValType::I32 => wasm_encoder::ConstExpr::i32_const(0),
            wasm_encoder::ValType::I64 => wasm_encoder::ConstExpr::i64_const(0),
            wasm_encoder::ValType::F32 => wasm_encoder::ConstExpr::f32_const(0.0.into()),
            wasm_encoder::ValType::F64 => wasm_encoder::ConstExpr::f64_const(0.0.into()),
            _ => panic!("Unsupported global type for tmp init"),
        }
    }
    pub fn global_type(&self) -> GlobalType {
        match self {
            GlobalConstructor::DataSymbol(symbol) => symbol.type_info.clone(),
            GlobalConstructor::TempStore(global_type) => global_type.clone(),
        }
    }
}
