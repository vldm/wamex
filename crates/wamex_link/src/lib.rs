extern crate alloc;
use alloc::collections::BTreeMap;
use core::error;
use std::{cell::RefCell, fmt::Display};

use js_sys::{
    Object, Reflect,
    WebAssembly::{self},
};
use wamex_metadata::BumpVersion;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, Response};

use crate::{js_helpers::copy_imports, module_alloc::RawAllocEntry};

mod deserialize;
mod js_helpers;
mod logs;
mod module_alloc;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Module {} not found", .module_id.module_name())]
    ModuleNotFound { module_id: ModuleId },

    #[error(
        "Cannot resolve dependency {dependency:?}:{entry} needed for module {module_id:?}\n => {sub_error}", entry = .entry.as_deref().unwrap_or("<none>"), 
        sub_error = .sub_error.as_ref().map(|e| e.to_string()).unwrap_or("<none>".to_string())
    )]
    CannotResolveDependency {
        module_id: ModuleId,
        dependency: ModuleId,
        entry: Option<String>,
        sub_error: Option<Box<dyn error::Error>>,
    },
    #[error("Failed to fetch url: {url}, result:{error:?}")]
    FetchError { url: String, error: JsValue },

    #[error("Error with js communication {context}: {error:?}")]
    JsError {
        context: &'static str,
        error: JsValue,
    },

    #[error("Failed to deserialize: {0}")]
    DeserializationError(#[from] Box<dyn error::Error + Send + Sync>),
}

#[derive(Debug, PartialEq, PartialOrd, Eq, Ord, Hash, Clone)]
pub struct ModuleId {
    name: String,
    version: Option<BumpVersion>,
    module_url_path: Option<String>,
}

impl ModuleId {
    pub fn new(name: &str) -> Self {
        ModuleId {
            name: name.to_string(),
            version: None,
            module_url_path: None,
        }
    }
    pub fn new_with_url(name: &str, module_url_path: &str) -> Self {
        ModuleId {
            name: name.to_string(),
            version: None,
            module_url_path: Some(module_url_path.to_string()),
        }
    }
    pub fn module_name(&self) -> &str {
        &self.name
    }

    pub fn build_url(&self) -> String {
        let url = if let Some(url) = &self.module_url_path {
            format!("{}/", url)
        } else {
            String::new()
        };
        let version = if let Some(version) = &self.version {
            format!("-{}", version)
        } else {
            String::new()
        };

        let name = &self.name;
        format!("{}/{}{}.wasm", url, name, version)
    }
}
#[derive(Debug)]
struct InstantiatedModule {
    version: wamex_metadata::BumpVersion,
    instantiated: WebAssembly::Instance,
    alloc_guard: GuardedAllocEntry,
    needs_update: bool,
}

impl InstantiatedModule {
    pub fn pending_updates(&self) -> bool {
        self.needs_update
    }
    pub fn mark_for_update(&mut self) {
        self.needs_update = true;
    }

    fn free(mut self, linkage_state: &mut LinkageState) {
        // Free allocated memory and table space.
        let entry = self.alloc_guard.take().unwrap();
        linkage_state.alloc_state.free(entry);
    }
}

#[derive(Debug)]
struct GuardedAllocEntry {
    entry: Option<RawAllocEntry>,
}

impl GuardedAllocEntry {
    pub fn new(entry: RawAllocEntry) -> Self {
        Self { entry: Some(entry) }
    }

    pub fn take(&mut self) -> Option<RawAllocEntry> {
        self.entry.take()
    }
}

// Dropping this guard inside LinkageState::global will panic, because it will try to borrow LINKAGE_STATE again.
impl Drop for GuardedAllocEntry {
    fn drop(&mut self) {
        if let Some(entry) = self.entry.take() {
            LinkageState::global(|state| {
                state.alloc_state.free(entry);
            });
        }
    }
}

//TODO: Remove unwraps allow to reduce build size.

#[derive(Debug)]
struct LinkageState {
    // Handle to all loaded modules
    loaded_modules: BTreeMap<ModuleId, InstantiatedModule>,

