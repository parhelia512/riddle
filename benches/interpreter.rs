//! Interpreter hot-path benchmarks.
//!
//! Each benchmark compiles a fixed Riddle program to MIR once (setup), then
//! times `main` on a fresh [`interpreter::Session`]. The workloads pin the
//! paths `riddle run` and the REPL hit hardest: basic-block dispatch, call
//! frames, and std container churn. Run with `cargo bench`; regressions here
//! guard the executor's no-clone block loop.

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use interpreter::{Config, Session};
use riddlec::pipeline;

/// A program whose main answers a checksum, so a silent semantics change
/// fails loudly instead of just timing differently.
struct Workload {
    source: &'static str,
    exit_code: i32,
}

const LOOPS: Workload = Workload {
    source: r#"
    fun main() -> i32 {
        let mut total = 0i64;
        let mut i = 0;
        while i < 60_000 {
            total += i as i64;
            i += 1;
        }
        if total == 1_799_970_000 { 0 } else { 1 }
    }
    "#,
    exit_code: 0,
};

const FIB: Workload = Workload {
    source: r#"
    fun fib(n: i32) -> i64 {
        if n < 2 {
            n as i64
        } else {
            fib(n - 2) + fib(n - 1)
        }
    }

    fun main() -> i32 {
        if fib(24) == 46_368 { 0 } else { 1 }
    }
    "#,
    exit_code: 0,
};

const VECTOR: Workload = Workload {
    source: r#"
    fun main() -> i32 {
        let mut total = 0i64;
        let mut i = 0;
        while i < 40 {
            let mut v: Vector<i32> = Vector::new();
            let mut j = 0;
            while j < 400 {
                v.push(j);
                j += 1;
            }
            let mut k: usize = 0;
            while k < v.len() {
                total += v[k] as i64;
                k += 1;
            }
            i += 1;
        }
        if total == 3_192_000 { 0 } else { 1 }
    }
    "#,
    exit_code: 0,
};

fn bench_workload(c: &mut Criterion, name: &str, workload: &Workload) {
    let mut group = c.benchmark_group(format!("interpreter/{name}"));
    group.sample_size(30);
    let result = pipeline::compile(workload.source);
    assert!(
        result.success(),
        "bench program {name} must compile: {:#?}",
        result.analysis_diagnostics
    );
    let module = result.mir_module.expect("successful compile produced MIR");
    let config = Config::default();

    group.bench_function(BenchmarkId::new("run_main", name), |b| {
        b.iter_batched(
            || Session::new(&module, result.source_files.clone(), &config),
            |mut session| {
                let code = session
                    .call("main", Vec::new())
                    .expect("workload main must not trap");
                let interpreter::Val::Int(bits) = code else {
                    panic!("workload main must return an integer");
                };
                assert_eq!(
                    bits as u32 as i32, workload.exit_code,
                    "workload {name} produced the wrong exit code"
                );
            },
            BatchSize::PerIteration,
        )
    });
    group.finish();
}

fn interpreter_benchmarks(c: &mut Criterion) {
    bench_workload(c, "loops", &LOOPS);
    bench_workload(c, "fib", &FIB);
    bench_workload(c, "vector", &VECTOR);
}

criterion_group!(benches, interpreter_benchmarks);
criterion_main!(benches);
