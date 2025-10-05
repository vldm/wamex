use std::pin::Pin;

#[cfg(feature = "split")]
use wamex::wasm_split;

#[cfg_attr(feature = "split", wasm_split(static_str))]
pub fn static_str() -> Pin<Box<&'static str>> {
    Box::pin("SUPER STATIC   STRING")
}

#[cfg_attr(feature = "split", wasm_split(string_from_static))]
pub fn string_from_static() -> Pin<Box<String>> {
    let mut new = String::from("OTHER STATIC STRING");
    new.push_str("some_test");

    Box::pin(new)
}

#[cfg_attr(feature = "split", wasm_split(async_string))]
pub async fn async_string() -> String {
    async { "ASYNC STRING".to_string() }.await
}

fn impl_dyn_fns() -> String {
    "DYN FNS".to_string()
}

#[cfg_attr(feature = "split", wasm_split(multiple_dyn_fns))]
pub fn multiple_dyn_fns(first_part: bool) -> String {
    if first_part {
        dyn_fns_inner(&|| impl_dyn_fns().split(' ').next().unwrap().to_string())
    } else {
        dyn_fns_inner(&impl_dyn_fns)
    }
}

#[inline(never)]
pub fn dyn_fns_inner(func: &dyn Fn() -> String) -> String {
    func()
}
