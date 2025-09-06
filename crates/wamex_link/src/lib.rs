use core::error;
use std::{
    cell::{Cell, Ref},
    collections::BTreeMap,
};

use js_sys::{
    Array, Function, Object, Reflect,
    WebAssembly::{self},
};
use log::debug;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, Response};

#[derive(Debug)]
pub struct InstantiatedModule {
    instantiated: WebAssembly::Instance,
    metadata: wamex_metadata::Module,
}
#[derive(Debug)]
struct LinkageState {
    module_exports: BTreeMap<ModuleId, InstantiatedModule>,

    global_imports: Object,
}

impl LinkageState {
    fn new() -> Self {
        Self {
            module_exports: Default::default(),
            global_imports: Self::_get_exports(wasm_bindgen::function_table()),
        }
    }

    pub fn save_loaded_module(
        &mut self,
        module_id: ModuleId,
        module: InstantiatedModule,
    ) -> Result<(), JsValue> {
        debug!("Get exports from instantiated module");
        let defined_exports = module.instantiated.exports();
        debug!("Module exports: {:?}", defined_exports);
        debug!("Store exports and module info in linkage state");

        self.module_exports.insert(module_id.clone(), module);

        // Extend global imports with module exports.
        copy_fields(&self.global_imports, &defined_exports)
    }

    fn _get_exports(indirect_function_table: JsValue) -> Object {
        let new_object = Object::new();
        let obj = wasm_bindgen::exports().try_into().unwrap();
        copy_fields(&new_object, &obj).unwrap();
        let set = Reflect::set(
            &new_object,
            &JsValue::from_str("__indirect_function_table"),
            &indirect_function_table,
        )
        .unwrap();
        assert!(set);
        new_object
    }

    fn modify_global<F, U>(op: F) -> U
    where
        F: FnOnce(&mut LinkageState) -> U,
    {
        LINKAGE_STATE.with(|state_cell| {
            let mut state = state_cell.take().unwrap();
            let res = op(&mut state);
            state_cell.set(Some(state));
            res
        })
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
    DeserializationError(#[from] Box<dyn error::Error + Send + Sync>),
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

fn copy_fields(target: &Object, source: &Object) -> Result<(), JsValue> {
    let entries = Object::entries(source);
    for entry in entries.iter() {
        let pair = Array::from(&entry);
        let key = pair.get(0);
        let value = pair.get(1);

        if cfg!(debug_assertions) {
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

pub fn deserialize_decl(buffer: &[u8]) -> Result<wamex_metadata::Module, Error> {
    // {
    //     let module: wamex_metadata::Module =
    //         bitcode::decode(buffer).map_err(|e| Error::DeserializationError(e.into()))?;
    //     Ok(module)
    // }
    Ok(Default::default())
}

fn _set_table_base(exports: &Object) -> Result<Function, Error> {
    let values: Array = Object::values(exports);
    let func = values
        .iter()
        .find_map(|v| v.dyn_into::<Function>().ok())
        .ok_or(Error::JsError {
            context: "No function found in exports",
            error: JsValue::from("No function found"),
        })?;
    Ok(func)
}

// Load webassembly module from URL and instantiate it, link with active imports.
pub async fn load(module_id: ModuleId, reload: bool) -> Result<(), Error> {
    debug!("call load for module: {:?}", module_id);
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

    let metadata = deserialize_decl(&decl_buffer)?;
    assert_eq!(metadata.version, module_id.version);

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

    let new_exports = Object::new();
    copy_fields(&new_exports, &global_imports).map_err(|e| Error::JsError {
        context: "Failed to copy global imports",
        error: e,
    })?;
    debug!("New exports: {:?}", new_exports);

    // TODO: Set size from metadata.
    let alloc = Vec::<u8>::with_capacity(1024);
    let start = alloc.leak();

    let setted = Reflect::set(
        &new_exports,
        &JsValue::from_str("__lib_base"),
        &JsValue::from(start.as_ptr() as usize),
    )
    .map_err(|error| Error::JsError {
        context: "Failed to set __lib_base import",
        error,
    })?;
    if !setted {
        return Err(Error::JsError {
            context: "Failed to set __lib_base import",
            error: JsValue::from("Reflect::set returned false"),
        });
    }

    //TODO: Calculate table base
    let setted = Reflect::set(
        &new_exports,
        &JsValue::from_str("__table_base"),
        &JsValue::from(0 as usize),
    )
    .map_err(|error| Error::JsError {
        context: "Failed to set __table_base import",
        error,
    })?;
    if !setted {
        return Err(Error::JsError {
            context: "Failed to set __table_base import",
            error: JsValue::from("Reflect::set returned false"),
        });
    }

    let imports = Object::new();

    Reflect::set(&imports, &JsValue::from_str("__wasm_split"), &new_exports).map_err(|error| {
        Error::JsError {
            context: "Failed to set __wasm_split imports",
            error,
        }
    })?;

    debug!("Instantiate module, imports: {:?}", imports);
    // 5. Instantiate module.
    let sub_module_instance: WebAssembly::Instance =
        JsFuture::from(WebAssembly::instantiate_module(&module, &imports))
            .await
            .map_err(|error| Error::JsError {
                context: "Failed to instantiate module",
                error,
            })?
            .into();

    let instantiated = InstantiatedModule {
        instantiated: sub_module_instance.clone(),
        metadata,
    };

    // 6. Store exports and module info in linkage state.
    LinkageState::modify_global(|state| state.save_loaded_module(module_id, instantiated))
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
            format!("{}/{}.decl", url, self.name)
        } else {
            format!("{}.decl", self.name)
        }
    }
}

async unsafe fn unload(module: ModuleId) -> Result<(), JsValue> {
    todo!()
}
