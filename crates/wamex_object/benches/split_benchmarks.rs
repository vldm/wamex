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

/// Get current memory usage in KB (Linux only)
#[cfg(target_os = "linux")]
fn get_memory_usage() -> Option<u64> {
    let status = read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if line.starts_with("VmRSS:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                return parts[1].parse().ok();
            }
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn get_memory_usage() -> Option<u64> {
    None
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

// fn benchmark_incremental_split_pipeline(c: &mut Criterion) {
//     c.bench_function("incremental_split_lazy_routes_second_run", |b| {
//         let src_wasm = load_lazy_routes_diff_wasm(false);

//         let changed_wasm = load_lazy_routes_diff_wasm(true);
//         let mut state = wamex_cli::IncrementalSplitState::new();
//         let _result = state
//             .split_incremental(
//                 black_box(&src_wasm),
//                 false,
//                 true,
//                 SplitPointExtractor::Wamex,
//                 |_, _| Ok(()),
//             )
//             .unwrap();
//         let src_state = state.clone();
//         b.iter_custom(|iters| {
//             let mut total_duration = std::time::Duration::ZERO;
//             assert!(!src_state.is_empty());
//             for _ in 0..iters {
//                 let mut state = src_state.clone();
//                 let start = std::time::Instant::now();
//                 let result = state
//                     .split_incremental(
//                         black_box(&changed_wasm),
//                         false,
//                         true,
//                         SplitPointExtractor::Wamex,
//                         |_, _| Ok(()),
//                     )
//                     .unwrap();

//                 hint_black_box(state);
//                 hint_black_box(result);
//                 total_duration += start.elapsed();
//             }
//             total_duration
//         })
//     });
// }

// fn benchmark_memory_usage_patterns(c: &mut Criterion<MemUsage>) {
//     c.bench_function("memory_usage_full_pipeline", |b| {
//         let lazy_routes_wasm = load_lazy_routes_wasm();
//         b.iter_custom(|iters| {
//             let mut accumulated_mem_usage = 0;

//             for _ in 0..iters {
//                 let mut oneshot = false;
//                 // Use CLI API and hook into emit_module_fn to measure peak memory during emission
//                 let _ = wamex_cli::split_inner(
//                     black_box(&lazy_routes_wasm),
//                     false,
//                     true,
//                     SplitPointExtractor::Legacy,
//                     |_, _| {
//                         if oneshot {
//                             return Ok(());
//                         }
//                         oneshot = true;
//                         // Measure memory usage during module emission
//                         if let Some(current_mem) = get_memory_usage() {
//                             accumulated_mem_usage += current_mem;
//                         }
//                         Ok(())
//                     },
//                 );
//             }

//             accumulated_mem_usage
//         })
//     });
// }

struct MemUsage;

impl Measurement for MemUsage {
    type Intermediate = u64;
    type Value = u64;

    fn start(&self) -> Self::Intermediate {
        get_memory_usage().unwrap()
    }
    fn end(&self, i: Self::Intermediate) -> Self::Value {
        i
    }
    fn add(&self, v1: &Self::Value, v2: &Self::Value) -> Self::Value {
        *v1 + *v2
    }
    fn zero(&self) -> Self::Value {
        0
    }
    fn to_f64(&self, val: &Self::Value) -> f64 {
        let res = *val as f64;
        if res.is_nan() { 0.0 } else { res }
    }
    fn formatter(&self) -> &dyn ValueFormatter {
        &MemUsage
    }
}

impl ValueFormatter for MemUsage {
    fn scale_throughputs(
        &self,
        _typical: f64,
        _throughput: &Throughput,
        _values: &mut [f64],
    ) -> &'static str {
        "scale throughput is not applicable"
    }

    fn scale_values(&self, ns: f64, values: &mut [f64]) -> &'static str {
        let (factor, unit) = if ns < 10f64.powi(0) {
            (10f64.powi(3), "Bytes")
        } else if ns < 10f64.powi(3) {
            (10f64.powi(0), "KB")
        } else if ns < 10f64.powi(6) {
            (10f64.powi(-3), "MB")
        } else if ns < 10f64.powi(9) {
            (10f64.powi(-6), "GB")
        } else {
            (10f64.powi(-9), "TB")
        };
        for val in values {
            *val *= factor;
        }

        unit
    }

    fn scale_for_machines(&self, _values: &mut [f64]) -> &'static str {
        "bytes"
    }
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
