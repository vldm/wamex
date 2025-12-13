use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::{Display, Debug},
};

use wamex_types::map_vec::MiniSet;

use crate::{
    index::{ExportId, IdMap, ImportId, InputFuncId, SymbolId},
};

pub const WAMEX_ENTRY_PREFIX: &str = "__wamex_00";
pub const SPLIT_IMPORT_POSTFIX: &str = "00_import_";
pub const SPLIT_EXPORT_POSTFIX: &str = "00_export_";

pub fn parser<'a>(name: &'a str, prefix: &str, postfix: &str) -> Option<(&'a str, &'a str)> {
    if !name.starts_with(prefix) {
        return None;
    }
    let name = &name[prefix.len()..];
    let postfix_index = name.find(postfix)?;
    let module_name = &name[..postfix_index];
    let fn_name = &name[postfix_index + postfix.len()..];

    Some((module_name, fn_name))
}

pub fn parse_wamex_entry_name(name: &str) -> Option<(&str, &str)> {
    if let Some(v) = parser(name, WAMEX_ENTRY_PREFIX, SPLIT_IMPORT_POSTFIX) {
        return Some(v);
    }
    parser(name, WAMEX_ENTRY_PREFIX, SPLIT_EXPORT_POSTFIX)
}

/// Split-point entrypoint pair (import stub + export impl).
///
/// Note: the algorithm that discovers split points lives in `wamex-cli`.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct SplitPoint {
    pub module_name: String,
    pub unique_id: String,
    pub import: ImportId,
    pub import_func: InputFuncId,
    pub export: ExportId,
    pub export_func: InputFuncId,
}

impl SplitPoint {
    pub fn import_func(&self) -> InputFuncId {
        self.import_func
    }
    pub fn export_func(&self) -> InputFuncId {
        self.export_func
    }
}

/// Content plan for a single emitted module.
#[derive(Debug, Default, Clone)]
pub struct OutputModuleInfo {
    pub defined_symbols: BTreeSet<SymbolId>,
    pub imports: MiniSet<SymbolId>,
    pub exports: MiniSet<SymbolId>,
    pub split_points: Vec<SplitPoint>,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone)]
pub enum ModuleIdentifier {
    Main,
    Split(String),
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone)]
pub struct SharedModuleIdentifier(pub Vec<ModuleIdentifier>);

impl Display for SharedModuleIdentifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut components = self.0.iter();
        let Some(first) = components.next() else {
            return Ok(());
        };

        write!(f, "{}", first)?;
        components.try_for_each(|n| write!(f, "_{}", n))?;
        Ok(())
    }
}

impl SharedModuleIdentifier {
    pub fn contains(&self, module: &ModuleIdentifier) -> bool {
        self.0.iter().any(|m| m == module)
    }

    pub fn remove(&mut self, module: &ModuleIdentifier) -> bool {
        let original_len = self.0.len();
        self.0.retain(|m| m != module);
        original_len != self.0.len()
    }

    pub fn includes(&self, other: &SplitModuleIdentifier) -> bool {
        match other {
            SplitModuleIdentifier::Single(name) => self.contains(name),
            SplitModuleIdentifier::Shared(shared) => shared.0.iter().all(|name| self.contains(name)),
        }
    }
}

impl PartialEq<SplitModuleIdentifier> for SharedModuleIdentifier {
    fn eq(&self, other: &SplitModuleIdentifier) -> bool {
        match other {
            SplitModuleIdentifier::Single(_) => false,
            SplitModuleIdentifier::Shared(shared) => shared == self,
        }
    }
}

impl<'a> IntoIterator for &'a SharedModuleIdentifier {
    type Item = &'a ModuleIdentifier;
    type IntoIter = std::slice::Iter<'a, ModuleIdentifier>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone)]
pub enum SplitModuleIdentifier {
    Single(ModuleIdentifier),
    Shared(SharedModuleIdentifier),
}

impl Display for ModuleIdentifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Main => write!(f, "main"),
            Self::Split(name) => write!(f, "{}", name),
        }
    }
}

impl Display for SplitModuleIdentifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Single(name) => Display::fmt(name, f),
            Self::Shared(name) => Display::fmt(name, f),
        }
    }
}

impl SplitModuleIdentifier {
    pub fn as_single(&self) -> Option<&ModuleIdentifier> {
        match self {
            Self::Single(name) => Some(name),
            Self::Shared(_) => None,
        }
    }

    pub fn as_shared(&self) -> Option<&SharedModuleIdentifier> {
        match self {
            Self::Single(_) => None,
            Self::Shared(name) => Some(name),
        }
    }

    pub fn is_shared(&self) -> bool {
        matches!(self, Self::Shared(_))
    }

    pub fn is_main(&self) -> bool {
        matches!(self, Self::Single(ModuleIdentifier::Main))
    }

    pub fn is_part_of(&self, other: &SharedModuleIdentifier) -> bool {
        match self {
            Self::Single(name) => other.contains(name),
            Self::Shared(shared) => shared.0.iter().all(|name| other.contains(name)),
        }
    }

    pub fn collect_deps(&self, shared_modules: &[SharedModuleIdentifier]) -> Vec<SharedModuleIdentifier> {
        let mut result = Vec::new();
        for shared_module in shared_modules {
            if matches!(&self, SplitModuleIdentifier::Shared(our_module) if shared_module == our_module) {
                continue;
            }
            if self.is_part_of(shared_module) {
                result.push(shared_module.clone());
            }
        }
        result
    }
}

/// Emission plan for a full split program.
///
/// Note: the algorithm that computes this structure lives in `wamex-cli`.
#[derive(Debug, Default)]
pub struct SplitProgramInfo {
    pub output_modules: Vec<(SplitModuleIdentifier, OutputModuleInfo)>,
    pub symbol_output_module: IdMap<SymbolId, usize>,
}

/// Helpers used by `wamex-cli` diff/incremental logic.
#[derive(Clone, Debug)]
pub struct ModuleSnapshot {
    pub defined_symbols: BTreeSet<SymbolId>,
    pub imports: MiniSet<SymbolId>,
    pub exports: MiniSet<SymbolId>,
    pub split_points: Vec<SplitPoint>,
}

impl From<&OutputModuleInfo> for ModuleSnapshot {
    fn from(value: &OutputModuleInfo) -> Self {
        Self {
            defined_symbols: value.defined_symbols.clone(),
            imports: value.imports.clone(),
            exports: value.exports.clone(),
            split_points: value.split_points.clone(),
        }
    }
}

pub fn merge_split_points_by_module_name<'a>(split_points: &'a [SplitPoint]) -> BTreeMap<&'a str, Vec<&'a SplitPoint>> {
    let mut result = BTreeMap::<&'a str, Vec<&'a SplitPoint>>::new();
    for split_point in split_points {
        result.entry(split_point.module_name.as_str()).or_default().push(split_point);
    }
    for results in result.values_mut() {
        results.sort_by_key(|sp| sp.unique_id.as_str());
    }
    result
}
