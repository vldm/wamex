//! Integration tests for the split routine.
//!
//! Validates that the split pipeline produces valid, well-linked WASM modules:
//! - Each output is a structurally valid WASM binary.
//! - Main module exports `memory`.
//! - Submodules have PIC base imports (`__memory_base`, `__table_base`).
//! - Submodules import `memory`.
//! - Per-dependency PIC bases (`__file{N}___memory_base`, `__file{N}___table_base`) are present
//!   when a submodule depends on another split module.
//! - Every import (excluding runtime-provided ones) is satisfied by an export
//!   in some other output module.

use std::collections::{BTreeMap, BTreeSet};

use wamex_object::{ObjectReader, emit::split_routine_generic_test};

const EXAMPLE_WASM: &[u8] = include_bytes!("../../wamex-cli/test-data/example.wasm");
const LAZY_ROUTES: &[u8] = include_bytes!("../../wamex-cli/test-data/lazy_routes.wasm");

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct ParsedModule {
    name: String,
    imports: Vec<(String, String)>,
    exports: Vec<(String, wasmparser::ExternalKind)>,
}

fn run_split(src: &[u8]) -> Vec<(String, Vec<u8>)> {
    split_routine_generic_test(src).expect("split routine should succeed")
}

fn parse_module(name: &str, bytes: &[u8]) -> ParsedModule {
    let reader = ObjectReader::parse(bytes)
        .unwrap_or_else(|e| panic!("Failed to parse module '{name}': {e}"));
    let imports = reader
        .imports
        .iter()
        .map(|(_, i)| (i.module.to_owned(), i.name.to_owned()))
        .collect();
    let exports = reader
        .exports
        .iter()
        .map(|(_, e)| (e.name.to_owned(), e.kind))
        .collect();
    ParsedModule {
        name: name.to_owned(),
        imports,
        exports,
    }
}

fn export_names(m: &ParsedModule) -> BTreeSet<&str> {
    m.exports.iter().map(|(name, _)| name.as_str()).collect()
}

fn is_main(name: &str) -> bool {
    name == "main"
}

