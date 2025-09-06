//! This module contains serialization and deserialization of the metadata
//! that is used by wasm-split to store information about resulting modules and linkage between them.
//!
//! This metadata is used for linker to determine if module compatible with other modules.
//!
//!
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Debug,
    str::FromStr,
};

use anyhow::Result;
use serde::{ser::SerializeStruct, Deserialize, Serialize};
pub use wamex_metadata::{
    BumpVersion, DemangledName, ExportedSymbol, Module, SymbolSignature, Type,
};

use crate::{
    analysis::{
        dep_graph::DepNode,
        split_point::{SplitModuleIdentifier, SplitProgramInfo},
        ModuleInfo,
    },
    emit::{DataSegment, EmitInfo, ModuleEmitState, SymbolRelation},
    helpers::Hash,
    index::{DataSegmentId, DataSymbolId, IdVec, InputFuncId},
    ModuleStructure,
};

pub trait SymbolSignatureExt {
    fn from_input_function(module_info: &ModuleInfo, func_id: InputFuncId, lazy: bool) -> Self;
    fn from_data_symbol(
        module_info: &ModuleInfo,
        segment: DataSegmentId,
        idx: DataSymbolId,
    ) -> Self;
    fn from_node(module_info: &ModuleInfo, node: &crate::analysis::dep_graph::DepNode) -> Self;
}
impl SymbolSignatureExt for SymbolSignature {
    fn from_input_function(module_info: &ModuleInfo, func_id: InputFuncId, lazy: bool) -> Self {
        let name = module_info
            .wasm
            .names
            .functions
            .get(func_id)
            .expect("Function name should be defined");

        let funct_type_id = module_info.get_function_type_id(func_id);

        let type_info = module_info
            .wasm
            .types
            .get(funct_type_id)
            .expect("Function type should be defined");
        let params = type_info
            .params()
            .into_iter()
            .map(Type::try_from)
            .collect::<Result<Vec<_>, _>>()
            .expect("Failed to convert function parameter types");
        let results = type_info
            .results()
            .into_iter()
            .map(Type::try_from)
            .collect::<Result<Vec<_>, _>>()
            .expect("Failed to convert function result types");

        SymbolSignature::Function {
            name: DemangledName::new(name, false),
            lazy,
            params,  // Populate with actual parameter types if available
            results, // Populate with actual result type if available
        }
    }

    fn from_data_symbol(
        module_info: &ModuleInfo,
        segment: DataSegmentId,
        idx: DataSymbolId,
    ) -> Self {
        let symbol = module_info
            .wasm
            .linking
            .get_data_in_segment(segment, idx)
            .expect("Data symbol should be defined");

        SymbolSignature::Data {
            size: symbol.size,
            name: DemangledName::new(symbol.name, true),
        }
    }

    fn from_node(module_info: &ModuleInfo, node: &crate::analysis::dep_graph::DepNode) -> Self {
        match node {
            crate::analysis::dep_graph::DepNode::Function(func_id) => {
                Self::from_input_function(module_info, *func_id, false)
            }
            crate::analysis::dep_graph::DepNode::DataSymbol(segment, idx) => {
                Self::from_data_symbol(module_info, *segment, *idx)
            }
        }
    }
}

pub trait ModuleExt {
    fn build_modules_metadata(
        module_info: &ModuleInfo,
        program_info: &SplitProgramInfo,
        module_structure: ModuleStructure,
    ) -> BTreeMap<String, Module>;
}
impl ModuleExt for Module {
    fn build_modules_metadata(
        module_info: &ModuleInfo,
        program_info: &SplitProgramInfo,
        module_structure: ModuleStructure,
    ) -> BTreeMap<String, Module> {
        let modules_that_exports: BTreeMap<DepNode, SplitModuleIdentifier> = program_info
            .output_modules
            .iter()
            .fold(BTreeMap::new(), |mut acc, (name, deps)| {
                if name.is_shared()
                    || (module_structure != ModuleStructure::EmitMainChunked && name.is_main())
                {
                    for dep in &deps.link_symbols {
                        let prev = acc.insert(dep.clone(), name.clone());
                        assert!(
                            prev.is_none(),
                            "Duplicate export for {dep:?} in {name:?}, previous was {prev:?}",
                        );
                    }
                }
                acc
            });

        let mut metadata = BTreeMap::new();

        for (name, split_deps) in &program_info.output_modules {
            let module_name = name.name();
            let version = BumpVersion::new();
            let mut provides = Vec::new();
            let mut deps: BTreeMap<String, Vec<ExportedSymbol>> = BTreeMap::new();

            // Split points are exported
            for split_point in &split_deps.split_points {
                provides.push(ExportedSymbol {
                    signature: SymbolSignature::from_input_function(
                        module_info,
                        split_point.import_func,
                        true, // all split points are lazy-loading by default
                    ),
                    version: BumpVersion::new(),
                });
            }
            for node in &split_deps.link_symbols {
                let exported_symbol = ExportedSymbol {
                    signature: SymbolSignature::from_node(module_info, node),
                    version: BumpVersion::new(),
                };
                match name {
                    SplitModuleIdentifier::Shared(_) => provides.push(exported_symbol),
                    name @ SplitModuleIdentifier::Single(_)
                        if module_structure != ModuleStructure::EmitMainChunked
                            && name.is_main() =>
                    {
                        provides.push(exported_symbol);
                    }
                    SplitModuleIdentifier::Single(_) => {
                        let import_module = modules_that_exports
                            .get(node)
                            .expect("No exporting module found");
                        deps.entry(import_module.name())
                            .or_default()
                            .push(exported_symbol);
                    }
                }
            }

            metadata.insert(
                module_name,
                Module {
                    version,
                    provides,
                    deps,
                },
            );
        }

        metadata
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

pub fn _build_module_structure(module: &ModuleInfo) -> crate::diff::symbols_map::ModuleStructure {
    let all_relocations =
        EmitInfo::all_relocations(module.wasm).expect("Failed to get all relocations");

    // TODO: reuse from emit modules
    let data_segments_symbols = module
        .data_symbols
        .chunk_by(|left, right| left.segment_index == right.segment_index)
        .collect::<Vec<_>>();
    let data_segments = module
        .wasm
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
            let segment_info = module.wasm.linking.segments_info[data_segment].clone();

            DataSegment::new_inner(data.clone(), segment_info, data_symbols, data_relocs)
        })
        .collect::<Result<IdVec<_>>>()
        .unwrap();

    let mut print_data_fromat = String::new();
    for (i, segment) in data_segments.iter() {
        for symbol in segment._data_symbols_iter() {
            let (chunk_hex, chunk_utf8) = match symbol.symbol_relation() {
                SymbolRelation::Regular { chunk, .. } => (
                    hex::encode(chunk),
                    String::from_utf8_lossy(chunk).to_string(),
                ),
                SymbolRelation::BoundToPrevious { .. } => {
                    ("<bound to previous>".to_string(), "".to_string())
                }
            };
            print_data_fromat.push_str(&format!(
                "Data symbol {i}.{index}: {name} [{chunk_hex}] [{chunk_utf8}]\n",
                i = i,
                index = symbol.index(),
                name = symbol.name(),
                chunk_utf8 = chunk_utf8.escape_debug(),
            ));
        }
    }
    log::warn!("Data segments: {print_data_fromat}");

    let registry = crate::diff::symbols_map::ModuleStructure::new_from_module(
        module,
        &all_relocations,
        &data_segments,
    );

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
