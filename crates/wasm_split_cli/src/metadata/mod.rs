use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Debug,
};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{
    analysis::{split_point::SplitProgramInfo, ModuleInfo},
    emit::{DataSegment, EmitInfo, ModuleEmitState, SymbolRelation},
    index::IdVec,
    metadata::linker_metadata::{Module, SymbolSignature},
    ModuleStructure,
};

mod graph_utils;
mod linker_metadata;
mod uniq;

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

#[derive(Debug, Clone, PartialOrd, Ord, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Snapshot {
    // Vec<(signature, hash)> of the symbol
    pub symbols: Vec<(SymbolSignature, Hash)>,
    // Map<ID, Vec<ID>> of dependencies, where ID is index in the `symbols` array
    pub deps: BTreeMap<usize, BTreeSet<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Metadata {
    pub snapshot: Snapshot,
    #[serde(flatten)]
    pub modules: BTreeMap<String, Module>,
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
        for (j, symbol) in segment._data_symbols_iter().enumerate() {
            let chunk = match symbol.symbol_relation() {
                SymbolRelation::Regular { chunk, .. } => hex::encode(chunk),
                SymbolRelation::BoundToPrevious { .. } => "<bound to previous>".to_string(),
            };
            print_data_fromat.push_str(&format!(
                "Data symbol {i}.{j}: {name} [{chunk}]\n",
                i = i,
                j = j,
                name = symbol.name(),
            ));
        }
    }
    log::warn!("Data segments: {print_data_fromat}");

    let mut registry =
        uniq::ModuleStructure::new_from_module(module, &all_relocations, &data_segments);

    registry.warn_dups(module);

    log::error!("Refining hashes...");
    registry.refine_hashes();

    registry.warn_dups(module);

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
