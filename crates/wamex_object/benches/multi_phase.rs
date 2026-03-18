use std::{fs, path::PathBuf};

use profiler::{Metrics, bench::Bencher, metrics::perf_event};

#[global_allocator]
static ALLOCATOR: profiler::metrics::mem::ProfileAllocator =
    profiler::metrics::mem::ProfileAllocator::new();

/// Defines custom metrics for the benchmark.
#[derive(Metrics)]
struct MyMetrics {
    /// CPU cycles spent in the span.
    /// The first metric in the list will be used as the primary metric and adds report of %parent in the report.
    #[new(perf_event::events::Hardware::CPU_CYCLES)]
    pub cycles: profiler::PerfEventMetric,

    /// Time spent on CPU for specific thread.
    /// On short intervals can report more than cpu-time/wall-time.
    /// But gives a good estimate on real CPU time spent in kernel/user mode.
    #[new(perf_event::events::Software::TASK_CLOCK)]
    #[config(show_spread = false, show_baseline = false)]
    pub task_clock: profiler::metrics::PerfEventMetric,

    #[new(perf_event::events::Hardware::INSTRUCTIONS)]
    #[config(show_spread = false)]
    pub instructions: profiler::metrics::PerfEventMetric,

    #[raw_end_fn(MyMetrics::calculate_ipc)]
    #[config(show_spread = false, show_baseline = false)]
    pub ipc: u64,

    ///
    /// Metrics can be marked as #[hidden], so they will be collected but not used in report.
    ///
    #[hidden]
    #[new(&ALLOCATOR)]
    pub memprofiler: profiler::metrics::mem::ProfilerMetrics,

    /// #[raw_end_fn] can be used to define custom metrics from existing ones.
    #[raw_end_fn(MyMetrics::calculate_peak)]
    #[config(show_spread = false, show_baseline = false)]
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

fn emit_modules(bench: &mut Bencher) {
    bench.run_custom(|mut s| {
        let file = load_lazy_routes_wasm();
        s.finish_setup();
        wamex_object::emit::split_routine_generic_test(&file);
    });
}

profiler::bench_main!(MyMetrics => emit_modules);
