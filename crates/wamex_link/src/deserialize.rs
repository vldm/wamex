use rkyv::rancor;
use wasm_bindgen::JsValue;

use super::Error;

pub fn rkyv_deserialize<T>(
    value: &impl rkyv::Deserialize<T, rkyv::api::high::HighDeserializer<rancor::Error>>,
) -> Result<T, Error> {
    rkyv::api::high::deserialize::<T, rancor::Error>(value)
        .map_err(|e| Error::DeserializationError(e.into()))
}

pub fn deserialize_decl(buffer: &[u8]) -> Result<&wamex_metadata::ArchivedModule, Error> {
    let module = rkyv::access::<wamex_metadata::ArchivedModule, rancor::Error>(buffer)
        .map_err(|e| Error::DeserializationError(e.into()))?;
    // let module: wamex_metadata::Module =
    // rkyv::from_bytes(buffer).map_err(|e| Error::DeserializationError(e.into()))?;
    Ok(module)
}

pub fn buffer_to_rust(buffer: JsValue) -> Vec<u8> {
    js_sys::Uint8Array::new(&buffer).to_vec()
}
