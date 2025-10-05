use wasm_bindgen::prelude::*;
use wasm_log::Config;
mod sub;

#[wasm_bindgen]
pub async fn print_lazy_loaded_string(data: &str) -> Result<String, JsError> {
    let _ = wasm_log::try_init(Config::default());
    let data = if data == "static" {
        format!("{}", sub::static_str().await)
    } else if data == "string" {
        format!("{}", sub::string_from_static().await)
    } else if data == "async" {
        format!("{}", sub::async_string().await)
    } else if data == "dyn" {
        format!("{}", sub::multiple_dyn_fns(true).await)
    } else {
        format!("{}", data)
    };

    Ok(data)
}
