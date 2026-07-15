//! Criterion benchmarks for the wasmi guest path — the PERF-01 measurement
//! prerequisite from the 2026-07-14 deep-dive review.
//!
//! Coverage:
//!
//! 1. **Guest instantiation** — module compile (`WasmiBlock::load_from_bytes`)
//!    and the per-call cost of `handle` on a preloaded block. Today every
//!    `handle` creates a fresh `Store` + `Instance` (and re-runs `_start` for
//!    TinyGo guests), so "warm" calls still pay the full instantiation —
//!    exactly the overhead PERF-01 targets.
//! 2. **ABI round-trips** — 1 KiB / 64 KiB / 1 MiB bodies echoed through a
//!    Rust guest, measuring the JSON `(Message, Vec<u8>)` framing (JSON
//!    encodes bytes as integer arrays, expanding payloads several-fold).
//! 3. **Nested calls** — a guest that `call_block`s a native block and a
//!    guest that `call_block`s another WASM guest (itself), via the full
//!    `Wafer` runtime dispatch path.
//! 4. **Cold vs warm TinyGo guest** — a TinyGo wasi module exports `_start`
//!    (Go runtime init), which the loader re-runs on every per-call
//!    instantiation; compared against the Rust guest which has no `_start`.
//!    Skipped with a note when the TinyGo fixture was not built (requires
//!    `tinygo` on PATH — see `scripts/build-fixtures.sh`).
//!
//! Fixtures are built by `scripts/build-fixtures.sh` (same mechanism as the
//! wasm integration tests) and loaded at runtime via path, so compiling the
//! bench does not require them.

use std::{hint::black_box, path::PathBuf, sync::Arc, time::Duration};

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use wafer_run::{
    wasm::WasmiBlock, Block, BlockInfo, Context, ErrorCode, FuelLimit, InputStream, InstanceMode,
    Message, OutputStream, ResourceLimits, Wafer, WaferError,
};

// ---------------------------------------------------------------------------
// Fixture discovery — same runtime-path mechanism as the wasm e2e tests
// (e.g. tests/dispatch_streaming.rs).
// ---------------------------------------------------------------------------

/// Path to the prebuilt Rust bench guest. Panics with a build hint when the
/// fixture is missing — run `scripts/build-fixtures.sh` first.
fn bench_guest_wasm() -> Vec<u8> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("benches/fixtures/bench_guest/target/wasm32-wasip1/release/bench_guest.wasm");
    std::fs::read(&p).unwrap_or_else(|e| {
        panic!(
            "failed to read bench guest wasm at {}: {e}\n\
             Did you build the fixtures first?\n  bash scripts/build-fixtures.sh",
            p.display()
        )
    })
}

/// Path to the prebuilt TinyGo bench guest. `None` when the fixture was not
/// built (TinyGo is a local toolchain, not a CI dependency) — the TinyGo
/// benchmarks are skipped with a note in that case.
fn tinygo_guest_wasm() -> Option<Vec<u8>> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("benches/fixtures/tinygo_guest/target/tinygo_guest.wasm");
    std::fs::read(&p).ok()
}

// ---------------------------------------------------------------------------
// Mock context — minimal implementation for direct `WasmiBlock::handle`
// benches (no nested dispatch). Same shape as tests/wasmi_block_test.rs.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct MockContext;

#[async_trait::async_trait]
impl Context for MockContext {
    async fn call_block(&self, _name: &str, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::error(WaferError::new(
            ErrorCode::Unimplemented,
            "mock context: call_block not supported",
        ))
    }

    fn is_cancelled(&self) -> bool {
        false
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        None
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        Arc::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Native echo block — callee for the guest→native nested-call bench.
// ---------------------------------------------------------------------------

struct NativeEchoBlock;

#[async_trait::async_trait]
impl Block for NativeEchoBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "bench/native-echo",
            "0.0.0",
            "handler@v1",
            "Native echo callee for nested-call benchmarks",
        )
        .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, input: InputStream) -> OutputStream {
        OutputStream::respond(input.collect_to_bytes().await)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn tokio_rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime builds")
}

