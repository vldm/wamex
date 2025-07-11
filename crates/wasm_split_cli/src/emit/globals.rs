use wasm_encoder::GlobalType;

// Init global variable that will replace all usage of DataSymbol.
pub struct GlobalConstructor {
    pub data_offset: i32,
    pub type_info: wasm_encoder::GlobalType,
}

impl GlobalConstructor {
    pub const POINTER_TYPE: wasm_encoder::GlobalType = wasm_encoder::GlobalType {
        val_type: wasm_encoder::ValType::I32,
        mutable: false,
        shared: false,
    };
    pub fn global_init(&self, lib_base_id: u32) -> wasm_encoder::ConstExpr {
        wasm_encoder::ConstExpr::global_get(lib_base_id)
            .with_i32_const(self.data_offset)
            .with_i32_add()
    }
    pub fn global_type(&self) -> GlobalType {
        self.type_info.clone()
    }
}
