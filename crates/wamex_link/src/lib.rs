use std::{cell::Cell, collections::BTreeMap};

use js_sys::{
    Function, Object, Reflect,
    WebAssembly::{self, Module},
};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::Request;

#[derive(Default, Debug)]
pub struct Symbol {
    sym_type: u32,
    sym_name: String,
}
#[derive(Default, Debug)]
pub struct ModuleDecl {
    deps: Vec<Symbol>,
    provides: Vec<Symbol>,
}
#[derive(Debug)]
struct LinkageState {
    module_exports: BTreeMap<() /*ModuleId*/, ModuleDecl>,
    main_module: MainModuleStruct,
}

impl LinkageState {
    fn new() -> Self {
        Self {
            module_exports: Default::default(),
            main_module: MainModuleStruct::new(),
        }
    }
}

#[derive(Debug)]
struct MainModuleStruct {
    // Shared memory
    memory: JsValue,
    // Where to store lazy fns.
    indirect_function_table: JsValue,
}
impl MainModuleStruct {
    fn new() -> Self {
        Self {
            memory: wasm_bindgen::memory(),
            indirect_function_table: wasm_bindgen::function_table(),
        }
    }
}

thread_local! {
    pub static LINKAGE_STATE: Cell<Option<LinkageState>> = Cell::new(Some(LinkageState::new()));
}

#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn link_my_module(module_name: &str, module: ModuleDecl) {
    LINKAGE_STATE.with(|state_cell| {
        let mut state = state_cell.take().expect("Linkage state already taken");
        state.module_exports.insert(module_name.to_string(), module);
        state_cell.set(Some(state));
    });
}

// Load webassembly module from URL and instantiate it, link with active imports.
pub async fn load(module: ModuleId, state: &mut LinkageState, reload: bool) -> Result<(), JsValue> {
    let request = Request::new_with_str(module.url()).expect("Cannot create request");

    let window = web_sys::window().unwrap();
    let resp_value = window.fetch_with_request(&request);
    // 1. Assert module decl "deps" are satisfied.
    // 2. check if module already loaded, if reload - replace it.

    // 3. fetch module + ModuleDecl
    let imports = Object::new();
    Reflect::set(&imports, &JsValue::from_str("env"), &Object::new())?;

    // 4. Create imports object (we can reuse global one?)

    // 5. Instantiate module.
    let sub_module = JsFuture::from(WebAssembly::instantiate_streaming(
        &resp_value,
        &Object::new(),
    ))
    .await?;

    // 6. Store exports in linkage state.
    todo!()
}

struct ModuleId {
    name: String,
    version: String,
}
impl ModuleId {
    fn url(&self) -> &str {
        todo!();
    }
    fn module_decl(&self) -> &ModuleDecl {
        todo!();
    }
}

async unsafe fn unload(module: ModuleId) -> Result<(), JsValue> {
    todo!()
}
