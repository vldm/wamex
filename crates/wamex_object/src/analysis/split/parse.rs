//! Parse related functionality.
//! Defines how to identify split points in input modules.

use std::{collections::BTreeMap, fmt::Debug};

use anyhow::Context;

use super::SplitPoint;
use crate::typed::{Module, snapshot::EntitiesSnapshot};
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum SplitPointExtractor {
    /// Use regexp and `_wasm_split_` prefix to identify split points.
    Legacy,
    /// Use `__wamex_` prefix and `.start_with` instead of regexp.
    Wamex,
}
pub(crate) fn parser<'a>(name: &'a str, prefix: &str, postfix: &str) -> Option<(&'a str, &'a str)> {
    if !name.starts_with(prefix) {
        return None;
    }
    let name = &name[prefix.len()..];
    let postfix_index = name.find(postfix)?;
    let module_name = &name[..postfix_index];
    let fn_name = &name[postfix_index + postfix.len()..];

    Some((module_name, fn_name))
}

fn parse_entries<'i, I, Id, V>(
    prefix: &str,
    postfix: &str,
    collection: I,
) -> BTreeMap<(String, String), Id>
where
    I: Iterator<Item = (Id, V)> + 'i,
    V: AsRef<str>,
{
    collection
        .filter_map(|(id, name)| {
            if let Some((module_name, unique_id)) = parser(name.as_ref(), prefix, postfix) {
                Some(((module_name.into(), unique_id.into()), id))
            } else {
                None
            }
        })
        .collect()
}

pub(crate) const SPLIT_IMPORT_POSTFIX: &str = "00_import_";
pub(crate) const SPLIT_EXPORT_POSTFIX: &str = "00_export_";

fn find_split_points_with_prefix(
    info: &Module,
    snapshot: &EntitiesSnapshot,
    prefix: &str,
) -> anyhow::Result<Vec<SplitPoint>> {
    let import_map = parse_entries(
        prefix,
        SPLIT_IMPORT_POSTFIX,
        info.functions
            .imports_iter()
            .map(|(i, import)| (i, &*import.name)),
    );
    let mut export_map = parse_entries(prefix, SPLIT_EXPORT_POSTFIX, info.functions.exports_iter());

    let split_points = import_map
        .into_iter()
        .map(|(key, import_func)| -> anyhow::Result<SplitPoint> {
            let export_func = export_map
                .remove(&key)
                .with_context(|| format!("No corresponding export for split import {key:?}"))?;
            let import_func = snapshot.pack_ref(import_func);
            let export_func = snapshot.pack_ref(export_func);

            Ok(SplitPoint::new(key.0, key.1, import_func, export_func))
        })
        .collect::<anyhow::Result<Vec<SplitPoint>>>()?;

    if let Some((key, _)) = export_map.iter().next() {
        log::error!(
            "No corresponding import for split export {key:?} hash {key_hash:?}. Maybe split module is defined but not used.",
            key_hash = key.1,
            key = key.0
        );
    }

    Ok(split_points)
}

pub fn find_split_points_legacy(
    info: &Module,
    snapshot: &EntitiesSnapshot,
) -> anyhow::Result<Vec<SplitPoint>> {
    find_split_points_with_prefix(info, snapshot, "__wasm_split_00")
}

pub(crate) const WAMEX_ENTRY_PREFIX: &str = "__wamex_00";

fn find_split_points_wamex(
    info: &Module,
    snapshot: &EntitiesSnapshot,
) -> anyhow::Result<Vec<SplitPoint>> {
    find_split_points_with_prefix(info, snapshot, WAMEX_ENTRY_PREFIX)
}

#[tracing::instrument(skip_all)]
pub fn find_split_points(
    info: &Module,
    snapshot: &EntitiesSnapshot,
    split_point_type: SplitPointExtractor,
) -> anyhow::Result<Vec<SplitPoint>> {
    match split_point_type {
        SplitPointExtractor::Legacy => find_split_points_legacy(info, snapshot),
        SplitPointExtractor::Wamex => find_split_points_wamex(info, snapshot),
    }
}
