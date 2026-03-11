extern crate alloc;
use core::error;
use std::cell::RefCell;

use js_sys::{Object, Reflect, WebAssembly};
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
#[derive(Debug)]
pub struct CantResolveDependency {
    module_id: ModuleId,
    dependency: ModuleId,
    entry: Option<String>,
    sub_error: Option<Box<dyn error::Error>>,
}
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Module {} not found", .module_id.module_name())]
    ModuleNotFound { module_id: ModuleId },

    #[error(
        "Cannot resolve dependency {dependency:?}:{entry} needed for module {module_id:?}\n => {sub_error}", 
        entry = .0.entry.as_deref().unwrap_or("<none>"), 
        sub_error = .0.sub_error.as_ref().map(|e| e.to_string()).unwrap_or("<none>".to_string()),
        module_id = .0.module_id,
        dependency = .0.dependency,
    )]
    CannotResolveDependency(Box<CantResolveDependency>),
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
    instantiated: WebAssembly::Instance,
    alloc_guard: GuardedAllocEntry,
}

impl InstantiatedModule {
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

#[derive(Debug, Default)]
enum ModuleFetchState {
    Loaded {
        module_version: BumpVersion,
        module: InstantiatedModule,
    },
    Pending(Vec<Waiter>),
    #[default]
    Invalid,
}

#[derive(Debug, Default)]
struct ModuleInfo {
    // Load updates from this URL base.
    last_url_base: String,
    latest_module: ModuleFetchState,
    // Modules that was loaded before, but now outdated.
    // This modules are still kept in memory, may have some live references, so freeing them is unsafe.
    outdated_versions: MiniMap<BumpVersion, InstantiatedModule>,
    try_refetch: bool,
}

impl ModuleInfo {
    pub fn loaded_version(&self) -> Option<BumpVersion> {
        match &self.latest_module {
            ModuleFetchState::Loaded { module_version, .. } => Some(*module_version),
            _ => None,
        }
    }

    // Try refetch module on next load.
    // If version is updated - reload it.
    pub fn mark_for_refetch(&mut self) {
        self.try_refetch = true;
    }
    // Return WaiterHandle if load in progress.
    pub fn try_subscribe_for_active_fetch(&mut self) -> Option<WaiterHandle> {
        match &mut self.latest_module {
            ModuleFetchState::Pending(waiters) => {
                let (tx, rx) = futures::channel::oneshot::channel();
                waiters.push(tx);
                Some(rx)
            }
            _ => None,
        }
    }
    #[allow(clippy::wrong_self_convention)]
    pub fn to_fetch_state(&mut self) {
        match std::mem::replace(&mut self.latest_module, ModuleFetchState::Pending(vec![])) {
            ModuleFetchState::Loaded {
                module_version,
                module,
            } => {
                // save old module to outdated versions
                self.outdated_versions.insert(module_version, module);
            }
            ModuleFetchState::Invalid => {
                // nothing to do
            }
            // If it was already pending - recover
            state => {
                self.latest_module = state;
            }
        }
    }

    fn take_waiters(&mut self, new_state: ModuleFetchState) -> Vec<Waiter> {
        match std::mem::replace(&mut self.latest_module, new_state) {
            ModuleFetchState::Pending(waiters) => waiters,
            state => {
                panic!("Invalid state transition: {:?}", state);
            }
        }
    }

    #[allow(clippy::wrong_self_convention)]
    pub fn to_loaded_state(
        &mut self,
        module_version: BumpVersion,
        module: InstantiatedModule,
    ) -> Vec<Waiter> {
        let new_state = ModuleFetchState::Loaded {
            module_version,
            module,
        };

        self.try_refetch = false;

        self.take_waiters(new_state)
    }

    pub fn abort_fetch(&mut self) -> Vec<Waiter> {
        assert!(
            matches!(&self.latest_module, ModuleFetchState::Pending(_)),
            "Can only abort pending fetch"
        );

        if let Some((old_version, old_module)) = self.outdated_versions.take_last() {
            return self.to_loaded_state(old_version, old_module);
        }

        self.take_waiters(ModuleFetchState::Invalid)
    }
    pub fn is_pending(&self) -> bool {
        matches!(&self.latest_module, ModuleFetchState::Pending(_))
    }
    pub fn is_invalid(&self) -> bool {
        self.last_url_base.is_empty()
            && matches!(&self.latest_module, ModuleFetchState::Invalid)
            && self.outdated_versions.is_empty()
    }
}

type ModuleName = String;
type Waiter = futures::channel::oneshot::Sender<()>;
type WaiterHandle = futures::channel::oneshot::Receiver<()>;
//TODO: Remove unwraps allow to reduce build size.
#[derive(Debug)]
struct LinkageState {
    //TODO: Use Enum for module state: Loaded, Pending
    // Handle to all loaded modules
    modules: MiniMap<ModuleName, ModuleInfo>,

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