    // If module was reloaded, we keep old modules until `unload` is called.
    outdated_modules: BTreeMap<(ModuleId, wamex_metadata::BumpVersion), InstantiatedModule>,

    // Computed global imports object, includes all loaded module exports.
    global_imports: Object,

    // Memory and table allocator for modules
    alloc_state: module_alloc::AllocState,
}

impl LinkageState {
    fn new() -> Self {
        let table = wasm_bindgen::function_table();
        Self {
            global_imports: Self::_get_main_exports(&table),
            alloc_state: module_alloc::AllocState::new(table.into()),

            loaded_modules: Default::default(),
            outdated_modules: Default::default(),
        }
    }

    // TODO: We can split exports into separate fields for modularity.
    pub fn save_loaded_module(
        &mut self,
        module_id: ModuleId,
        module: InstantiatedModule,
    ) -> Result<(), Error> {
        debug!("Get exports from instantiated module");
        let defined_exports = module.instantiated.exports();
        debug!("Module exports: {:?}", defined_exports);
        debug!("Store exports and module info in linkage state");

        self.loaded_modules.insert(module_id.clone(), module);

        // Extend global imports with module exports.
        // On adding to global imports - filter out module specific fields.
        copy_imports(&self.global_imports, &defined_exports, true)
    }

    // fn debug_object

    fn _get_main_exports(indirect_function_table: &JsValue) -> Object {
        let new_object = Object::new();
        let obj = wasm_bindgen::exports();

        log::debug!("Main exports value: {:?}", obj);
        let obj = obj.try_into().unwrap();
        copy_imports(&new_object, &obj, false).unwrap();
        let set = Reflect::set(
            &new_object,
            &JsValue::from_str("__indirect_function_table"),
            indirect_function_table,
        )
        .unwrap();
        assert!(set);
        new_object
    }

    fn global<F, U>(op: F) -> U
    where
        F: FnOnce(&mut LinkageState) -> U,
    {
        thread_local! {
            pub static LINKAGE_STATE: RefCell<LinkageState> = RefCell::new(LinkageState::new());
        }

        LINKAGE_STATE.with(|state_cell| {
            let mut state = state_cell
                .try_borrow_mut()
                .expect("LinkageState global reentrant borrow");
            let res = op(&mut state);
            res
        })
    }
}
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

pub async fn load(module_id: ModuleId, reload: bool) -> Result<bool, String> {
    load_inner(module_id, reload).await.map_err(|e| {
        log::error!("Error loading module: {}", e);
        e.to_string()
    })
}

