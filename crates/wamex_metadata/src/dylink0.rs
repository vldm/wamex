//! Implementation of dylnk0 section encoding.
//!

use std::borrow::Cow;

use anyhow::{Result, bail};
#[cfg(feature = "encoder")]
use wasm_encoder::Encode;
use wasmparser::{Dylink0SectionReader, Dylink0Subsection};

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct ImportInfo<'src> {
    pub module: Cow<'src, str>,
    pub name: Cow<'src, str>,
    pub flags: wasmparser::SymbolFlags,
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct Dylink0Section<'src> {
    pub memory_size: u32,
    pub memory_alignment: u32,
    pub table_size: u32,
    pub table_alignment: u32,
    pub needed_libraries: Vec<Cow<'src, str>>,
    pub import_info: Vec<ImportInfo<'src>>,
}

impl<'src> Dylink0Section<'src> {
    const WASM_DYLINK_MEM_INFO: u8 = 1;
    const WASM_DYLINK_NEEDED: u8 = 2;
    // const WASM_DYLINK_EXPORT_INFO: u8 = 3;
    const WASM_DYLINK_IMPORT_INFO: u8 = 4;
    // const WASM_DYLINK_RUNTIME_PATH: u8 = 5;

    #[cfg(feature = "encoder")]
    pub fn encode_section(&self) -> Vec<u8> {
        let mut encoded_bytes = Vec::new();
        // Memory info subsection
        {
            encoded_bytes.push(Self::WASM_DYLINK_MEM_INFO);

            let mut payload = Vec::new();
            self.memory_size.encode(&mut payload);
            self.memory_alignment.encode(&mut payload);
            self.table_size.encode(&mut payload);
            self.table_alignment.encode(&mut payload);

            payload.len().encode(&mut encoded_bytes);
            encoded_bytes.extend(payload);
        }

        // Needed libraries subsection
        if !self.needed_libraries.is_empty() {
            encoded_bytes.push(Self::WASM_DYLINK_NEEDED);
            let mut needed_payload = Vec::new();
            self.needed_libraries.len().encode(&mut needed_payload);
            for lib in &self.needed_libraries {
                lib.encode(&mut needed_payload);
            }
            needed_payload.len().encode(&mut encoded_bytes);
            encoded_bytes.extend(needed_payload);
        }
        // Import info subsection
        if !self.import_info.is_empty() {
            encoded_bytes.push(Self::WASM_DYLINK_IMPORT_INFO);
            let mut import_payload = Vec::new();
            self.import_info.len().encode(&mut import_payload);
            for import in &self.import_info {
                import.module.encode(&mut import_payload);
                import.name.encode(&mut import_payload);
                import.flags.bits().encode(&mut import_payload);
            }
            import_payload.len().encode(&mut encoded_bytes);
            encoded_bytes.extend(import_payload);
        }
        encoded_bytes
    }

    #[cfg(feature = "parser")]
    pub fn from_reader(mut reader: Dylink0SectionReader<'src>) -> Result<Self> {
        let mut this = Dylink0Section::default();
        while let Some(subsection) = reader.next() {
            match subsection? {
                Dylink0Subsection::MemInfo(mem_info) => {
                    this.memory_size = mem_info.memory_size;
                    this.memory_alignment = mem_info.memory_alignment;
                    this.table_size = mem_info.table_size;
                    this.table_alignment = mem_info.table_alignment;
                }
                Dylink0Subsection::Needed(needed) => {
                    this.needed_libraries = needed.into_iter().map(|s| Cow::Borrowed(s)).collect();
                }

                Dylink0Subsection::ImportInfo(imports) => {
                    this.import_info = imports
                        .into_iter()
                        .map(|imp| ImportInfo {
                            module: Cow::Borrowed(imp.module),
                            name: Cow::Borrowed(imp.field),
                            flags: imp.flags,
                        })
                        .collect();
                }
                Dylink0Subsection::RuntimePath(_) => {
                    bail!("RuntimePath subsection is not supported");
                }
                Dylink0Subsection::ExportInfo(exports) => {
                    bail!("ExportInfo subsection is not supported: {exports:?}");
                }
                Dylink0Subsection::Unknown { .. } => {
                    bail!("Unknown subsection is not supported");
                }
            }
        }
        Ok(this)
    }
}

#[cfg(test)]
#[cfg(feature = "encoder")]
#[cfg(feature = "parser")]
mod tests {
    use wasmparser::BinaryReader;

    use super::{Dylink0Section, ImportInfo};

    fn test_data() -> Dylink0Section<'static> {
        Dylink0Section {
            memory_size: 65536,
            memory_alignment: 16,
            table_size: 10,
            table_alignment: 4,
            needed_libraries: vec!["libfoo.so".into(), "libbar.so".into()],
            import_info: vec![
                ImportInfo {
                    module: "env".to_string().into(),
                    name: "malloc".to_string().into(),
                    flags: wasmparser::SymbolFlags::from_name("EXPORTED").unwrap(),
                },
                ImportInfo {
                    module: "env".to_string().into(),
                    name: "free".to_string().into(),
                    flags: wasmparser::SymbolFlags::from_name("EXPLICIT_NAME").unwrap(),
                },
            ],
        }
    }

    #[test]
    fn test_encode_decode() {
        let test_data = test_data();

        let encoded = test_data.encode_section();
        let decoded = wasmparser::Dylink0SectionReader::new(BinaryReader::new(&encoded, 0));

        let roundtrip = Dylink0Section::from_reader(decoded).unwrap();
        assert_eq!(test_data, roundtrip);
    }
}
