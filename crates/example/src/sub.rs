use std::pin::Pin;
use super::*;

pub const SOME_STATIC_SHARED: &str = "SUPER STATIC SHARED STRING";
#[cfg_attr(feature = "split", wamex::split(static_str))]
pub fn static_str() -> Pin<Box<&'static str>> {
    Box::pin(SOME_STATIC_SHARED)
}

#[cfg_attr(feature = "split", wamex::split(string_from_static))]
pub fn string_build() -> Pin<Box<String>> {
    let mut new = String::from("OTHER STATIC STRING");
    new.push_str("small addition");

    Box::pin(new)
}

#[cfg_attr(feature = "split", wamex::split(string_build_with_shared_const))]
pub fn string_build_with_shared_const() -> Pin<Box<String>> {
    let mut new = String::from(SOME_STATIC_SHARED);
    new.push_str("hi");

    // core::panicking
    Box::pin(new)
}

#[cfg_attr(feature = "split", wamex::split(async_string))]
pub async fn async_string() -> String {
    async { "ASYNC STRING".to_string() }.await
}

fn impl_dyn_fns() -> String {
    "DYN FNS".to_string()
}

#[cfg_attr(feature = "split", wamex::split(multiple_dyn_fns))]
pub fn multiple_dyn_fns(first_part: bool) -> String {
    if first_part {
        dyn_fns_inner(&|| impl_dyn_fns().split(' ').next().unwrap().to_string())
    } else {
        dyn_fns_inner(&impl_dyn_fns)
    }
}

#[cfg_attr(feature = "split", wamex::split(dep_dyn))]
pub async fn dep_dyn() -> String {
    multiple_dyn_fns(false).await
}

#[inline(never)]
pub fn dyn_fns_inner(func: &dyn Fn() -> String) -> String {
    let mut res = func();
    res.push_str(" FROM INNER");
    res
}
