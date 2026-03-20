#[cfg(target_os = "linux")]
use std::fs::read_to_string;
use std::{
    fs,
    hint::{black_box, black_box as hint_black_box},
    path::PathBuf,
};

use criterion::{
    Criterion, Throughput, criterion_group, criterion_main,
    measurement::{Measurement, ValueFormatter},
};
use wamex_object::{
    ObjectReader, analysis,
    emit::emit_modules,
    typed::{FileLoader, LoadedFile, Module},
};

fn load_lazy_routes_wasm() -> Vec<u8> {
    let mut src: PathBuf = std::env::var("CARGO_MANIFEST_DIR").unwrap().into();
    src.push("../");
    src.push("wamex-cli");
    src.push("test-data");
    src.push("lazy_routes.wasm");
    fs::read(src).expect("Failed to load test-data/lazy_routes.wasm")
}

fn load_lazy_routes_diff_wasm(changed: bool) -> Vec<u8> {
    let mut src: PathBuf = std::env::var("CARGO_MANIFEST_DIR").unwrap().into();
    src.push("../");
    src.push("wamex-cli");
    src.push("test-data");
    src.push("lazy-small-change");
    if !changed {
        src.push("lazy_routes.wasm");
    } else {
        src.push("lazy_routes_changed.wasm");
    }
    fs::read(src).expect("Failed to load load_lazy_routes_diff_wasm")
}

fn benchmark_parse_module(c: &mut Criterion) {
    c.bench_function("parse_lazy_routes", |b| {
        let lazy_routes_wasm = load_lazy_routes_wasm();
        b.iter(|| {
            let module = ObjectReader::parse(black_box(&lazy_routes_wasm)).unwrap();
            hint_black_box(module);
        })
    });
}

fn benchmark_parse_object_module(c: &mut Criterion) {
    c.bench_function("parse_lazy_routes_object", |b| {
        let lazy_routes_wasm = load_lazy_routes_wasm();
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;
            for _ in 0..iters {
                let module = ObjectReader::parse(&lazy_routes_wasm).unwrap();
                let start = std::time::Instant::now();
                let info = Module::from_raw_module(black_box(&module)).unwrap();
                hint_black_box(info);
                total_duration += start.elapsed();
            }
            total_duration
        })
    });
}

fn benchmark_dependency_analysis(c: &mut Criterion) {
    c.bench_function("get_dependencies", |b| {
        let lazy_routes_wasm = load_lazy_routes_wasm();
        let module = ObjectReader::parse(&lazy_routes_wasm).unwrap();
        let info = LoadedFile::from_raw_module(module).unwrap();
        b.iter(|| {
            let dep_graph = analysis::get_dependencies(black_box(&info)).unwrap();
            hint_black_box(dep_graph);
        })
    });
}

fn benchmark_compute_split_modules(c: &mut Criterion) {
    let mut group = c.benchmark_group("compute_split_modules");
    group.bench_function("no_merge", |b| {
        let lazy_routes_wasm = load_lazy_routes_wasm();
        let module = ObjectReader::parse(&lazy_routes_wasm).unwrap();
        let info = LoadedFile::from_raw_module(module).unwrap();
        let dep_graph = analysis::get_dependencies(&info).unwrap();
        let wbg_fns = analysis::wbg_closures(&info.module, &dep_graph);
        let split_points = analysis::find_split_points_legacy(&info.module).unwrap();
        b.iter(|| {
            let split_program_info = analysis::compute_split_modules(
                black_box(&info.module),
                black_box(&dep_graph),
                black_box(&split_points),
                black_box(&wbg_fns),
                false,
            )
            .unwrap();
            hint_black_box(split_program_info);
        })
    });
    group.bench_function("merge_with_main", |b| {
        let lazy_routes_wasm = load_lazy_routes_wasm();
        let module = ObjectReader::parse(&lazy_routes_wasm).unwrap();
        let info = LoadedFile::from_raw_module(module).unwrap();
        let dep_graph = analysis::get_dependencies(&info).unwrap();
        let wbg_fns = analysis::wbg_closures(&info.module, &dep_graph);
        let split_points = analysis::find_split_points_legacy(&info.module).unwrap();
        b.iter(|| {
            let split_program_info = analysis::compute_split_modules(
                black_box(&info.module),
                black_box(&dep_graph),
                black_box(&split_points),
                black_box(&wbg_fns),
                true,
            )
            .unwrap();
            hint_black_box(split_program_info);
        })
    });
}

fn benchmark_emit_modules(c: &mut Criterion) {
    c.bench_function("emit_modules", |b| {
        let lazy_routes_wasm = load_lazy_routes_wasm();
        let mut file_loader = FileLoader::new();
        let file_id = file_loader
            .load_from_bytes(lazy_routes_wasm.into_boxed_slice())
            .unwrap();
        let info = file_loader.get_file(file_id);
        let dep_graph = analysis::get_dependencies(info).unwrap();
        let wbg_fns = analysis::wbg_closures(&info.module, &dep_graph);
        let split_points = analysis::find_split_points_legacy(&info.module).unwrap();
        let split_program_info = analysis::compute_split_modules(
            &info.module,
            &dep_graph,
            &split_points,
            &wbg_fns,
            true,
        )
        .unwrap();

        b.iter(|| {
            emit_modules(
                black_box(&file_loader),
                black_box(&split_program_info),
                |_ident, _bytes| Ok(()),
            )
            .unwrap();
        })
    });
}

criterion_group! {
    split_benches,
    benchmark_parse_module,
    benchmark_parse_object_module,
    benchmark_dependency_analysis,
    benchmark_compute_split_modules,
    benchmark_emit_modules,
                                    //  benchmark_full_split_pipeline,
                                     // benchmark_incremental_split_pipeline,
}

// // custom group with memory usage benchmark.
// pub fn memory_usage_benches() {
//     let mut criterion: criterion::Criterion<MemUsage> = (criterion::Criterion::default())
//         .configure_from_args()
//         .with_measurement(MemUsage);
//     benchmark_memory_usage_patterns(&mut criterion);
// }

criterion_main!(
    split_benches, // memory_usage_benches
);
