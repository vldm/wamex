extern crate alloc;
use core::error;
use std::cell::RefCell;

use js_sys::{
    Object, Reflect,
    WebAssembly::{self},
};
use wamex_types::{BumpVersion, ModuleId, map_vec::MiniMap};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, Response};

use crate::{js_helpers::copy_imports, module_alloc::RawAllocEntry};

mod deserialize;
mod js_helpers;
mod logs;
mod module_alloc;

pub type Result<T, E = String> = std::result::Result<T, E>;

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

#[derive(Debug)]
struct InstantiatedModule {
    version: BumpVersion,
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

type Waiter = futures::channel::oneshot::Sender<()>;
type WaiterHandle = futures::channel::oneshot::Receiver<()>;
//TODO: Remove unwraps allow to reduce build size.
#[derive(Debug)]
struct LinkageState {
    //TODO: Use Enum for module state: Loaded, Pending
    // Handle to all loaded modules
    loaded_modules: MiniMap<ModuleId, InstantiatedModule>,
    pending_modules: MiniMap<ModuleId, Vec<Waiter>>,

    // If module was reloaded, we keep old modules until `unload` is called.
    outdated_modules: MiniMap<(ModuleId, BumpVersion), InstantiatedModule>,

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
            pending_modules: Default::default(),
            outdated_modules: Default::default(),
        }
    }
    pub fn mark_pending(&mut self, module_id: &ModuleId) -> Option<WaiterHandle> {
        if let Some(other_waiter) = self.pending_modules.get_mut(&module_id) {
            let (sender, receiver) = futures::channel::oneshot::channel();
            other_waiter.push(sender);
            Some(receiver)
        } else {
            self.pending_modules.insert(module_id.clone(), vec![]);
            None
        }
    }

    // TODO: We can split exports into separate fields for modularity.
    pub fn save_loaded_module(
        &mut self,
        module_id: ModuleId,
        module: InstantiatedModule,
    ) -> Result<Vec<Waiter>, Error> {
        debug!("Get exports from instantiated module");
        let defined_exports = module.instantiated.exports();
        debug!("Module exports: {:?}", defined_exports);
        debug!("Store exports and module info in linkage state");

        let version = module.version.clone();
        if let Some(old_v) = self.loaded_modules.insert(module_id.clone(), module) {
            warn!(
                "Module {module_id:?} was already loaded, replacing previous: {:?}",
                old_v
            );

            if old_v.version != version {
                debug!(
                    "Saving outdated module version: {:?}, {:?}",
                    module_id, old_v.version
                );
                self.outdated_modules
                    .insert((module_id.clone(), old_v.version.clone()), old_v);
            }
        }

        // Extend global imports with module exports.
        // On adding to global imports - filter out module specific fields.
        copy_imports(&self.global_imports, &defined_exports, true)?;
        Ok(self.pending_modules.remove(&module_id).unwrap_or_default())
    }

    fn debug_keys(obj: &Object) {
        let keys = Object::keys(obj);
        let mut key_list = vec![];
        for i in 0..keys.length() {
            let key = keys.get(i);
            key_list.push(key.as_string().unwrap_or_default());
        }
        debug!("Object keys: {:?}", key_list);
    }

    fn _get_main_exports(indirect_function_table: &JsValue) -> Object {
        let new_object = Object::new();
        let obj = wasm_bindgen::exports();

        debug!("Main exports value: {:?}", obj);
        let obj = obj.try_into().unwrap();
        Self::debug_keys(&obj);
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

// Load webassembly module from URL and instantiate it, link with active imports.
// Return true if module was updated during this load call.
pub async fn load(module_id: ModuleId, reload: bool) -> Result<bool, String> {
    Box::pin(load_inner(module_id, reload)).await.map_err(|e| {
        error!("Error loading module: {}", e);
        e.to_string()
    })
}

// Load webassembly module from URL and instantiate it, link with active imports.
// Return true if module was updated during this load call.
async fn load_inner(module_id: ModuleId, reload: bool) -> Result<bool, Error> {
    debug!("call load for module: {:?}", module_id);
    // 0. check if module already loaded, if reload - replace it.

    // TODO: fix structure:
    // 1. non-versioned modules (latest).
    // 2. outdated modules (by version).
    // 3. pending modules (loading in progress).
    // Avoid loading multiple times same version.
    let contains = LinkageState::global(|state| match state.loaded_modules.get(&module_id) {
        Some(instantiated) => !instantiated.pending_updates(),
        None => false,
    });

    if contains && !reload {
        return Ok(false);
    }

    // is another load in progress?
    let waiter = LinkageState::global(|state| state.mark_pending(&module_id));

    if let Some(waiter) = waiter {
        debug!(
            "Another load in progress for module {:?}, waiting...",
            module_id
        );
        // wait for other load to finish
        waiter.await.unwrap();
        debug!("Load finished for module {:?}, continuing...", module_id);
        return Ok(false);
    }

    // 1. fetch ModuleDecl
    let module_fut = fetch_buffer(module_id.download_url());
    // 2. Assert module decl "deps" are satisfied.
    //TODO: implement dependency checking, and export filtering.

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
    debug!(
        "Fetched module {} metadata: {:?}",
        module_id.module_name(),
        metadata
    );

    if !metadata.needed_libraries.is_empty() {
        debug!(
            "Fetching module {} deps: {:?}",
            module_id.module_name(),
            metadata.needed_libraries
        );
        for dep in &metadata.needed_libraries {
            let dep_id = ModuleId::dep_from_module(&module_id, &dep);
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

    debug!("Module {} version: {:?}", module_id.module_name(), version);
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

    LinkageState::debug_keys(&new_exports);
    let imports = Object::new();
    obj_set!(&imports, "__wamex", new_exports);

    debug!(
        "Instantiate module {}, imports: {:?}",
        module_id.module_name(),
        imports
    );
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
    let waiters =
        LinkageState::global(|state| state.save_loaded_module(module_id.clone(), instantiated))?;
    // notify waiters
    if !waiters.is_empty() {
        debug!(
            "Notifying {} waiters for module {:?}",
            waiters.len(),
            module_id
        );
    }
    for waiter in waiters {
        let _ = waiter.send(());
    }
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
pub async unsafe fn unload(module: ModuleId, version: BumpVersion) -> Result<(), Error> {
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

#[wasm_bindgen]
pub fn __wamex_reload(url_base_path: &str, module_name: &str) -> js_sys::Promise {
    let module_id = ModuleId::new_with_url(module_name, url_base_path);
    let future = async move {
        match load(module_id, true).await {
            Ok(updated) => Ok(JsValue::from_bool(updated)),
            Err(e) => Err(JsValue::from_str(&format!(
                "Failed to reload module: {}",
                e
            ))),
        }
    };
    wasm_bindgen_futures::future_to_promise(future)
}
