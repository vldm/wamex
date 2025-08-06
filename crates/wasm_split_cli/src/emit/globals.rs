use wasm_encoder::GlobalType;

use crate::index::DataSymbolId;

// Init global variable that will replace all usage of DataSymbol.
#[derive(Clone, Debug)]
pub struct DataSymbol {
    pub data_offset: i32,
    pub symbol_index: DataSymbolId,
    pub type_info: wasm_encoder::GlobalType,
}

#[derive(Clone, Debug)]
pub enum GlobalConstructor {
    DataSymbol(DataSymbol),
    TempStore(wasm_encoder::GlobalType),
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
            .with_i32_const(symbol.data_offset)
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
