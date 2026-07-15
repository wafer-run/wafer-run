//! Core-ABI version negotiation e2e (PERF-01).
//!
//! - A v2 guest (MessagePack frames; `bench_guest`, which hand-rolls the
//!   same glue the `#[wafer_block]` macro emits) round-trips a 1 MiB body
//!   under the PRODUCTION default resource limits — impossible under the
//!   v1 JSON framing, whose int-array byte encoding exhausted the default
//!   fuel budget well below 1 MiB (documented by the #290 bench suite).
//! - A guest declaring a future ABI version fails loud at dispatch instead
//!   of being silently mis-decoded as v2.
//!
//! The v1 fallback path needs no dedicated fixture: `tests/dispatch_guest`,
//! `tests/attachment_dispatch`, `tests/service_client_guest` and
//! `tests/hostile_db_guest` all hand-roll v1 JSON glue without a
//! `__wafer_abi_version` export, so their suites exercise v1 decoding on
//! every run, permanently.

use wafer_block::{ErrorCode, Message, WaferError};
use wafer_run::{
    streams::{input::InputStream, output::OutputStream},
    wasm::wasmi_loader::WasmiBlock,
};

#[derive(Clone)]
struct MockContext;

#[async_trait::async_trait]
impl wafer_run::context::Context for MockContext {
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

    fn clone_arc(&self) -> std::sync::Arc<dyn wafer_run::context::Context> {
        std::sync::Arc::new(self.clone())
    }
}

fn bench_guest_wasm() -> Vec<u8> {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("benches/fixtures/bench_guest/target/wasm32-wasip1/release/bench_guest.wasm");
    std::fs::read(&p).unwrap_or_else(|e| {
        panic!("missing bench_guest fixture at {p:?} ({e}) — run scripts/build-fixtures.sh")
    })
}

/// The PERF-01 functional gate: 1 MiB must round-trip through a v2 guest
/// under the production default limits (100M fuel / 16 MiB memory).
#[tokio::test]
async fn v2_guest_roundtrips_1mib_under_default_limits() {
    use wafer_block::Block;

    let block = WasmiBlock::load_from_bytes(&bench_guest_wasm()).expect("bench_guest loads");
    let body = vec![0xA5u8; 1024 * 1024];

    let out = block
        .handle(
            &MockContext,
            Message::new("bench.echo"),
            InputStream::from_bytes(body.clone()),
        )
        .await;

    let response = out
        .collect_buffered()
        .await
        .expect("1 MiB echo must succeed under default limits on ABI v2");
    assert_eq!(
        response.body, body,
        "guest must echo the exact 1 MiB body back"
    );
}

/// A guest exporting a `__wafer_abi_version` this host does not speak must
/// fail loud at dispatch — never be silently mis-decoded as v2.
#[tokio::test]
async fn guest_with_future_abi_version_errors_loud() {
    use wafer_block::Block;

    let wasm_bytes = wat::parse_str(
        r#"
        (module
          (memory (export "memory") 1)
          (func (export "__wafer_abi_version") (result i32) i32.const 99)
          (func (export "__wafer_alloc") (param i32) (result i32) i32.const 0)
          (func (export "__wafer_info") (result i64) i64.const 0)
          (func (export "__wafer_handle") (param i32 i32) (result i64) i64.const 0)
          (func (export "__wafer_lifecycle") (param i32 i32) (result i64) i64.const 0)
        )
        "#,
    )
    .expect("WAT should parse");

    let block = WasmiBlock::load_from_bytes(&wasm_bytes)
        .expect("module loads — the version is checked at dispatch");

    let out = block
        .handle(&MockContext, Message::new("any.op"), InputStream::empty())
        .await;
    match out.collect_buffered().await {
        Err(wafer_block::TerminalNotResponse::Error(e)) => {
            assert!(
                e.message.contains("ABI v99"),
                "error should name the unsupported version, got: {}",
                e.message
            );
        }
        other => panic!("expected an error terminal, got {other:?}"),
    }
}