// Load webassembly module from URL and instantiate it, link with active imports.
async fn load_inner(module_id: ModuleId, reload: bool) -> Result<bool, Error> {
    debug!("call load for module: {:?}", module_id);
    // 0. check if module already loaded, if reload - replace it.

    let contains = LinkageState::global(|state| match state.loaded_modules.get(&module_id) {
        Some(instantiated) => !instantiated.pending_updates(),
        None => false,
    });

    if contains && !reload {
        return Ok(false);
    }

    // 1. fetch ModuleDecl
    // TODO: use custom section instead of separate fetch?
    // let array = WebAssembly::Module::custom_sections(&module, "__wamex_metadata");
    let module_fut = fetch_buffer(module_id.build_url());
    // let decl_buffer = fetch_buffer(module_id.module_decl_url()).await?;
    // let decl_buffer = deserialize::buffer_to_rust(decl_buffer);

    // 2. Assert module decl "deps" are satisfied.

    // 3. fetch module
    let buffer = module_fut.await?;
    let module = WebAssembly::Module::new(&buffer).map_err(|error| Error::JsError {
        context: "Failed to create module",
        error,
    })?;

    // TODO: move this logic into subroutine
    let results = WebAssembly::Module::custom_sections(&module, "dylink.0");
    if results.length() != 1 {
        return Err(Error::DeserializationError(
            "No dylink.0 section found".into(),
        ));
    }

    let array = deserialize::buffer_to_rust(results.get(0));
    let metadata = deserialize::deserialize_metadata(&array)?;
    debug!("Fetched module metadata: {:?}", metadata);

    if !metadata.needed_libraries.is_empty() {
        debug!("Fetching module deps: {:?}", metadata.needed_libraries);
        for dep in &metadata.needed_libraries {
            let dep_id = ModuleId::new(&dep);
            Box::pin(load_inner(dep_id.clone(), false))
                .await
                .map_err(|e| Error::CannotResolveDependency {
                    module_id: module_id.clone(),
                    dependency: dep_id,
                    entry: None,
                    sub_error: Some(Box::new(e)),
                })?;
        }
    }

    let version = deserialize::parse_version(&module)?;

    debug!("Module version: {:?}", version);
    // 4. Create imports object (we can reuse global one?)
    // TODO: Limit only for needed imports?

    let global_imports = LinkageState::global(|state| state.global_imports.clone());

    let new_exports = Object::new();
    copy_imports(&new_exports, &global_imports, false)?;
    debug!("New exports: {:?}", new_exports);

    debug!(
        "Allocating memory:{} and fn_table:{} for module",
        metadata.memory_size, metadata.table_size
    );
    let entry = LinkageState::global(|state| {
        state.alloc_state.alloc(
            metadata.memory_size,
            metadata.memory_alignment,
            metadata.table_size,
        )
    })?;

    obj_set!(&new_exports, "__lib_base", entry.memory_start());
    obj_set!(&new_exports, "__table_base", entry.table_start());

    let imports = Object::new();
    obj_set!(&imports, "__wasm_split", new_exports);

    debug!("Instantiate module, imports: {:?}", imports);
    // 5. Instantiate module.

    let fut_res = JsFuture::from(WebAssembly::instantiate_module(&module, &imports))
        .await
        .map_err(|error| Error::JsError {
            context: "Failed to instantiate module",
            error,
        })?;

    // can be instance or TypeError|LinkError|CompileError|RuntimeError
    let sub_module_instance: WebAssembly::Instance =
        fut_res.dyn_into().map_err(|error| Error::JsError {
            context: "Failed to cast module instance",
            error,
        })?;

    let instantiated = InstantiatedModule {
        instantiated: sub_module_instance.clone(),
        version,
        alloc_guard: GuardedAllocEntry::new(entry),
        needs_update: false,
    };

    debug!("Saving module: {:?}", instantiated);

    // 6. Store exports and module info in linkage state.
    LinkageState::global(|state| state.save_loaded_module(module_id, instantiated))?;
    Ok(true)
}

pub fn mark_for_update(module_id: ModuleId) -> Result<(), Error> {
    LinkageState::global(|state| {
        let Some(instantiated) = state.loaded_modules.get_mut(&module_id) else {
            return Err(Error::ModuleNotFound { module_id });
        };
        instantiated.mark_for_update();
        Ok(())
    })
}

/// Forcibly unload module.
/// Each submodule can have static data, which is allocated dynamically on module load.
///
/// Calling this function will free that memory, and can cause use-after-free if some code still holds references
/// to that memory.
pub async unsafe fn unload(
    module: ModuleId,
    version: wamex_metadata::BumpVersion,
) -> Result<(), Error> {
    LinkageState::global(|state| {
        let mut done = false;
        if let Some(outdated) = state
            .outdated_modules
            .remove(&(module.clone(), version.clone()))
        {
            debug!("Found outdated module: {:?}, unloading...", outdated);
            outdated.free(state);
            done = true;
        }

        if let Some(instantiated) = state.loaded_modules.get(&module) {
            if instantiated.version == version {
                let instantiated = state.loaded_modules.remove(&module).unwrap();
                debug!("Unloading current version of module: {:?}", instantiated);
                instantiated.free(state);
                done = true;
            }
        };

        if !done {
            return Err(Error::ModuleNotFound { module_id: module });
        }
        Ok(())
    })
}