/// Well-known PIC globals/entities provided by the loader at runtime.
fn is_pic_base(name: &str) -> bool {
    name == "__memory_base"
        || name == "__table_base"
        || name == "__stack_pointer"
        || name.ends_with("___memory_base")
        || name.ends_with("___table_base")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

fn run_all_assertions(src: &[u8], fixture_name: &str) {
    let emitted = run_split(src);
    assert!(
        emitted.len() >= 2,
        "[{fixture_name}] Expected at least 2 modules (main + submodule), got {}",
        emitted.len()
    );

    // 1. Validate each module is structurally parseable.
    for (id, bytes) in &emitted {
        ObjectReader::parse(bytes)
            .unwrap_or_else(|e| panic!("[{fixture_name}/{id}] ObjectReader::parse failed: {e}"));
    }

    let modules: Vec<ParsedModule> = emitted
        .iter()
        .map(|(id, bytes)| parse_module(id, bytes))
        .collect();

    // 2. Every module has imports and exports.
    for m in &modules {
        assert!(
            !m.imports.is_empty(),
            "[{fixture_name}/{}] module has no imports",
            m.name
        );
        assert!(
            !m.exports.is_empty(),
            "[{fixture_name}/{}] module has no exports",
            m.name
        );
    }

    // 3. Main module exports memory.
    let main_module = modules
        .iter()
        .find(|m| is_main(&m.name))
        .unwrap_or_else(|| panic!("[{fixture_name}] No main module found"));

    let main_exports = export_names(main_module);
    assert!(
        main_exports.contains("memory"),
        "[{fixture_name}/main] Missing required export 'memory'. Exports: {main_exports:?}"
    );

    // 4. Each submodule imports memory.
    for m in modules.iter().filter(|m| !is_main(&m.name)) {
        let import_names: BTreeSet<&str> =
            m.imports.iter().map(|(_, name)| name.as_str()).collect();
        assert!(
            import_names.contains("memory"),
            "[{fixture_name}/{}] Missing required import 'memory'. Imports: {import_names:?}",
            m.name
        );
    }

    // 5. Submodules have own __memory_base and __table_base imports (PIC relocation).
    for m in modules.iter().filter(|m| !is_main(&m.name)) {
        let import_names: BTreeSet<&str> =
            m.imports.iter().map(|(_, name)| name.as_str()).collect();
        for required in ["__memory_base", "__table_base"] {
            assert!(
                import_names.contains(required),
                "[{fixture_name}/{}] Missing PIC base import '{required}'. Imports: {import_names:?}",
                m.name
            );
        }
    }

    // 5b. Submodules that depend on other split modules must have per-dependency
    //     PIC bases (__file{N}___memory_base / __file{N}___table_base).
    //     We verify that every __file*___memory_base has a matching __file*___table_base
    //     and vice-versa.
    for m in modules.iter().filter(|m| !is_main(&m.name)) {
        let dep_memory_bases: BTreeSet<&str> = m
            .imports
            .iter()
            .map(|(_, name)| name.as_str())
            .filter(|n| *n != "__memory_base" && n.ends_with("___memory_base"))
            .collect();
        let dep_table_bases: BTreeSet<&str> = m
            .imports
            .iter()
            .map(|(_, name)| name.as_str())
            .filter(|n| *n != "__table_base" && n.ends_with("___table_base"))
            .collect();
        // Every dependency memory base must have a corresponding table base.
        for mb in &dep_memory_bases {
            let prefix = mb.strip_suffix("___memory_base").unwrap();
            let expected_tb = format!("{prefix}___table_base");
            assert!(
                dep_table_bases.contains(expected_tb.as_str()),
                "[{fixture_name}/{}] Dependency '{prefix}' has __memory_base but no __table_base. \
                 table_bases: {dep_table_bases:?}",
                m.name
            );
        }
        for tb in &dep_table_bases {
            let prefix = tb.strip_suffix("___table_base").unwrap();
            let expected_mb = format!("{prefix}___memory_base");
            assert!(
                dep_memory_bases.contains(expected_mb.as_str()),
                "[{fixture_name}/{}] Dependency '{prefix}' has __table_base but no __memory_base. \
                 memory_bases: {dep_memory_bases:?}",
                m.name
            );
        }
    }

    // 6. For every import whose module name matches another output module,
    //    verify a matching export exists in that module (cross-module linkage).
    //    Imports from unknown modules (runtime/host-provided) are skipped
    //    since they are supplied by the loader, not by other output modules.
    let output_names: BTreeSet<&str> = modules.iter().map(|m| m.name.as_str()).collect();

    // TODO: use dylink info or module name from __memory_base/__table_base imports to determine exact module that provide some imports.
    let all_exports: BTreeMap<&str, Vec<&str>> = modules
        .iter()
        .flat_map(|m| {
            m.exports
                .iter()
                .map(|(name, _)| (name.as_str(), m.name.as_str()))
        })
        .fold(BTreeMap::new(), |mut acc, (name, module)| {
            acc.entry(name).or_default().push(module);
            acc
        });

    for m in &modules {
        for (imp_module, imp_name) in &m.imports {
            // Only check imports that reference another output module.
            if !output_names.contains(imp_module.as_str()) {
                continue;
            }
            if is_pic_base(imp_name) {
                continue;
            }
            assert!(
                all_exports.contains_key(imp_name.as_str()),
                "[{fixture_name}/{}] Import '{imp_module}'.'{imp_name}' is not satisfied \
                 by any output module export.\n\
                 Available exports: {:?}",
                m.name,
                all_exports.keys().collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn test_split_example_wasm() {
    run_all_assertions(EXAMPLE_WASM, "example");
}

#[test]
fn test_split_lazy_routes() {
    run_all_assertions(LAZY_ROUTES, "lazy_routes");
}

///
/// Strict spec-level validation via `wasmparser::validate`.
///
#[test]
// Panics because trampolines doesn't provide valid types.
fn test_strict_validation_example() {
    let emitted = run_split(EXAMPLE_WASM);
    for (id, bytes) in &emitted {
        wasmparser::validate(bytes)
            .unwrap_or_else(|e| panic!("[example/{id}] wasmparser::validate failed: {e}"));
    }
}
