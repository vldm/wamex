# WAsm module EXtractor (WAMEX)
WAMEX is a tool for splitting large WebAssembly module into few smaller dynamically loadable modules.
It is based on [wasm-split-prototype](https://github.com/jbms/wasm-split-prototype/tree/main) by jbms,
unlike `wasm-split` splitted modules are partially implementing [dynamic linking convention](https://github.com/WebAssembly/tool-conventions/blob/main/DynamicLinking.md).

This allows to load and reload them on demand, which can be used for hot module reloading (HMR) during development.
Also making modules dynamically loadable allows compatibility checking of provided and required symbols during runtime loading.

## Motivation
During development of large WebAssembly application, some pieces of app can be rarely used,
but they still needed to be downloaded by browser.
This can lead to large initial download size and slow loading time.

Another common problem is that during development - feedback loop for such application is too slow.
This is because the whole module needs to be recompiled and reloaded, also there is no way to resume application state after reload.
Incremental compiliation reduces compile time significantly and improve development experience, but another solution might be to implement some kind of hot reloading.
Front-end frameworks like Dioxus and Leptos are both implementing hot reloading, but it is limited only for html layout changes.
And currently changing logic of the application requires full reload of the page.
But this reload clear all "in-memory" state. Which makes "hot reloading" not so useful.

The idea of `wamex` is inspired by [HMR](https://pinia.vuejs.org/cookbook/hot-module-replacement.html).

# Usage
There three main parts of the tool - macro that marks functions as splitable, CLI tool that performs the actual splitting, and loader that loads splitted modules during runtime.

## Macro usage

Include `wamex` crate in your `Cargo.toml`:

```toml
[dependencies]
wamex = "0.1"
```

Then mark functions that should be extracted into separate module with `#[wamex::split]` attribute:

```rust
#[wamex::split(module_name)]
fn heavy_computation() -> u32 {
    // some heavy computation here
}
#[wamex::split(module_name)]
async fn fetch_data() {
    // some async data fetching here
}
```

Functions marked with this attribute will be extracted during wamex CLI tool execution.
Wamex suport both sync and async functions, but for end usage async function will be generated. This is because loading sub-module is async operation.

Marking function with `#[wamex::split]` will generate code like this:
```rust
pub fn heavy_computation() -> impl ::core::future::Future<Output = u32> {
        #[link(wasm_import_module = "./__wamex_loader.rs")]
        extern "C" {
            fn IMPORT_FN() -> u32;
        }
        #[no_mangle]
        pub extern "C" fn EXPORT_FN() -> u32 {
            // some heavy computation here
        }
        async {
            let _ = ::wamex::load(::wamex::ModuleId::new("module_name")).await;
            unsafe{IMPORT_FN()}
        }
    }
```
The details about name of import/export functions are described in [Sub-module convention](#sub-module-convention) section.
This structure can be recreated in other languages, so `wamex-cli` can be used to split modules written in other languages too.

# Runtime linking
Currently `wamex` comes with `wamex-loader` crate that provides fully functional runtime loader based on `wasm-bindgen` and `web-sys`.
By the report of `twiggy` its footprint on MVP is around 100Kb, mostly because of `DynamicLinking` parsing, this size is big for web, but there are lot of opportunities for optimizations in future.

# Implementation details

## Building dependency graph:
Firstly tool build dependency graph for the wasm module.
Each node is rather a function or a static/constant piece of data.

In code it looks like this (for simplifcation we use `static` but same works for inline constants, and other "non-simple" data):
```rust 
static VAR: &str = "Hello, world!";
fn sub_method() -> &str {
    VAR
}
fn main(hot: bool) -> &str {
    static MAIN_STR: &str = "Hello from main!";
    if hot {
        MAIN_STR
    }
    else {
        sub_method()
    }
}
```
In this code graph will look like this:
```
fn: main
├── DATA: MAIN_STR
├── fn: sub_method
│   └── DATA: VAR
└── DATA: MAIN_STR
```
The `main` function depends on `MAIN_STR` and `sub_method`, which in turn depends on `VAR`.
`VAR` and `MAIN_STR` is not exported as global variable in WASM, but it symbol information 
is still available in the `linking` section of the WASM module. Names of functions are available in the `name` section.

The `relocation` table represents uses of these symbols in the code and data segments.
In order to build a dependency graph, WASM application need to be build with relocation information, and all this custom sections should be available and parsed.

More details are available at: https://github.com/WebAssembly/tool-conventions/blob/main/Linking.md


## Sub-module convention:

Source module is a wasm module that contains all the code and data that is needed to run the application. After splitting it will have one main module and multiple sub modules. Main module is the one that is loaded first and contains the entry point of the application. It will lazy load all submodules when needed.

Sub modules are parts of source modules that need to be extracted, each sub module contain it's own entrypoint functions that along with all unique dependencies need to be extracted from the source module. In order to find sub module, this tool uses convention
that was implemented in the original prototype. Sub module should contain two functions marked as `#[no_mangle]`:
- `__wamex_00{SUB_MODULE_NAME}00_export_{SALT}` - this function is sub module entry point, it is defined in the sub module and exported to the main module.
- `__wamex_00{SUB_MODULE_NAME}00_import_{SALT}` - this function is used in the main module to lazy load sub module and call it's entry point.
