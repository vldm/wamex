use js_sys::{Array, Object, Reflect};
use wasm_bindgen::JsValue;

use super::Error;

#[macro_export]
macro_rules! obj_set {
    ($obj:expr, $key:literal, $value:expr) => {
        obj_set!($obj, &JsValue::from_str($key), &$value.into())
    };
    ($obj:expr, $key:expr, $value:expr) => {{
        let set = Reflect::set($obj, &$key, $value).map_err(|error| Error::JsError {
            context: "Failed to set import",
            error,
        })?;
        if !set {
            return Err(Error::JsError {
                context: "Failed to set import",
                error: JsValue::from("Reflect::set returned false"),
            });
        }
    }};
}

pub fn copy_imports(
    target: &Object,
    source: &Object,
    remove_blacklisted_imports: bool,
) -> Result<(), Error> {
    thread_local! {
        static BLACKLISTED_FIELDS: Vec<JsValue> = [
            "memory",
            "__indirect_function_table",
            "__lib_base",
            "__table_base",
        ].into_iter().map(JsValue::from_str).collect()
    }

    let entries = Object::entries(source);
    for entry in entries.iter() {
        let pair = Array::from(&entry);
        let key = pair.get(0);
        let value = pair.get(1);

        if remove_blacklisted_imports && BLACKLISTED_FIELDS.with(|bl| bl.contains(&key)) {
            continue;
        }

        if cfg!(debug_assertions) {
            let key_str = key.as_string().unwrap_or_default();

            let existing = Reflect::get(target, &key).map_err(|e| Error::JsError {
                context: "Failed to get existing import",
                error: e,
            })?;
            if !existing.is_undefined() {
                web_sys::console::warn_1(
                    &format!("Overriding existing import: {}", key_str).into(),
                );
            }
        }
        obj_set!(target, &key, &value);
    }
    Ok(())
}
