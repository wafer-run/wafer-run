//! A runtime configured the way a trusted single-user host configures it —
//! unmetered fuel, lazy translation — loads a real guest and runs it.
//!
//! The mode itself reaching wasmi, metered + lazy being refused, and an
//! invalid module failing at load are covered by the loader's unit tests
//! (`wasm::wasmi_loader`), which can observe the engine's fuel directly.
//! This test covers the embedder's path end to end: the builder setting is
//! read back through `Wafer::resource_limits`, a compiled Rust guest is
//! loaded with those limits, and its `info()` and `handle()` work.
//!
//! The fixture is `tests/pool_guest/`, built by `scripts/build-fixtures.sh`.

#![cfg(feature = "wasm")]

use std::{path::PathBuf, sync::Arc};

use wafer_block::streams::output::TerminalNotResponse;
use wafer_run::{
    wasm::WasmiBlock, Block, Context, ErrorCode, FuelLimit, InputStream, Message, OutputStream,
    Wafer, WaferError, WasmCompilation,
};

fn pool_guest_wasm() -> Vec<u8> {
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

    fn resource_access_admitted(
        &self,
        _resource: &str,
        _resource_type: wafer_block::types::ResourceType,
        _access: wafer_block::types::ResourceAccess,
    ) -> bool {
        false
    }
}

async fn call(block: &WasmiBlock, kind: &str) -> String {
    let out = block
        .handle(&MockContext, Message::new(kind), InputStream::empty())
        .await;
    match out.collect_buffered().await {
        Ok(buf) => String::from_utf8(buf.body).expect("pool-guest responds UTF-8"),
        Err(TerminalNotResponse::Error(e)) => panic!("guest call failed: {e:?}"),
        Err(other) => panic!("unexpected non-error terminal: {other:?}"),
    }
}

#[tokio::test]
async fn unmetered_lazy_translation_runtime_runs_a_guest() {
    let wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .fuel_per_call(FuelLimit::Unmetered)
        .wasm_compilation(WasmCompilation::LazyTranslation)
        .build()
        .expect("unmetered fuel accepts lazy translation");
    let limits = wafer.resource_limits();
    assert_eq!(limits.fuel, FuelLimit::Unmetered);
    assert_eq!(
        limits.compilation,
        WasmCompilation::LazyTranslation,
        "the builder's compilation mode must reach the limits direct loaders use"
    );

    let block = WasmiBlock::load_approving_declaration(&pool_guest_wasm(), limits)
        .expect("a valid guest loads under lazy translation");
    assert_eq!(block.info().name, "test/pool-guest");
    // Two calls on the pooled instance: the second reaches functions the
    // first already translated, and the guest's state carries over.
    assert_eq!(call(&block, "pool.count").await, "1");
    assert_eq!(call(&block, "pool.count").await, "2");
}
