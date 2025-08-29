use std::pin::Pin;

#[cfg(feature = "split")]
use wamex::wasm_split;

#[cfg_attr(feature = "split", wasm_split(static_str))]
pub async fn static_str() -> Pin<Box<&'static str>> {
    Box::pin("SUPER STATIC   STRING")
}

#[cfg_attr(feature = "split", wasm_split(string_from_static))]
pub async fn string_from_static() -> Pin<Box<String>> {
    let mut new = String::from("OTHER   STATIC STRING");
    new.push_str("one more");
    new.push_str("some_test");

    Box::pin(new)
}
