#[cfg(target_os = "linux")]
use std::fs::read_to_string;
use std::{fs, hint::black_box as hint_black_box, path::PathBuf};

use criterion::{
    black_box, criterion_group, criterion_main,
    measurement::{Measurement, ValueFormatter},
    Criterion, Throughput,
};
use wamex_cli::{
    analysis::{self, split_point::SplitProgramInfo},
    emit,
    read::InputModule,
};

fn load_lazy_routes_wasm() -> Vec<u8> {
    let mut src: PathBuf = std::env::var("CARGO_MANIFEST_DIR").unwrap().into();
    src.push("test-data");
    src.push("lazy_routes.wasm");
    fs::read(src).expect("Failed to load test-data/lazy_routes.wasm")
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
    let lazy_routes_wasm = load_lazy_routes_wasm();
    c.bench_function("parse_lazy_routes", |b| {
        b.iter(|| {
            let module = InputModule::parse(black_box(&lazy_routes_wasm)).unwrap();
            hint_black_box(module);
        })
    });
}

fn benchmark_dependency_analysis(c: &mut Criterion) {
    let lazy_routes_wasm = load_lazy_routes_wasm();
    let module = InputModule::parse(&lazy_routes_wasm).unwrap();
    let info = analysis::ModuleInfo::from_raw_module(&module).unwrap();

    c.bench_function("get_dependencies", |b| {
        b.iter(|| {
            let dep_graph =
                analysis::dep_graph::get_dependencies(black_box(&module), black_box(&info))
                    .unwrap();
            hint_black_box(dep_graph);
        })
    });
}

fn benchmark_compute_split_modules(c: &mut Criterion) {
    let lazy_routes_wasm = load_lazy_routes_wasm();
    let module = InputModule::parse(&lazy_routes_wasm).unwrap();
    let info = analysis::ModuleInfo::from_raw_module(&module).unwrap();
    let dep_graph = analysis::dep_graph::get_dependencies(&module, &info).unwrap();
    let split_points = analysis::split_point::find_split_points(&module, &info).unwrap();

    c.bench_function("compute_split_modules", |b| {
        b.iter(|| {
            let split_program_info = SplitProgramInfo::compute_split_modules(
                black_box(&info),
                black_box(&dep_graph),
                black_box(&split_points),
            )
            .unwrap();
            hint_black_box(split_program_info);
        })
    });
}

fn benchmark_emit_modules(c: &mut Criterion) {
    let lazy_routes_wasm = load_lazy_routes_wasm();
    let module = InputModule::parse(&lazy_routes_wasm).unwrap();
    let info = analysis::ModuleInfo::from_raw_module(&module).unwrap();
    let dep_graph = analysis::dep_graph::get_dependencies(&module, &info).unwrap();
    let split_points = analysis::split_point::find_split_points(&module, &info).unwrap();
    let mut split_program_info =
        SplitProgramInfo::compute_split_modules(&info, &dep_graph, &split_points).unwrap();

    // Apply the same optimizations as the main split function
    emit::merge_main_shared(&mut split_program_info);
    let wbg_fns = emit::hoist_wbg_deps_to_main(&info, &dep_graph, &mut split_program_info);

    c.bench_function("emit_modules_fast", |b| {
        b.iter(|| {
            let mut output_counter = 0;
            let result = emit::emit_modules(
                black_box(&info),
                black_box(&split_program_info),
                black_box(&wbg_fns),
                false,
                |_identifier, data| {
                    // Just count outputs instead of writing to disk
                    output_counter += 1;
                    hint_black_box(data);
                    Ok(())
                },
            );
            hint_black_box(result.unwrap());
            hint_black_box(output_counter);
        })
    });
    c.bench_function("emit_modules_precise", |b| {
        b.iter(|| {
            let mut output_counter = 0;
            let result = emit::emit_modules(
                black_box(&info),
                black_box(&split_program_info),
                black_box(&wbg_fns),
                true,
                |_identifier, data| {
                    // Just count outputs instead of writing to disk
                    output_counter += 1;
                    hint_black_box(data);
                    Ok(())
                },
            );
            hint_black_box(result.unwrap());
            hint_black_box(output_counter);
        })
    });
}

fn benchmark_full_split_pipeline(c: &mut Criterion) {
    let lazy_routes_wasm = load_lazy_routes_wasm();
    c.bench_function("full_split_lazy_routes", |b| {
        b.iter(|| {
            // Use the CLI API with dry_run to avoid file I/O
            let result = wamex_cli::split_inner(
                black_box(&lazy_routes_wasm),
                false,
                false,
                true,
                None,
                |_, _| Ok(()),
            );
            hint_black_box(result.unwrap());
        })
    });
}

fn benchmark_memory_usage_patterns(c: &mut Criterion<MemUsage>) {
    let lazy_routes_wasm = load_lazy_routes_wasm();
    c.bench_function("memory_usage_full_pipeline", |b| {
        b.iter_custom(|iters| {
            let mut accumulated_mem_usage = 0;

            for _ in 0..iters {
                let mut oneshot = false;
                // Use CLI API and hook into emit_module_fn to measure peak memory during emission
                let _ = wamex_cli::split_inner(
                    black_box(&lazy_routes_wasm),
                    false,
                    false,
                    true,
                    None,
                    |_, _| {
                        if oneshot {
                            return Ok(());
                        }
                        oneshot = true;
                        // Measure memory usage during module emission
                        if let Some(current_mem) = get_memory_usage() {
                            accumulated_mem_usage += current_mem;
                        }
                        Ok(())
                    },
                );
            }

            accumulated_mem_usage
        })
    });
}

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
        if res.is_nan() {
            0.0
        } else {
            res
        }
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
            (10f64.powi(3), "KB")
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

criterion_group!(
    split_benches,
    benchmark_parse_module,
    benchmark_dependency_analysis,
    benchmark_compute_split_modules,
    benchmark_emit_modules,
    benchmark_full_split_pipeline,
);

// custom group with memory usage benchmark.
pub fn memory_usage_benches() {
    let mut criterion: criterion::Criterion<MemUsage> = (criterion::Criterion::default())
        .configure_from_args()
        .with_measurement(MemUsage);
    benchmark_memory_usage_patterns(&mut criterion);
}

criterion_main!(split_benches, memory_usage_benches);
