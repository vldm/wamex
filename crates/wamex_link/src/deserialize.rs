use js_sys::WebAssembly::Module;
use wamex_metadata::BumpVersion;
use wasm_bindgen::JsValue;

use super::Error;

pub fn deserialize_metadata(
    array: &[u8],
) -> Result<wamex_metadata::dylink0::Dylink0Section<'_>, Error> {
    let decoded = wasmparser::Dylink0SectionReader::new(wasmparser::BinaryReader::new(&array, 0));
    wamex_metadata::dylink0::Dylink0Section::from_reader(decoded).map_err(|e| {
        Error::DeserializationError(format!("Failed to parse dylink.0 section: {e}").into())
    })
}

pub fn buffer_to_rust(buffer: JsValue) -> Vec<u8> {
    js_sys::Uint8Array::new(&buffer).to_vec()
}

pub fn parse_version(module: &Module) -> Result<BumpVersion, Error> {
    let results = Module::custom_sections(module, "__wamex_version");
    if results.length() != 1 {
        return Err(Error::DeserializationError(
            "No __wamex_version section found".into(),
        ));
    }
    let array = buffer_to_rust(results.get(0));
    if array.len() != 4 {
        return Err(Error::DeserializationError(
            "Invalid __wamex_version section length".into(),
        ));
    }
    Ok(BumpVersion::from_bytes(&array[..4].try_into().unwrap()))
}
