use std::{fs, path::PathBuf};

use profiler::{
    Metrics,
    bench::Bencher,
    metrics::{SystemEvent, SystemPerfMetric},
};

#[global_allocator]
static ALLOCATOR: profiler::metrics::mem::ProfileAllocator =
    profiler::metrics::mem::ProfileAllocator::new();

/// Defines custom metrics for the benchmark.
#[derive(Metrics)]
struct MyMetrics {
    #[new(SystemEvent::Cycles)]
    pub cycles: SystemPerfMetric,

    #[config(show_spread = false, show_baseline = false)]
    pub wall_time: profiler::metrics::InstantProvider,

    #[new(SystemEvent::Instructions)]
    #[config(show_spread = false)]
    pub instructions: SystemPerfMetric,

    #[raw_end_fn(MyMetrics::calculate_ipc)]
    #[config(show_spread = false, show_baseline = false)]
    pub ipc: u64,

    #[hidden]
    #[new(&ALLOCATOR)]
    pub memprofiler: profiler::metrics::mem::ProfilerMetrics,

    #[raw_end_fn(MyMetrics::calculate_peak)]
    #[config(show_spread = false, show_baseline = false, aggregation = profiler::metrics::MetricAggregation::Max)]
    pub mem_peak: usize,
}

impl MyMetrics {
    fn calculate_peak(result: &<MyMetrics as Metrics>::Result) -> usize {
        let mem = &result.4;
        mem.alloced_bytes
    }
    fn calculate_ipc(result: &<MyMetrics as Metrics>::Result) -> u64 {
        let cycles = result.0;
        let instructions = result.2;
        if cycles == 0 {
            0
        } else {
            instructions / cycles
        }
    }
}

fn load_lazy_routes_wasm() -> Vec<u8> {
    let mut src: PathBuf = std::env::var("CARGO_MANIFEST_DIR").unwrap().into();
    src.push("../");
    src.push("wamex-cli");
    src.push("test-data");
    src.push("lazy_routes.wasm");
    fs::read(src).expect("Failed to load test-data/lazy_routes.wasm")
}

fn full(bench: &mut Bencher) {
    bench.run_custom(|mut s| {
        let file = load_lazy_routes_wasm();
        s.finish_setup();
        let _ = wamex_object::emit::split_routine_generic_test(&file);
    });
}

profiler::bench_main!(MyMetrics => full);
