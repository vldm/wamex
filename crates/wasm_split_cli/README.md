# Module splitting and dynamic loading
During development of large WebAssembly application, some pieces of app can be rarely used,
but they still needed to be downloaded by browser.
This can lead to large initial download size and slow loading time.

Another common problem is that feedback loop for such application is too slow.
This is because the whole module needs to be recompiled and then reloaded.
Reducing recompilation time can significantly improve development experience, another solution is to implement some kind of hot reloading.
Dioxus and Leptos are both implementing hot reloading, but it is limited only for html layout changes.
Changing logic of the application requires full reload of the page.
But this reload clear all "in-memory" state. Which makes "hot reloading" not so useful.

Original author of https://github.com/jbms/wasm-split-prototype/tree/main was aimed on solving first problem.
This project is based on that prototype but in mind with js [HMR](https://pinia.vuejs.org/cookbook/hot-module-replacement.html).

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
fn main
├── MAIN_STR
├── fn sub_method
│   └── VAR
└── MAIN_STR
```
The `main` function depends on `MAIN_STR` and `sub_method`, which in turn depends on `VAR`.
By default (without `no_mangle` attribute) `VAR` and `MAIN_STR` is not exported as global variable, but it symbol information 
is still available in the `linking` section of the WASM module. Names of functions are available in the `name` section.

The `relocation` table represents uses of these symbols in the code and data segments.
In order to build a dependency graph, WASM application need to be build with relocation information, and all this custom sections should be available and parsed.

More details are available at: https://github.com/WebAssembly/tool-conventions/blob/main/Linking.md


## Sub-module convention:

Source module is a wasm module that contains all the code and data that is needed to run the application. After splitting it will have one main module and multiple sub modules. Main module is the one that is loaded first and contains the entry point of the application. It will lazy load all submodules when needed.

Sub modules are parts of source modules that need to be extracted, each sub module contain it's own entrypoint functions that along with all unique dependencies need to be extracted from the source module. In order to find sub module, this tool uses convention
that was implemented in the original prototype. Sub module should contain two functions marked as `#[no_mangle]`:
- `__wasm_split_00{SUB_MODULE_NAME}00_export_{RANDOM_ID}` - this function is sub module entry point, it is defined in the sub module and exported to the main module.
- `__wasm_split_00{SUB_MODULE_NAME}00_import_{RANDOM_ID}` - this function is used in the main module to lazy load sub module and call it's entry point.