            modules: Default::default(),
        }
    }
    pub fn subscribe_for_active_fetch(&mut self, module_name: &str) -> Option<WaiterHandle> {
        let module = self.modules.get_mut(module_name)?;
        module.try_subscribe_for_active_fetch()
    }

    /// Return true if module needs to be refetched.
    pub fn need_fetch(&self, module_id: &ModuleId) -> bool {
        let module = match self.modules.get(module_id.module_name()) {
            Some(m) => m,
            None => return true,
        };

        let loaded_version = module.loaded_version().expect("Module should be loaded");
        loaded_version < module_id.version().unwrap_or_default()
            || module.try_refetch && module_id.version().is_none()
    }
    // Set module state to fetching.
    pub fn fetch_module(&mut self, module_id: &ModuleId) {
        let module = self
            .modules
            .entry(module_id.module_name().to_string())
            .or_insert_with(ModuleInfo::default);
        module.to_fetch_state();
    }

    pub fn old_version(&self, module_id: &ModuleId) -> Option<BumpVersion> {
        self.modules
            .get(module_id.module_name())
            .expect("Module should be present")
            .outdated_versions
            .last()
            .map(|(v, _)| *v)
    }

    // Abort ongoing load, return waiters to notify.
    pub fn abort_load_module(&mut self, module_id: &ModuleId) -> Vec<Waiter> {
        let module = self
            .modules
            .get_mut(module_id.module_name())
            .expect("Module should be present");
        let res = module.abort_fetch();
        if module.is_invalid() {
            self.modules.remove(module_id.module_name());
        }
        res
    }

    // TODO: We can split exports into separate fields for modularity.
    pub fn save_loaded_module(
        &mut self,
        module_id: ModuleId,
        version: BumpVersion,
        im: InstantiatedModule,
    ) -> Result<Vec<Waiter>, Error> {
        let defined_exports = im.instantiated.exports();
        debug!("Module exports: {:?}", defined_exports);

        let module = self
            .modules
            .get_mut(module_id.module_name())
            .expect("Module should be present");

        // If version > replace state to loaded
        // if version <= keep old module
        if let Some((old_v, _)) = module.outdated_versions.last()
            && version <= *old_v
        {
            warn!(
                "Loaded module {} version not updated (old:{:?}, new:{:?}), skipping reload",
                module_id.module_name(),
                old_v,
                version
            );
            module.abort_fetch();
        }

        let waiters = module.to_loaded_state(version, im);

        // Extend global imports with module exports.
        // On adding to global imports - filter out module specific fields.
        copy_imports(&self.global_imports, &defined_exports, true)?;
        Ok(waiters)
    }

    fn debug_keys(obj: &Object) {
        trace!("Object keys: {:?}", {
            let keys = Object::keys(obj);
            (0..keys.length())
                .map(|i| {
                    let key = keys.get(i);
                    key.as_string().unwrap_or_default()
                })
                .collect::<Vec<_>>()
        });
    }

    fn _get_main_exports(indirect_function_table: &JsValue) -> Object {
        let new_object = Object::new();
        let obj = wasm_bindgen::exports();

        trace!("Main exports value: {:?}", obj);
        let obj = obj.into();
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
            op(&mut state)
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
    let response = future
        .await
        .map_err(|e| Error::FetchError { url, error: e })?;

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
pub async fn load(module_id: ModuleId) -> bool {
    Box::pin(try_load(module_id))
        .await
        .map_err(|e| {
            error!("Error loading module: {}", e);
            e
        })
        .unwrap()
}

// Load webassembly module from URL and instantiate it, link with active imports.
// Return true if module was updated during this load call.
pub async fn try_load(module_id: ModuleId) -> Result<bool, Error> {
    debug!("call load for module: {:?}", module_id);
    // is another load in progress?
    let waiter =
        LinkageState::global(|state| state.subscribe_for_active_fetch(module_id.module_name()));

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

    // If no version specified - but marked for update - we need to refetch it.
    let need_update = LinkageState::global(|state| state.need_fetch(&module_id));
    if !need_update {
        return Ok(false);
    }

    LinkageState::global(|state| state.fetch_module(&module_id));

    // 1. fetch module buffer
    let buffer = fetch_buffer(module_id.download_url()).await?;
    // 2. Assert module decl "deps" are satisfied.
    //TODO: implement dependency checking, and export filtering.

    let module = WebAssembly::Module::new(&buffer).map_err(|error| Error::JsError {
        context: "Failed to create module",
        error,
    })?;

    let version = deserialize::parse_version(&module)?;

    debug!("Module {} version: {:?}", module_id.module_name(), version);

    if let Some(old_version) = LinkageState::global(|state| state.old_version(&module_id))
        && version <= old_version
    {
        warn!(
            "Module {} version not updated (old:{:?}, new:{:?}), skipping reload",
            module_id.module_name(),
            old_version,
            version
        );
        LinkageState::global(|state| state.abort_load_module(&module_id));
        return Ok(false);
    }

    let module_id_clone = module_id.clone();

    // Wrap all load routine in async block to be able to abort on error.
    let res = async move {
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
                let dep_id = ModuleId::dep_from_module(&module_id, dep);
                let fut = Box::pin(try_load(dep_id.clone()));
                let res = fut.await;
                if let Err(e) = res {
                    return Err(Error::CannotResolveDependency(Box::new(
                        CantResolveDependency {
                            module_id: module_id.clone(),
                            dependency: dep_id,
                            entry: None,
                            sub_error: Some(Box::new(e)),
                        },
                    )));
                }
            }
        }

        // 4. Create imports object (we can reuse global one?)
        // TODO: Limit only for needed imports?

        let global_imports = LinkageState::global(|state| state.global_imports.clone());

        let new_exports = Object::new();
        copy_imports(&new_exports, &global_imports, false)?;
        trace!("New exports: {:?}", new_exports);

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
            alloc_guard: GuardedAllocEntry::new(entry),
        };
        Ok((version, instantiated))
    }
    .await;

    let (result, waiters) = match res {
        Ok((version, instantiated)) => {
            debug!("Saving module: {:?}", instantiated);
            // 6. Store exports and module info in linkage state.
            let waiters = LinkageState::global(|state| {
                state.save_loaded_module(module_id_clone.clone(), version, instantiated)
            })?;

            (Ok(true), waiters)
        }
        Err(e) => {
            log::error!("Error loading module {:?}", module_id_clone);
            let waiters = LinkageState::global(|state| state.abort_load_module(&module_id_clone));
            (Err(e), waiters)
        }
    };

    // notify waiters
    if !waiters.is_empty() {
        debug!(
            "Notifying {} waiters for module {:?}",
            waiters.len(),
            module_id_clone
        );
    }
    for waiter in waiters {
        let _ = waiter.send(());
    }

    result
}

