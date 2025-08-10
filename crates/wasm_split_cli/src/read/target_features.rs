use anyhow::{bail, Result};
use wasm_encoder::Encode;
use wasmparser::{WasmFeatures, WasmFeaturesInflated};

use crate::read::CustomSectionReader;

pub struct TargetFeatures {
    pub features: WasmFeaturesInflated,
}

impl TargetFeatures {
    pub fn encode_custom_section(&self) -> wasm_encoder::CustomSection<'static> {
        let data = self.feature_bytes();
        wasm_encoder::CustomSection {
            name: "target_features".into(),
            data: data.into(),
        }
    }
    fn feature_bytes(&self) -> Vec<u8> {
        let mut sink = Vec::new();
        let mut count = 0;

        macro_rules! push_feature {
            ( $feature:ident, $name:literal) => {
                if self.features.$feature {
                    count += 1;
                    sink.push(b'+');
                    $name.encode(&mut sink);
                }
            };
        }

        push_feature!(mutable_global, "mutable-globals");
        push_feature!(multi_value, "multivalue");
        push_feature!(sign_extension, "sign-ext");
        push_feature!(extended_const, "extended-const");
        push_feature!(reference_types, "reference-types");
        push_feature!(saturating_float_to_int, "nontrapping-fptoint");
        push_feature!(bulk_memory, "bulk-memory");
        push_feature!(bulk_memory_opt, "bulk-memory-opt");
        push_feature!(call_indirect_overlong, "call-indirect-overlong");

        let mut result = Vec::new();
        (count as u32).encode(&mut result);
        result.append(&mut sink);
        result
    }
}

impl Default for TargetFeatures {
    fn default() -> Self {
        Self {
            features: WasmFeaturesInflated::from(WasmFeatures::empty()),
        }
    }
}
impl Clone for TargetFeatures {
    fn clone(&self) -> Self {
        let mut new = Self::default();

        new.features.mutable_global = self.features.mutable_global;
        new.features.multi_value = self.features.multi_value;
        new.features.sign_extension = self.features.sign_extension;
        new.features.extended_const = self.features.extended_const;
        new.features.reference_types = self.features.reference_types;

        new.features.saturating_float_to_int = self.features.saturating_float_to_int;
        new.features.bulk_memory = self.features.bulk_memory;
        new.features.bulk_memory_opt = self.features.bulk_memory_opt;
        new.features.call_indirect_overlong = self.features.call_indirect_overlong;

        new
    }
}

impl<'a> CustomSectionReader<'a> for TargetFeatures {
    type Reader = wasmparser::BinaryReader<'a>;

    fn read(mut reader: Self::Reader) -> Result<Self> {
        let mut this = Self::default();
        let count = reader.read_var_u32()?;

        for _ in 0..count {
            let sym = reader.read_u8()? == b'+';
            let val = reader.read_string()?;
            match val {
                "mutable-globals" => this.features.mutable_global = sym,
                "multivalue" => this.features.multi_value = sym,
                "sign-ext" => this.features.sign_extension = sym,
                "extended-const" => this.features.extended_const = sym,
                "reference-types" => this.features.reference_types = sym,
                "bulk-memory" => this.features.bulk_memory = sym,
                "bulk-memory-opt" => this.features.bulk_memory_opt = sym,
                "call-indirect-overlong" => this.features.call_indirect_overlong = sym,
                "nontrapping-fptoint" => this.features.saturating_float_to_int = sym,
                // I haven't found convention of feature names.
                // So only used ones that produce cargo
                rest => bail!("Unknown target feature: {} = {}", rest, sym),
            }
        }
        Ok(this)
    }
}
