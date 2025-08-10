use wasm_split::wasm_split;

fn my_secret_exported_function() -> usize {
    #[link(wasm_import_module = "./__wasm_split.js")]
    extern "C" {
        fn __wasm_split_load_exported_secret(
            callback: unsafe extern "C" fn(*const ::std::ffi::c_void, bool),
            data: *const ::std::ffi::c_void,
        ) -> ();

        #[allow(improper_ctypes)]
        fn __wasm_split_00exported_secret00_import_4e1afc07a63998b484feddfe971bf2c7_my_secret_exported_function(
        ) -> usize;

    }
    #[allow(improper_ctypes_definitions)]
    #[no_mangle]
    pub extern "C" fn __wasm_split_00exported_secret00_export_4e1afc07a63998b484feddfe971bf2c7_my_secret_exported_function(
    ) -> usize {
        42
    }

    // Load this library
    unsafe { __wasm_split_load_exported_secret(load_callback_sync, std::ptr::null()) };

    unsafe {
        __wasm_split_00exported_secret00_import_4e1afc07a63998b484feddfe971bf2c7_my_secret_exported_function()
    }
}

unsafe extern "C" fn load_callback_sync(loader: *const std::ffi::c_void, success: bool) {
    // Do nothing
}

#[wasm_split(my_secret_exported_function_split)]
async fn my_secret_exported_function_split() -> usize {
    42
}

#[no_mangle]
pub extern "C" fn add(mut left: usize, right: usize) -> usize {
    if left > 10 {
        left = my_secret_exported_function();
    }
    left + right
}

#[no_mangle]
pub extern "C" fn void_fn() {
    // This is a dummy function to ensure that the library is not empty.
    // It can be used to test the linking and loading of the library.
}