/// Forcibly unload module.
///
/// If no version is provided in module_id - all versions will be unloaded.
/// If version is provided only remove from outdated versions if it matches.
///
/// # Safety
/// Each submodule can have static data, which is allocated dynamically on module load.
///
/// Calling this function will free that memory, and can cause use-after-free if some code still holds references
/// to that memory.
///
pub async unsafe fn unload(module_id: ModuleId) -> Result<(), Error> {
    let done = LinkageState::global(|state| {
        if module_id.version().is_none() {
            let to_remove = state.modules.remove(module_id.module_name());
            if let Some(mut to_remove) = to_remove {
                // Keep pending loads.
                if to_remove.is_pending() {
                    let outdated_versions = std::mem::take(&mut to_remove.outdated_versions);
                    state
                        .modules
                        .insert(module_id.module_name().to_string(), to_remove);

                    debug!(
                        "Module {:?} is pending, unloading only outdated versions: {:?}",
                        module_id,
                        outdated_versions.len(),
                    );
                    return true;
                };

                debug!(
                    "Unloaded all versions of module: {:?}, total: {total_num}",
                    module_id,
                    total_num = 1 + to_remove.outdated_versions.len()
                );
                return true;
            }
        };

        // Unload specific outdated version.
        let Some(module) = state.modules.get_mut(module_id.module_name()) else {
            return false;
        };

        if let Some(version) = module_id.version()
            && let Some(instantiated) = module.outdated_versions.remove(&version)
        {
            debug!("Unloaded module: {:?} version: {:?}", module_id, version);
            instantiated.free(state);
            return true;
        }

        false
    });

    if !done {
        return Err(Error::ModuleNotFound { module_id });
    }
    Ok(())
}

#[cfg_attr(feature = "bindgen_refetch", wasm_bindgen)]
pub fn __wamex_mark_for_update(module_name: &str) -> bool {
    LinkageState::global(|state| {
        let Some(module_info) = state.modules.get_mut(module_name) else {
            return Err(Error::ModuleNotFound {
                module_id: ModuleId::new(module_name),
            });
        };
        module_info.mark_for_refetch();
        Ok(())
    })
    .is_ok()
}
