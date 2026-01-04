//! Extension to differ to match split points by position in module.
//! This is needed to match wamex split points which have unique names based on span an changes in incremental builds.
//!
//! Currently unused, because even if matcher work correctly, it cannot detect "closures" and other rustc-generated functions.
mod cli;

use std::collections::BTreeMap;

use analysis::split_point::{
    SPLIT_EXPORT_POSTFIX, SPLIT_IMPORT_POSTFIX, WAMEX_ENTRY_PREFIX, parser,
};
pub use cli::Compare;
use wamex_object::symbols::{Differ, SymbolId, SymbolMapWithContent, SymbolMapping};

use crate::analysis;

#[allow(dead_code)]
type SVec<T> = smallvec::SmallVec<[T; 4]>;
#[allow(dead_code)]
pub trait DifferExt {
    // Try to match unmatched wamex split points by their module name and function position.
    fn try_match_wamex_split_point(&self, mapping: &mut SymbolMapping);
}
impl<'left, 'right, L, R> DifferExt for Differ<L, R>
where
    L: SymbolMapWithContent<'left>,
    R: SymbolMapWithContent<'right>,
{
    fn try_match_wamex_split_point(&self, mapping: &mut SymbolMapping) {
        // To avoid conflicts wamex entrypoints contain unique portion in their names.
        // So we match them by module name.
        let mut non_matched_wamex_left_symbols: BTreeMap<&str, SVec<(&str, SymbolId)>> =
            BTreeMap::new();
        let mut non_matched_wamex_right_symbols: BTreeMap<&str, SVec<(&str, SymbolId)>> =
            BTreeMap::new();
        for left in &mapping.left_non_matched {
            let left_symbol = &self.left.symbols().get(*left).unwrap();
            let Some(name) = &left_symbol.linking_name else {
                continue;
            };
            if !name.contains(WAMEX_ENTRY_PREFIX) {
                continue;
            }
            let Some((module, fn_name)) = wamex_parse_name(name) else {
                continue;
            };
            non_matched_wamex_left_symbols
                .entry(module)
                .or_default()
                .push((fn_name, *left));
        }
        for right in &mapping.right_non_matched {
            let right_symbol = &self.right.symbols().get(*right).unwrap();
            let Some(name) = &right_symbol.linking_name else {
                continue;
            };
            if !name.contains(WAMEX_ENTRY_PREFIX) {
                continue;
            }
            let Some((module, fn_name)) = wamex_parse_name(name) else {
                continue;
            };
            non_matched_wamex_right_symbols
                .entry(module)
                .or_default()
                .push((fn_name, *right));
        }
        // Now match left and right symbols within same module by function number
        for (module, mut left_syms) in non_matched_wamex_left_symbols {
            let Some(mut right_syms) = non_matched_wamex_right_symbols.remove(module) else {
                continue;
            };
            left_syms.sort_by_key(|(fn_name, _)| *fn_name);
            right_syms.sort_by_key(|(fn_name, _)| *fn_name);
            for (left, right) in left_syms.into_iter().zip(right_syms.into_iter()) {
                log::info!(
                    "Matched wamex split point symbol: module: {module}, left: {:?}, right: {:?}",
                    left.0,
                    right.0
                );
                mapping.left_to_right.insert(left.1, right.1);
                // Remove from non-matched lists
                mapping.left_non_matched.retain(|v| *v != left.1);
                mapping.right_non_matched.retain(|v| *v != right.1);
            }
        }
    }
}

fn wamex_parse_name(name: &str) -> Option<(&str, &str)> {
    parse_wamex_entry_name(name)
}

pub fn parse_wamex_entry_name(name: &str) -> Option<(&str, &str)> {
    if let Some(v) = parser(name, WAMEX_ENTRY_PREFIX, SPLIT_IMPORT_POSTFIX) {
        return Some(v);
    }
    parser(name, WAMEX_ENTRY_PREFIX, SPLIT_EXPORT_POSTFIX)
}
