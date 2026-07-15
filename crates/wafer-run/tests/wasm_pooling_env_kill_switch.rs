//! Host kill switch for wasm instance pooling: `WAFER_RUN_WASM_POOLING`
//! (PERF-01 Part B).
//!
//! Lives in its own integration-test binary (its own process) with a SINGLE
//! `#[test]` because it mutates process-global env that every `WasmiBlock`
//! load and every `Wafer::seal` reads — parallel tests in the same binary
//! would race on it.

#![cfg(feature = "wasm")]

use std::{path::PathBuf, sync::Arc};

use wafer_block::streams::output::TerminalNotResponse;
use wafer_run::{
    wasm::{WasmiBlock, WASM_POOLING_ENV},
    Block, Context, ErrorCode, InputStream, Message, OutputStream, Wafer, WaferError,
};

fn pool_guest_singleton_wasm() -> Vec<u8> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/pool_guest/target/wasm32-wasip1/release/pool_guest_singleton.wasm");
    std::fs::read(&p).unwrap_or_else(|e| {
        panic!(
            "failed to read pool guest wasm at {}: {e}\n\
             Did you build the fixtures first?\n  bash scripts/build-fixtures.sh",
            p.display()
        )
    })
}

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

async fn count_call(block: &WasmiBlock) -> String {
    let out = block
        .handle(
            &MockContext,
            Message::new("pool.count"),
            InputStream::empty(),
        )
        .await;
    match out.collect_buffered().await {
        Ok(buf) => String::from_utf8(buf.body).expect("utf8 counter"),
        Err(TerminalNotResponse::Error(e)) => panic!("pool.count errored: {e}"),
        Err(other) => panic!("unexpected terminal: {other:?}"),
    }
}

/// One test, four phases (order matters — shared process env):
///  1. `off` → a Singleton-declared guest still runs cold.
///  2. invalid → `WasmiBlock` load fails loud, naming the env var.
///  3. invalid → `Wafer::seal` refuses boot too (covers hosts with no wasm
///     block loaded yet — never a silent fallback).
///  4. unset → pooling engages for the declared block (the default).
#[tokio::test]
async fn kill_switch_and_invalid_values() {
    let wasm = pool_guest_singleton_wasm();

    // Phase 1: explicit off — declared mode is not honored; every call is
    // a fresh instance and nothing is pooled.
    std::env::set_var(WASM_POOLING_ENV, "off");
    let block = WasmiBlock::load_from_bytes(&wasm).expect("load with pooling off");
    assert_eq!(count_call(&block).await, "1");
    assert_eq!(
        count_call(&block).await,
        "1",
        "{WASM_POOLING_ENV}=off must force cold per-call instantiation"
    );
    assert_eq!(block.pooled_instance_count(), 0);

    // Phase 2: invalid value — load must fail loud, never silently pick a
    // behavior.
    std::env::set_var(WASM_POOLING_ENV, "sometimes");
    let Err(err) = WasmiBlock::load_from_bytes(&wasm) else {
        panic!("invalid kill-switch value must fail WasmiBlock load");
    };
    let msg = err.to_string();
    assert!(
        msg.contains(WASM_POOLING_ENV) && msg.contains("sometimes"),
        "load error must name the env var and echo the bad value: {msg}"
    );

    // Phase 3: invalid value — seal refuses boot even with no wasm blocks
    // registered (an operator's typo must never be inert-by-accident).
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("Wafer::build");
    let err = wafer
        .seal()
        .await
        .expect_err("invalid kill-switch value must refuse seal");
    let msg = err.to_string();
    assert!(
        msg.contains(WASM_POOLING_ENV),
        "seal error must name the env var: {msg}"
    );

    // Phase 4: unset — back to the default: pooling enabled for blocks
    // that declared a state-retaining InstanceMode.
    std::env::remove_var(WASM_POOLING_ENV);
    let block = WasmiBlock::load_from_bytes(&wasm).expect("load with default pooling");
    assert_eq!(count_call(&block).await, "1");
    assert_eq!(
        count_call(&block).await,
        "2",
        "with the switch unset, a Singleton-declared guest must pool"
    );
    assert_eq!(block.pooled_instance_count(), 1);
}
