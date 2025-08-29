use core::error;
use std::{
    cell::{Cell, Ref},
    collections::BTreeMap,
};

use js_sys::{
    Array, Function, Object, Reflect,
    WebAssembly::{self},
};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, Response};

#[derive(Debug)]
pub struct InstantiatedModule {
    instantiated: JsValue,
    metadata: wamex_metadata::Module,
}
#[derive(Debug)]
struct LinkageState {
    module_exports: BTreeMap<ModuleId, InstantiatedModule>,
    main_module: MainModuleStruct,
    global_imports: Object,
}

impl LinkageState {
    fn new() -> Self {
        Self {
            module_exports: Default::default(),
            main_module: MainModuleStruct::new(),
            global_imports: Object::new(),
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Module {} is not loaded", .module_id.module_name())]
    ModuleNotLoaded { module_id: ModuleId },
    #[error("Failed to fetch url: {url}, result:{error:?}")]
    FetchError { url: String, error: JsValue },

    #[error("Error with js communication {context}: {error:?}")]
    JsError {
        context: &'static str,
        error: JsValue,
    },

    #[error("Failed to deserialize json: {0}")]
    DeserializationError(#[from] serde_json::Error),
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

// #[unsafe(no_mangle)]
// #[inline(never)]
// pub extern "C" fn link_my_module(module_name: &str, module: wamex_metadata::Module) {
//     LINKAGE_STATE.with(|state_cell| {
//         let mut state = state_cell.take().expect("Linkage state already taken");
//         state.module_exports.insert(module_name, module);
//         state_cell.set(Some(state));
//     });
// }

async fn fetch_buffer(url: String) -> Result<JsValue, Error> {
    let request = Request::new_with_str(&url).map_err(|error| Error::JsError {
        context: "Failed to create request",
        error,
    })?;
    let window = web_sys::window().unwrap();
    let future = JsFuture::from(window.fetch_with_request(&request));
    let response = future.await.map_err(|e| Error::FetchError {
        url: url,
        error: e.into(),
    })?;

    let response = response
        .dyn_ref::<Response>()
        .expect("Failed to cast fetch result to Response");
    let buffer = response.array_buffer().map_err(|error| Error::JsError {
        context: "Failed to get array buffer from response",
        error,
    })?;
    let buffer = JsFuture::from(buffer)
        .await
        .map_err(|error| Error::JsError {
            context: "Failed to resolve array buffer promise",
            error,
        })?;

    Ok(buffer)
}

fn buffer_to_rust(buffer: JsValue) -> Result<Vec<u8>, Error> {
    let uint8_array = js_sys::Uint8Array::new(&buffer);
    let mut vec = vec![0; uint8_array.length() as usize];
    uint8_array.copy_to(&mut vec[..]);
    Ok(vec)
}

fn extend_object(target: &Object, source: &Object) -> Result<(), JsValue> {
    let entries = Object::entries(source);
    for entry in entries.iter() {
        let pair = Array::from(&entry);
        let key = pair.get(0);
        let value = pair.get(1);

        #[cfg(debug_assertions)]
        {
            let key_str = key.as_string().unwrap_or_default();
            let existing = Reflect::get(target, &key)?;
            if !existing.is_undefined() {
                web_sys::console::warn_1(
                    &format!("Overriding existing import: {}", key_str).into(),
                );
            }
        }

        Reflect::set(target, &key, &value)?;
    }
    Ok(())
}

// Load webassembly module from URL and instantiate it, link with active imports.
pub async fn load(module_id: ModuleId, reload: bool) -> Result<(), Error> {
    // 1. fetch module
    let buffer = fetch_buffer(module_id.module_url()).await?;
    let module = WebAssembly::Module::new(&buffer).map_err(|error| Error::JsError {
        context: "Failed to create module",
        error,
    })?;
    // 1.1. fetch ModuleDecl
    // TODO:
    // let array = WebAssembly::Module::custom_sections(&module, "__wamex_metadata");
    let decl_buffer = fetch_buffer(module_id.module_decl_url()).await?;
    let decl_buffer = buffer_to_rust(decl_buffer)?;

    let module_decl: wamex_metadata::Module = serde_json::from_slice(&decl_buffer)?;
    assert_eq!(module_decl.version, module_id.version);

    // 2. Assert module decl "deps" are satisfied.
    // 3. check if module already loaded, if reload - replace it.

    // 4. Create imports object (we can reuse global one?)
    // TODO: Limit only for needed imports?
    let global_imports = LINKAGE_STATE.with(|state_cell| {
        let state = state_cell.take().expect("Linkage state already taken");
        let imports = state.global_imports.clone();
        state_cell.set(Some(state));
        imports
    });
    let imports = global_imports;
    // Reflect::set(&imports, &JsValue::from_str("env"), &Object::new())?;
    // 5. Instantiate module.
    let sub_module = JsFuture::from(WebAssembly::instantiate_module(&module, &imports))
        .await
        .map_err(|error| Error::JsError {
            context: "Failed to instantiate module",
            error,
        })?;
    let sub_module_instance: WebAssembly::Instance =
        Reflect::get(&sub_module, &JsValue::from_str("instance"))
            .map_err(|error| Error::JsError {
                context: "Failed to get instance from instantiated module",
                error,
            })?
            .into();
    let defined_exports = sub_module_instance.exports();

    // 6. Store exports and module info in linkage state.
    LINKAGE_STATE
        .with(|state_cell| {
            let state = state_cell.take().expect("Linkage state already taken");

            // Extend global imports with module exports.
            let result = extend_object(&state.global_imports, &defined_exports);
            state_cell.set(Some(state));
            result
        })
        .map_err(|e| Error::JsError {
            context: "Failed to extend global imports",
            error: e,
        })?;
    Ok(())
}

#[derive(Debug, PartialEq, PartialOrd, Eq, Ord, Hash, Clone)]
pub struct ModuleId {
    name: String,
    version: wamex_metadata::BumpVersion,
    module_url_path: Option<String>,
}

impl ModuleId {
    pub fn new(name: &str) -> Self {
        ModuleId {
            name: name.to_string(),
            version: wamex_metadata::BumpVersion::new(),
            module_url_path: None,
        }
    }
    pub fn new_with_url(name: &str, module_url_path: &str) -> Self {
        ModuleId {
            name: name.to_string(),
            version: wamex_metadata::BumpVersion::new(),
            module_url_path: Some(module_url_path.to_string()),
        }
    }
    pub fn module_name(&self) -> &str {
        &self.name
    }
    pub fn module_url(&self) -> String {
        if let Some(url) = &self.module_url_path {
            format!("{}/{}.wasm", url, self.name)
        } else {
            format!("{}.wasm", self.name)
        }
    }
    pub fn module_decl_url(&self) -> String {
        if let Some(url) = &self.module_url_path {
            format!("{}/{}.decl.json", url, self.name)
        } else {
            format!("{}.decl.json", self.name)
        }
    }
}

async unsafe fn unload(module: ModuleId) -> Result<(), JsValue> {
    todo!()
}
