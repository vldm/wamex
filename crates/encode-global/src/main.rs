use std::{fs::File, io::Write};

use wasm_encoder::{ConstExpr, EntityType, GlobalSection, GlobalType, ImportSection, Module};

fn main() {
    let mut module = Module::new();
    let mut imports = ImportSection::new();
    imports.import(
        "env",
        "__lib_base",
        EntityType::Global(GlobalType {
            val_type: wasm_encoder::ValType::I32,
            mutable: false,
            shared: false,
        }),
    );
    let mut globals = GlobalSection::new();
    globals.global(
        GlobalType {
            val_type: wasm_encoder::ValType::I32,
            mutable: false,
            shared: false,
        },
        &ConstExpr::global_get(0).with_i32_const(12).with_i32_add(),
    );
    module.section(&imports);
    module.section(&globals);
    let data = module.finish();
    File::create("out_wasm.wasm")
        .unwrap()
        .write_all(&data)
        .unwrap()
}