/// Drive one `handle` call on a directly-loaded block and return the
/// response body. Panics on a non-Respond terminal so mis-setup fails loudly.
async fn handle_once(block: &WasmiBlock, kind: &str, body: Vec<u8>) -> Vec<u8> {
    let ctx = MockContext;
    let out = block
        .handle(&ctx, Message::new(kind), InputStream::from_bytes(body))
        .await;
    out.collect_buffered()
        .await
        .unwrap_or_else(|t| panic!("guest {kind} should Respond, got: {t:?}"))
        .body
}

/// Drive one `run_block` call through the started runtime and return the
/// response body.
async fn run_block_once(wafer: &Wafer, block: &str, kind: &str, body: Vec<u8>) -> Vec<u8> {
    let out = wafer
        .run_block(block, Message::new(kind), InputStream::from_bytes(body))
        .await;
    out.collect_buffered()
        .await
        .unwrap_or_else(|t| panic!("run_block({block}, {kind}) should Respond, got: {t:?}"))
        .body
}

// ---------------------------------------------------------------------------
// 1 + 4. Guest instantiation, cold vs warm, Rust vs TinyGo
// ---------------------------------------------------------------------------

fn bench_instantiation(c: &mut Criterion) {
    let rt = tokio_rt();
    let rust_wasm = bench_guest_wasm();
    let tinygo_wasm = tinygo_guest_wasm();
    if tinygo_wasm.is_none() {
        eprintln!(
            "note: tinygo_guest.wasm not found — skipping TinyGo benches \
             (install tinygo and re-run scripts/build-fixtures.sh)"
        );
    }

    let mut group = c.benchmark_group("guest_instantiation");
    group.measurement_time(Duration::from_secs(5));

    // Module compile + linker build (no instantiation, no call).
    group.bench_function("compile/rust_guest", |b| {
        b.iter(|| WasmiBlock::load_from_bytes(black_box(&rust_wasm)).expect("load rust guest"))
    });
    if let Some(wasm) = &tinygo_wasm {
        group.bench_function("compile/tinygo_guest", |b| {
            b.iter(|| WasmiBlock::load_from_bytes(black_box(wasm)).expect("load tinygo guest"))
        });
    }

    // Cold: compile + first call.
    group.bench_function("cold_load_and_call/rust_guest", |b| {
        b.to_async(&rt).iter(|| async {
            let block = WasmiBlock::load_from_bytes(&rust_wasm).expect("load rust guest");
            black_box(handle_once(&block, "bench.echo", Vec::new()).await)
        })
    });
    if let Some(wasm) = &tinygo_wasm {
        group.bench_function("cold_load_and_call/tinygo_guest", |b| {
            b.to_async(&rt).iter(|| async {
                let block = WasmiBlock::load_from_bytes(wasm).expect("load tinygo guest");
                black_box(handle_once(&block, "bench.ignored", Vec::new()).await)
            })
        });
    }

    // Warm: `handle` on a preloaded block. Under today's loader this still
    // creates a fresh Store + Instance per call (and re-runs `_start` for
    // TinyGo) — the per-call floor PERF-01 targets.
    let rust_block = WasmiBlock::load_from_bytes(&rust_wasm).expect("load rust guest");
    group.bench_function("warm_call_preloaded/rust_guest", |b| {
        b.to_async(&rt)
            .iter(|| async { black_box(handle_once(&rust_block, "bench.echo", Vec::new()).await) })
    });
    if let Some(wasm) = &tinygo_wasm {
        let tinygo_block = WasmiBlock::load_from_bytes(wasm).expect("load tinygo guest");
        group.bench_function("warm_call_preloaded/tinygo_guest", |b| {
            b.to_async(&rt).iter(|| async {
                black_box(handle_once(&tinygo_block, "bench.ignored", Vec::new()).await)
            })
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// 2. ABI round-trips at 1 KiB / 64 KiB / 1 MiB
// ---------------------------------------------------------------------------

fn bench_abi_round_trip(c: &mut Criterion) {
    let rt = tokio_rt();
    let rust_wasm = bench_guest_wasm();

    // Raised limits: the JSON ABI expands a 1 MiB body to ~3.9 MiB of
    // integer-array text, and parsing + re-serialising it inside the
    // interpreted guest exceeds the production defaults (100M fuel,
    // 16 MiB memory). Fuel metering stays ON (matching the production
    // engine configuration) with a budget high enough to never trap, and
    // the same limits are used for every payload size so the sizes are
    // comparable. That the defaults cannot round-trip 1 MiB is itself a
    // PERF-01 data point — recorded here rather than hidden.
    let block = WasmiBlock::load_from_bytes_with_limits(
        &rust_wasm,
        ResourceLimits {
            fuel: FuelLimit::Metered(1_000_000_000_000),
            memory_pages: 4096, // 256 MiB
            ..ResourceLimits::default()
        },
    )
    .expect("load rust guest with raised limits");

    // Sanity-check the echo round-trip once per size before measuring.
    for size in [1usize << 10, 64 << 10, 1 << 20] {
        let payload = vec![0xA5u8; size];
        let echoed = rt.block_on(handle_once(&block, "bench.echo", payload.clone()));
        assert_eq!(echoed, payload, "echo mismatch at {size} bytes");
    }

    let mut group = c.benchmark_group("abi_round_trip");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));

    for (label, size) in [
        ("1KiB", 1usize << 10),
        ("64KiB", 64 << 10),
        ("1MiB", 1 << 20),
    ] {
        let payload = vec![0xA5u8; size];
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(label),
            &payload,
            |b, payload| {
                b.to_async(&rt).iter_batched(
                    || payload.clone(),
                    |p| async { black_box(handle_once(&block, "bench.echo", p).await) },
                    BatchSize::LargeInput,
                )
            },
        );
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// 3. Nested calls through the full runtime dispatch path
// ---------------------------------------------------------------------------

fn bench_nested_call(c: &mut Criterion) {
    let rt = tokio_rt();
    let rust_wasm = bench_guest_wasm();

    let wafer = rt.block_on(async {
        let mut w = Wafer::builder()
            .disable_inventory()
            .disable_lockfile()
            .build()
            .expect("Wafer::build");
        w.register_block("bench/native-echo", Arc::new(NativeEchoBlock))
            .expect("register native echo");
        let guest = WasmiBlock::load_from_bytes(&rust_wasm).expect("load rust guest");
        w.register_block("bench/guest", Arc::new(guest))
            .expect("register bench guest");
        w.start().await.expect("start runtime")
    });

    // Sanity-check every arm once before measuring.
    let payload = vec![0xA5u8; 1 << 10];
    for kind in ["bench.echo", "bench.nested_native", "bench.nested_wasm"] {
        let echoed = rt.block_on(run_block_once(&wafer, "bench/guest", kind, payload.clone()));
        assert_eq!(echoed, payload, "{kind} round-trip mismatch");
    }

    let mut group = c.benchmark_group("nested_call");
    group.sample_size(30);
    group.measurement_time(Duration::from_secs(5));
    group.throughput(Throughput::Bytes(payload.len() as u64));

    // Baseline: runtime dispatch into the guest, no nested call. The delta
    // between this and the nested arms is the nested-dispatch overhead.
    group.bench_function("baseline_guest_echo_via_runtime", |b| {
        b.to_async(&rt).iter_batched(
            || payload.clone(),
            |p| async { black_box(run_block_once(&wafer, "bench/guest", "bench.echo", p).await) },
            BatchSize::LargeInput,
        )
    });

    // Guest → native block (one guest instantiation + streaming ABI hop).
    group.bench_function("guest_to_native", |b| {
        b.to_async(&rt).iter_batched(
            || payload.clone(),
            |p| async {
                black_box(run_block_once(&wafer, "bench/guest", "bench.nested_native", p).await)
            },
            BatchSize::LargeInput,
        )
    });

    // Guest → guest (pays a second per-call instantiation for the callee).
    group.bench_function("guest_to_wasm", |b| {
        b.to_async(&rt).iter_batched(
            || payload.clone(),
            |p| async {
                black_box(run_block_once(&wafer, "bench/guest", "bench.nested_wasm", p).await)
            },
            BatchSize::LargeInput,
        )
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_instantiation,
    bench_abi_round_trip,
    bench_nested_call
);
criterion_main!(benches);
