use wasm_bindgen::prelude::*;
use wasm_log::Config;
mod sub;

#[wasm_bindgen]
pub async fn print_lazy_loaded_string(data: &str) -> Result<String, JsError> {
    wasm_log::init(Config::default());
    let data = if data == "static" {
        format!("{}", sub::static_str().await)
    } else if data == "string" {
        format!("{}", sub::string_from_static().await)
    } else {
        format!("{}", data)
    };

    Ok(data)
}
