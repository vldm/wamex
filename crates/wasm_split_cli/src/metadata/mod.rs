use std::collections::{BTreeMap, BTreeSet};

use serde::{ser::SerializeMap, Deserialize, Serialize};
use wasmparser::ValType;

use crate::{
    analysis::{
        dep_graph::DepNode,
        split_point::{SplitModuleIdentifier, SplitProgramInfo},
        ModuleInfo,
    },
    helpers::demangle_name,
    index::{DataSegmentId, DataSymbolId, InputFuncId},
    ModuleStructure,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExportedSymbol {
    name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    distinguishing_hash: String,
    version: BumpVersion,
    #[serde(flatten)]
    signature: SymbolSignature,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SymbolSignature {
    Function {
        // If lazy is true the module should be loaded before calling the function.
        // This also means that it cannot be directly "imported" by other modules.
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        lazy: bool,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        params: Vec<Type>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        results: Vec<Type>,
    },
    Data {
        size: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Type {
    I32,
    I64,
    F32,
    F64,
    V128,
    FuncRef,
    ExternRef,
    // Add other types as needed
    // Like structs, arrays, etc.
}

impl TryFrom<&ValType> for Type {
    type Error = &'static str;

    fn try_from(val_type: &ValType) -> Result<Self, Self::Error> {
        match val_type {
            ValType::I32 => Ok(Type::I32),
            ValType::I64 => Ok(Type::I64),
            ValType::F32 => Ok(Type::F32),
            ValType::F64 => Ok(Type::F64),
            ValType::V128 => Ok(Type::V128),
            ValType::Ref(r) if r.is_extern_ref() => Ok(Type::ExternRef),
            ValType::Ref(r) if r.is_func_ref() => Ok(Type::FuncRef),
            _ => Err("Unsupported ValType for conversion to Type"),
        }
    }
}

impl ExportedSymbol {
    pub fn from_input_function(
        module_info: &ModuleInfo,
        func_id: InputFuncId,
        lazy: bool,
        version: BumpVersion,
    ) -> Self {
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

        let (name, distinguishing_hash) = demangle_name(name);
        ExportedSymbol {
            name,
            distinguishing_hash,
            version,
            signature: SymbolSignature::Function {
                lazy,
                params,  // Populate with actual parameter types if available
                results, // Populate with actual result type if available
            },
        }
    }

    fn from_data_symbol(
        module_info: &ModuleInfo,
        segment: DataSegmentId,
        idx: DataSymbolId,
        version: BumpVersion,
    ) -> Self {
        let symbol = module_info
            .source
            .linking
            .get_data_in_segment(segment, idx)
            .expect("Data symbol should be defined");

        let (name, distinguishing_hash) = demangle_name(&symbol.name);
        ExportedSymbol {
            name,
            distinguishing_hash,
            version,
            signature: SymbolSignature::Data { size: symbol.size },
        }
    }

    fn from_node(
        module_info: &ModuleInfo,
        node: &crate::analysis::dep_graph::DepNode,
        version: BumpVersion,
    ) -> Self {
        match node {
            crate::analysis::dep_graph::DepNode::Function(func_id) => {
                ExportedSymbol::from_input_function(module_info, *func_id, false, version)
            }
            crate::analysis::dep_graph::DepNode::DataSymbol(segment, idx) => {
                ExportedSymbol::from_data_symbol(module_info, *segment, *idx, version)
            }
        }
    }
}

pub fn build_metadata(
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
            provides.push(ExportedSymbol::from_input_function(
                module_info,
                split_point.import_func,
                true, // all split points are lazy-loading by default
                BumpVersion::new(),
            ));
        }
        for node in &split_deps.link_symbols {
            let exported_symbol = ExportedSymbol::from_node(module_info, node, BumpVersion::new());
            match name {
                SplitModuleIdentifier::Shared(_) => provides.push(exported_symbol),
                name @ SplitModuleIdentifier::Single(_)
                    if module_structure != ModuleStructure::EmitMainChunked && name.is_main() =>
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

// Snapshot recovery functionality.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Hash(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct VecAsMap<T, U> {
    map: Vec<(T, U)>,
}

impl<T, U> Serialize for VecAsMap<T, U>
where
    T: Serialize,
    U: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut serializer = serializer.serialize_map(Some(self.map.len()))?;
        for (key, value) in &self.map {
            serializer.serialize_entry(key, value)?;
        }
        serializer.end()
    }
}
impl<'a, T, U> Deserialize<'a> for VecAsMap<T, U>
where
    T: Deserialize<'a>,
    U: Deserialize<'a>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'a>,
    {
        struct MapVisitor<T, U>(std::marker::PhantomData<(T, U)>);

        impl<'de, T, U> serde::de::Visitor<'de> for MapVisitor<T, U>
        where
            T: Deserialize<'de>,
            U: Deserialize<'de>,
        {
            type Value = VecAsMap<T, U>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a map")
            }

            fn visit_map<V>(self, mut map: V) -> Result<Self::Value, V::Error>
            where
                V: serde::de::MapAccess<'de>,
            {
                let mut vec = Vec::new();
                while let Some((key, value)) = map.next_entry()? {
                    vec.push((key, value));
                }
                Ok(VecAsMap { map: vec })
            }
        }

        deserializer.deserialize_map(MapVisitor::<T, U>(std::marker::PhantomData))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Snapshot {
    // Vec<(name, hash)> of the symbol
    symbols: VecAsMap<String, Hash>,
    // Map<ID, Vec<ID>> of dependencies, where ID is index in the `symbols` array
    deps: BTreeMap<usize, BTreeSet<usize>>,
}

/// Unique identifier of content.
/// This is generated by `split` command and used to distinguish different versions of the same content.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BumpVersion {
    version: u32,
}
impl BumpVersion {
    pub fn new() -> Self {
        BumpVersion { version: 0 }
    }

    pub fn bump(&mut self) {
        self.version += 1;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Module {
    version: BumpVersion,
    provides: Vec<ExportedSymbol>,
    deps: BTreeMap<String, Vec<ExportedSymbol>>,
}
