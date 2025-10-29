#![feature(trace_macros)]

use std::{
    collections::{BTreeSet, HashMap},
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{bail, Result};
use tempdir::TempDir;
use wamex_cli::{split, InputModule, Split};

fn split_cmd(src: &Path) -> anyhow::Result<TempDir> {
    let output_temp = TempDir::new("wasm_split_test")?;

    let cli = Split {
        input: src.into(),
        output: output_temp.path().into(),
        verbose: false,
        metadata: false,
        dry_run: false,
        precise_modification: true,
    };
    split(cli)?;

    Ok(output_temp)
}

fn list_data_segments(src: &InputModule) -> Vec<String> {
    src.data
        .data_segments
        .iter()
        .map(|(_id, s)| hex::encode(s.data))
        .collect()
}
fn list_imports(src: &InputModule) -> BTreeSet<String> {
    src.imports
        .iter()
        // .filter(|(_id, i)| matches!(i.ty, wasmparser::TypeRef::Func(_)))
        .map(|(_id, i)| i.name.to_owned())
        .collect()
}

fn list_exports(src: &InputModule) -> BTreeSet<String> {
    src.exports
        .iter()
        // .filter(|(_id, i)| matches!(i.kind, wasmparser::ExternalKind::Func))
        .map(|(_id, i)| i.name.to_owned())
        .collect()
}

mod static_str {
    pub const LOADER_FN: &str = "__wasm_split_load_static_str";
    pub const IMPORT_FN: &str =
        "__wasm_split_00static_str00_import_776d8e51aac0782b17e57d45872c52d7_static_str";

    pub const IMPORTS: &[&str] = &[LOADER_FN, IMPORT_FN];
    pub const EXPORT_BEFORE: &[&str] =
        &["__wasm_split_00static_str00_export_776d8e51aac0782b17e57d45872c52d7_static_str"];
    pub const EXPORT_AFTER: &[&str] = &[
        "__wamex___wasm_split_00static_str00_export_776d8e51aac0782b17e57d45872c52d7_static_str",
    ];
}
mod string_from_static {
    pub const LOADER_FN: &str = "__wasm_split_load_string_from_static";
    pub const IMPORT_FN: &str =
        "__wasm_split_00string_from_static00_import_17997317cc392c52ed3bea15880aab65_string_from_static";

    pub const IMPORTS: &[&str] = &[LOADER_FN, IMPORT_FN];
    pub const EXPORT_BEFORE: &[&str] = &[
        "__wasm_split_00string_from_static00_export_17997317cc392c52ed3bea15880aab65_string_from_static",
    ];
    pub const EXPORT_AFTER: &[&str] =
        &["__wamex___wasm_split_00string_from_static00_export_17997317cc392c52ed3bea15880aab65_string_from_static"];
}

const REQUIRED_MAIN_EXPORTS: &[&str] = &[
    "memory",
    "print_lazy_loaded_string",
    // TODO: data_begin?
    "__heap_base",
    "__data_end",
    "__wasm_split_load_callback",
];
const EXTRA_MAIN_EXPORTS: &[&str] = &["__stack_pointer", "__indirect_function_table"];

fn check_lists(list: &BTreeSet<String>, expected_lists: &[&[&str]], name: &str) -> Result<()> {
    for expected_list in expected_lists {
        let missing: Vec<_> = expected_list
            .iter()
            .filter(|e| !list.contains(&e.to_string()))
            .cloned()
            .collect();
        if !missing.is_empty() {
            dbg!(&list);
            bail!("Missing item in {name}: {:?}", missing);
        }
    }
    Ok(())
}
fn check_lists_not(
    list: &BTreeSet<String>,
    unexpected_lists: &[&[&str]],
    name: &str,
) -> Result<()> {
    for unexpected in unexpected_lists {
        let unexpected: Vec<_> = unexpected
            .iter()
            .filter(|e| list.contains(&e.to_string()))
            .cloned()
            .collect();
        if !unexpected.is_empty() {
            bail!("Unexpected item in {name}: {:?}", unexpected);
        }
    }
    Ok(())
}

macro_rules! test_list_contain {

    ($list:ident => $($expected:expr),* ) => {
        check_lists(&$list, &[$($expected),*],stringify!($list)).unwrap()
    };
    ($list:ident => @NOT $($unexpected:expr),*) => {
        check_lists_not(&$list, &[$($unexpected),*], stringify!($list)).unwrap()
    };
    ($($list:ident =>$(@$v:ident)? $($expected:expr),*);*) => {
        $(
            // test($list)
            test_list_contain!($list =>$(@$v)? $($expected),*);
        )*
    };
}

#[test]
fn test_correct_imports_exports() {
    let _ = env_logger::Builder::new()
        .filter(None, log::LevelFilter::Warn)
        .parse_env("RUST_LOG")
        .try_init();
    let mut src: PathBuf = std::env::var("CARGO_MANIFEST_DIR").unwrap().into();

    src.push("test-data");
    src.push("example.wasm");
    dbg!(&src);

    let output_temp = split_cmd(&src).expect("Failed to split wasm file");
    let input_bytes = std::fs::read(&src).expect("Failed to read wasm file");
    let input_module = InputModule::parse(&input_bytes).expect("Failed to parse wasm file");

    let imports = list_imports(&input_module);
    let exports = list_exports(&input_module);

    test_list_contain! {
        imports => static_str::IMPORTS, string_from_static::IMPORTS;
        exports => REQUIRED_MAIN_EXPORTS, static_str::EXPORT_BEFORE, string_from_static::EXPORT_BEFORE;
        exports => @NOT EXTRA_MAIN_EXPORTS// TODO: It is based on rustc imports
    }

    let mut list_entries = std::fs::read_dir(output_temp.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    list_entries.sort();

    assert_eq!(
        list_entries,
        vec!["main.wasm", "static_str.wasm", "string_from_static.wasm"]
    );

    let main = std::fs::read(output_temp.path().join("main.wasm")).unwrap();
    let main = InputModule::parse(&main).unwrap();

    let main_imports = list_imports(&main);
    let main_exports = list_exports(&main);
    test_list_contain! {
        main_imports => &[static_str::LOADER_FN, string_from_static::LOADER_FN];
        main_exports => EXTRA_MAIN_EXPORTS, REQUIRED_MAIN_EXPORTS;
        main_exports => @NOT static_str::EXPORT_BEFORE, string_from_static::EXPORT_BEFORE;
        main_imports => @NOT &[static_str::IMPORT_FN, string_from_static::IMPORT_FN]

    }

    let static_str = std::fs::read(output_temp.path().join("static_str.wasm")).unwrap();
    let static_str = InputModule::parse(&static_str).unwrap();
    let static_str_imports = list_imports(&static_str);
    let static_str_exports = list_exports(&static_str);

    test_list_contain! {
        static_str_imports => &["__lib_base", "__stack_pointer", "memory", "__wamex__ZN8dlmalloc8dlmalloc17Dlmalloc$LT$A$GT$6malloc17h6dc9611e5a260cc8E"];
        static_str_exports => static_str::EXPORT_AFTER;
        static_str_imports => @NOT string_from_static::IMPORTS, static_str::IMPORTS;
        static_str_exports => @NOT string_from_static::EXPORT_BEFORE
    }

    let string_from_static =
        std::fs::read(output_temp.path().join("string_from_static.wasm")).unwrap();

    let string_from_static = InputModule::parse(&string_from_static).unwrap();

    let string_from_static_imports = list_imports(&string_from_static);
    let string_from_static_exports = list_exports(&string_from_static);
    test_list_contain! {
        string_from_static_imports => &["__lib_base", "__stack_pointer", "memory", "__wamex__ZN8dlmalloc8dlmalloc17Dlmalloc$LT$A$GT$6malloc17h6dc9611e5a260cc8E", "__wamex__ZN5alloc7raw_vec12handle_error17hffd4f9c6873ec0fbE"];
        string_from_static_exports => string_from_static::EXPORT_AFTER;
        string_from_static_imports => @NOT string_from_static::IMPORTS, static_str::IMPORTS;
        string_from_static_exports => @NOT static_str::EXPORT_BEFORE
    }
}

// Snapshot imports, exports and data segments of wasm modules.
fn snapshot_module_structure(src: &Path) {
    let input_bytes = std::fs::read(src).expect("Failed to read wasm file");
    let input_module = InputModule::parse(&input_bytes).expect("Failed to parse wasm file");

    let imports = list_imports(&input_module);
    let exports = list_exports(&input_module);
    let data_segments = list_data_segments(&input_module);

    let file_name = src.file_name().unwrap().to_string_lossy();
    insta::assert_snapshot!(
        format!("{} - imports", file_name),
        format!("{:#?}", imports)
    );
    insta::assert_snapshot!(
        format!("{} - exports", file_name),
        format!("{:#?}", exports)
    );
    insta::assert_snapshot!(
        format!("{} - data_segments", file_name),
        format!("{:#?}", data_segments)
    );
}

#[test]
fn test_insta_imports_exports_of_extended_example() {
    let _ = env_logger::Builder::new()
        .filter(None, log::LevelFilter::Warn)
        .parse_env("RUST_LOG")
        .try_init();
    let mut src: PathBuf = std::env::var("CARGO_MANIFEST_DIR").unwrap().into();
    src.push("test-data");
    src.push("extended-example.wasm");
    snapshot_module_structure(&src);
    // now split and snapshot all files
    let output_temp = split_cmd(&src).expect("Failed to split wasm file");
    let mut num_readed = 0;
    let mut list_entries = std::fs::read_dir(output_temp.path()).unwrap();
    while let Some(entry) = list_entries.next() {
        let entry = entry.unwrap();
        let path = entry.path();
        snapshot_module_structure(&path);
        num_readed += 1;
    }
    // main, static_str, string_from_static, dyn, async, dep_dyn, shared_static_str, shared(shared_static_str + static_str)
    assert_eq!(num_readed, 8);
}

fn check_that_precise_modification_works(src: PathBuf) {
    let _ = env_logger::Builder::new()
        .filter(None, log::LevelFilter::Warn)
        .parse_env("RUST_LOG")
        .try_init();
    let output_temp = split_cmd(&src).expect("Failed to split wasm file");

    let mut files: HashMap<OsString, Vec<u8>> = HashMap::new();
    for entry in std::fs::read_dir(output_temp.path()).unwrap() {
        let entry = entry.unwrap();
        let data = std::fs::read(&entry.path()).unwrap();
        let file_name = entry.path().file_name().unwrap().to_owned();
        files.insert(file_name, data);
    }

    // non-precise split

    let output_temp = TempDir::new("wasm_split_test2").unwrap();

    let cli = Split {
        input: src.into(),
        output: output_temp.path().into(),
        verbose: false,
        metadata: false,
        dry_run: false,
        precise_modification: false,
    };
    split(cli).unwrap();

    for entry in std::fs::read_dir(output_temp.path()).unwrap() {
        let entry = entry.unwrap();
        let data = std::fs::read(&entry.path()).unwrap();
        let file_name = entry.path().file_name().unwrap().to_owned();
        let original_data = files.get(&file_name).unwrap();
        assert_eq!(
            hex::encode(&data),
            hex::encode(original_data),
            "File {:?} should be same in any mode",
            file_name
        );
    }
}

#[test]
fn check_precise_modification_mode() {
    let mut src: PathBuf = std::env::var("CARGO_MANIFEST_DIR").unwrap().into();
    src.push("test-data");
    for file in ["example.wasm", "extended-example.wasm", "lazy_routes.wasm"] {
        let mut file_path = src.clone();
        file_path.push(file);
        check_that_precise_modification_works(file_path);
    }
}
