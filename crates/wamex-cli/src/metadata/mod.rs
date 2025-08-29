use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Debug,
    str::FromStr,
};

use anyhow::Result;
use serde::{ser::SerializeStruct, Deserialize, Serialize};

use crate::{
    analysis::{split_point::SplitProgramInfo, ModuleInfo},
    emit::{DataSegment, EmitInfo, ModuleEmitState, SymbolRelation},
    index::IdVec,
    metadata::metadata_ext::{Module, ModuleExt, SymbolSignature},
    ModuleStructure,
};

mod metadata_ext;
pub mod uniq_symbols;

// Snapshot recovery functionality.
#[derive(Default, Copy, Clone, PartialOrd, Ord, PartialEq, Eq, Hash)]
pub struct Hash(u128);

impl Hash {
    const SEED: i64 = 0;
    pub fn from_hashable<H: std::hash::Hash>(value: &H) -> Self {
        let mut hasher = gxhash::GxHasher::with_seed(Self::SEED);
        value.hash(&mut hasher);
        Hash(hasher.finish_u128())
    }

    pub fn hash_bytes(data: &[u8]) -> Hash {
        Hash(gxhash::gxhash128(data, Self::SEED))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Snapshot {
    // Vec<(signature, hash)> of the symbol
    pub symbols: Vec<(SymbolSignature, Hash)>,
    // Map<ID, Vec<ID>> of dependencies, where ID is index in the `symbols` array
    pub deps: BTreeMap<usize, BTreeSet<usize>>,
}
impl Serialize for Snapshot {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("Snapshot", 2)?;
        state.serialize_field(
            "symbols",
            &self
                .symbols
                .iter()
                .map(|(sig, hash)| (sig.display_signature(), format!("{:x}", hash.0)))
                .collect::<Vec<_>>(),
        )?;
        state.serialize_field("deps", &self.deps)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for Snapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct SnapshotHelper {
            symbols: Vec<(String, String)>,
            deps: BTreeMap<usize, BTreeSet<usize>>,
        }
        let helper = SnapshotHelper::deserialize(deserializer)?;
        let symbols = helper
            .symbols
            .into_iter()
            .map(|(sig_str, hash_str)| {
                let sig = FromStr::from_str(&sig_str)
                    .map_err(|_| serde::de::Error::custom("Invalid symbol signature"))?;
                let hash = u128::from_str_radix(&hash_str, 16)
                    .map_err(|_| serde::de::Error::custom("Invalid hash format"))?;
                Ok((sig, Hash(hash)))
            })
            .collect::<Result<Vec<_>, D::Error>>()?;
        Ok(Snapshot {
            symbols,
            deps: helper.deps,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Metadata {
    pub snapshot: Snapshot,
    #[serde(flatten)]
    pub modules: BTreeMap<String, Module>,
}

pub fn _build_module_structure(module: &ModuleInfo) -> uniq_symbols::ModuleStructure {
    let all_relocations =
        EmitInfo::all_relocations(module.source).expect("Failed to get all relocations");

    // TODO: reuse from emit modules
    let data_segments_symbols = module
        .data_symbols
        .chunk_by(|left, right| left.segment_index == right.segment_index)
        .collect::<Vec<_>>();
    let data_segments = module
        .source
        .data
        .section_payload
        .data_segments
        .iter()
        .enumerate()
        .map(|(data_segment, data)| {
            let data_relocs =
                ModuleEmitState::get_relocations_for_range(&all_relocations, &data.range);
            let data_symbols = data_segments_symbols
                .get(data_segment)
                .cloned()
                .expect("Symbols for data segment not found");
            let segment_info = module.source.linking.segments_info[data_segment].clone();

            DataSegment::new_inner(data.clone(), segment_info, data_symbols, data_relocs)
        })
        .collect::<Result<IdVec<_>>>()
        .unwrap();

    let mut print_data_fromat = String::new();
    for (i, segment) in data_segments.iter() {
        for symbol in segment._data_symbols_iter() {
            let chunk = match symbol.symbol_relation() {
                SymbolRelation::Regular { chunk, .. } => hex::encode(chunk),
                SymbolRelation::BoundToPrevious { .. } => "<bound to previous>".to_string(),
            };
            print_data_fromat.push_str(&format!(
                "Data symbol {i}.{index}: {name} [{chunk}]\n",
                i = i,
                index = symbol.index(),
                name = symbol.name(),
            ));
        }
    }
    log::warn!("Data segments: {print_data_fromat}");

    let registry =
        uniq_symbols::ModuleStructure::new_from_module(module, &all_relocations, &data_segments);

    registry
}

pub fn build_metadata_and_snapshot(
    module: &ModuleInfo,
    program_info: &SplitProgramInfo,
    module_structure: ModuleStructure,
) -> Metadata {
    let modules = Module::build_modules_metadata(module, program_info, module_structure);
    assert!(
        modules.get("snapshot").is_none(),
        "field 'snapshot' is reserved, and not allowed as module name"
    );

    let registry = _build_module_structure(module);
    Metadata {
        snapshot: registry.structure.snapshot(),
        modules,
    }
}

impl Debug for Hash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:x}", self.0)
    }
}

impl Serialize for Hash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&format!("{:x}", self.0))
    }
}
impl<'de> Deserialize<'de> for Hash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let value = u128::from_str_radix(&s, 16).map_err(serde::de::Error::custom)?;
        Ok(Hash(value))
    }
}
