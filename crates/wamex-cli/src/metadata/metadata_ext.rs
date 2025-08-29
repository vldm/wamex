//! This module contains serialization and deserialization of the metadata
//! that is used by wasm-split to store information about resulting modules and linkage between them.
//!
//! This metadata is used for linker to determine if module compatible with other modules.
//!
//!

use std::{collections::BTreeMap, fmt::Debug, hash::Hasher, str::FromStr};

use anyhow::Result;
use serde::{Deserialize, Serialize};
pub use wamex_metadata::{
    BumpVersion, DemangledName, ExportedSymbol, Module, SymbolSignature, Type,
};
use wasmparser::ValType;

use crate::{
    analysis::{
        dep_graph::DepNode,
        split_point::{SplitModuleIdentifier, SplitProgramInfo},
        ModuleInfo,
    },
    index::{DataSegmentId, DataSymbolId, InputFuncId},
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
            .source
            .names
            .functions
            .get(func_id)
            .expect("Function name should be defined");

        let funct_type_id = module_info.get_function_type_id(func_id);

        let type_info = module_info
            .source
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
            .source
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
