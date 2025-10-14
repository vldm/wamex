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
    } else if data == "dep_dyn" {
        format!("{}", sub::dep_dyn().await)
    } else if data.starts_with("debug") {
        // debug memory
        let mut data_parts = data.split(' ');
        let _ = data_parts.next();
        let offset = data_parts.next().unwrap().parse::<usize>().unwrap();
        let size = data_parts.next().unwrap().parse::<usize>().unwrap();
        let slice = unsafe { std::slice::from_raw_parts(offset as *const u8, size) };
        format!("{}", hex::encode(slice))
    } else {
        format!("{}", data)
    };

    Ok(data)
}
