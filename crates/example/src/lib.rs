use std::pin::Pin;
use wasm_bindgen::prelude::*;

#[cfg(feature = "split")]
use wasm_split::wasm_split;

#[cfg_attr(feature = "split", wasm_split(static_str))]
async fn static_str() -> Pin<Box<&'static str>> {
    Box::pin("foo123123")
}

// #[cfg_attr(feature = "split", wasm_split(string_from_static))]
// async fn string_from_static() -> Pin<Box<String>> {
//     Box::pin(String::from("barssdaas2"))
// }

#[wasm_bindgen]
pub async fn print_lazy_loaded_string(data: &str) -> Result<String, JsError> {
    let data = if data == "static" {
        format!("{}", static_str().await)
    }
    //  else if data == "string" {
    //     format!("{}", string_from_static().await)
    // }
    else {
        format!("{}", data)
    };

    Ok(data)
}
