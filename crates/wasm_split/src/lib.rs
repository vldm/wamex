use std::{
    cell::Cell,
    ffi::c_void,
    future::Future,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll, Waker},
};

pub use wasm_split_macros::wasm_split;

pub type LoadCallbackFn = unsafe extern "C" fn(*const c_void, bool) -> ();
pub type LoadFn = unsafe extern "C" fn(LoadCallbackFn, *const c_void) -> ();

type Lazy = async_once_cell::Lazy<Option<()>, SplitLoaderFuture>;

pub struct LazySplitLoader {
    lazy: Pin<Rc<Lazy>>,
}

impl LazySplitLoader {
    pub unsafe fn new(load: LoadFn) -> Self {
        Self {
            lazy: Rc::pin(Lazy::new(SplitLoaderFuture::new(SplitLoader::new(load)))),
        }
    }
}

pub async fn ensure_loaded(loader: &'static std::thread::LocalKey<LazySplitLoader>) -> Option<()> {
    *loader.with(|inner| inner.lazy.clone()).as_ref().await
}

#[derive(Clone, Copy, Debug)]
enum SplitLoaderState {
    Deferred(LoadFn),
    Pending,
    Completed(Option<()>),
}

struct SplitLoader {
    state: Cell<SplitLoaderState>,
    waker: Cell<Option<Waker>>,
}

impl SplitLoader {
    fn new(load: LoadFn) -> Rc<Self> {
        Rc::new(SplitLoader {
            state: Cell::new(SplitLoaderState::Deferred(load)),
            waker: Cell::new(None),
        })
    }

    fn complete(&self, value: bool) {
        self.state.set(SplitLoaderState::Completed(if value {
            Some(())
        } else {
            None
        }));
        match self.waker.take() {
            Some(waker) => {
                waker.wake();
            }
            _ => {}
        }
    }
}

struct SplitLoaderFuture {
    loader: Rc<SplitLoader>,
}

impl SplitLoaderFuture {
    fn new(loader: Rc<SplitLoader>) -> Self {
        SplitLoaderFuture { loader }
    }
}

impl Future for SplitLoaderFuture {
    type Output = Option<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<()>> {
        match self.loader.state.get() {
            SplitLoaderState::Deferred(load) => {
                self.loader.state.set(SplitLoaderState::Pending);
                self.loader.waker.set(Some(cx.waker().clone()));
                unsafe {
                    load(
                        load_callback,
                        Rc::<SplitLoader>::into_raw(self.loader.clone()) as *const c_void,
                    )
                };
                Poll::Pending
            }
            SplitLoaderState::Pending => {
                self.loader.waker.set(Some(cx.waker().clone()));
                Poll::Pending
            }
            SplitLoaderState::Completed(value) => Poll::Ready(value),
        }
    }
}

unsafe extern "C" fn load_callback(loader: *const c_void, success: bool) {
    unsafe { Rc::from_raw(loader as *const SplitLoader) }.complete(success);
}


// pub enum LinkKind {
//     Function,
//     Global,
// }
// struct LinkEntry {
//     module: String,
//     name: String,
//     kind: LinkKind,
// }

// pub struct WasmModule {
//     module_file: String,
//     exports: Vec<LinkEntry>,
//     imports: Vec<LinkEntry>,
// }

// impl WasmModule {
//     pub fn new() -> Self {
//         Self {
//             module_file: "__wasm_split".to_string(),
//             exports: Vec::new(),
//             imports: Vec::new(),
//         }
//     }

//     pub fn init(&mut self, global_exports: Vec<LinkEntry>) {
//         // check if global exports can fulfill imports
//         for import in &self.imports {
//             if !global_exports
//                 .iter()
//                 .any(|e| e.name == import.name && e.module == import.module)
//             {
//                 panic!(
//                     "Import {} from module {} not found in global exports",
//                     import.name, import.module
//                 );
//             }
//         }
//     }
// }
