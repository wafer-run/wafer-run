//! End-to-end: load a wasm guest using the new typed SDK clients, dispatch
//! against a fake network block emitting two-frame codec-encoded responses.
//!
//! The guest (`tests/dispatch_guest/`) exports `__wafer_handle` and selects
//! between buffered and streaming SDK paths via `msg.kind`. The
//! `FakeNetworkBlock` below decodes the request via the new wire format
//! (`codec::decode::<wire::network::Request>`) and emits responses as a
//! `ResponseHeader` chunk followed by one or more body chunks via
//! `OutputStream::from_producer` — exactly the contract the production
//! handler implements.
//!
//! These tests prove the full chain works:
//!
//!   guest → SDK clients → host streaming ABI → fake block → codec wire → SDK → guest

#![cfg(feature = "wasm")]

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use wafer_block::{
    codec,
    common::{ErrorCode, ServiceOp},
    streams::{input::InputStream, output::OutputStream},
    types::BlockInfo,
    wire::network::{Request as WireRequest, ResponseHeader},
    Block, Context, Message, WaferError,
};
use wafer_run::{wasm::WasmiBlock, Wafer};

// ---------------------------------------------------------------------------
// Guest discovery
// ---------------------------------------------------------------------------

/// Path to the prebuilt dispatch guest wasm. The guest crate has its own
/// `[workspace]` and is excluded from the parent workspace, so it must be
/// built separately:
///
/// ```bash
/// cargo build --target wasm32-wasip1 --release \
///     --manifest-path crates/wafer-run/tests/dispatch_guest/Cargo.toml
/// ```
///
/// `wasm32-wasip1` is the same target real skill blocks use (see
/// `wafer-cli/src/build.rs`), and matches the platform-spawn cfg in
/// `wafer-block::spawn` — on `target_os = "wasi"`, `OutputStream::from_producer`
/// is unavailable, but guest blocks return synchronously via `GuestResult`
/// and do not need it.
fn dispatch_guest_wasm() -> Vec<u8> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/dispatch_guest/target/wasm32-wasip1/release/dispatch_guest.wasm");
    std::fs::read(&p).unwrap_or_else(|e| {
        panic!(
            "failed to read dispatch guest wasm at {}: {e}\n\
             Did you build it first?\n  cargo build --target wasm32-wasip1 --release \\\n    --manifest-path crates/wafer-run/tests/dispatch_guest/Cargo.toml",
            p.display()
        )
    })
}

// ---------------------------------------------------------------------------
// FakeNetworkBlock — emits two-frame codec-encoded responses
// ---------------------------------------------------------------------------

/// Test stub for `wafer-run/network`. Decodes the new MessagePack-encoded
/// `wire::network::Request`, branches on URL, and emits responses as a
/// `ResponseHeader` frame followed by one or more body frames via
/// `OutputStream::from_producer` — mirroring the production handler in
/// `wafer-core::interfaces::network::handler`.
///
/// URL contract:
///   - `/buffered`  → single body chunk `b"hello world"`
///   - `/streaming` → three body chunks `b"chunk-a"`, `b"chunk-b"`, `b"chunk-c"`
///   - anything else → terminal error
struct FakeNetworkBlock;

#[async_trait]
impl Block for FakeNetworkBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/network",
            "0.1.0",
            "network@v1",
            "Test stub — emits two-frame codec-encoded responses",
        )
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        if msg.kind != ServiceOp::NETWORK_DO_REQUEST {
            return OutputStream::error(WaferError::new(
                ErrorCode::InvalidArgument,
                format!("FakeNetworkBlock: unexpected msg.kind {}", msg.kind),
            ));
        }

        let body = input.collect_to_bytes().await;
        let req: WireRequest = match codec::decode(&body) {
            Ok(r) => r,
            Err(e) => {
                return OutputStream::error(WaferError::new(
                    ErrorCode::InvalidArgument,
                    format!("FakeNetworkBlock: invalid wire::network::Request: {e}"),
                ));
            }
        };

        // Decide what to emit based on the URL path. Two flavours: a single
        // body chunk (buffered round-trip) or three body chunks (streaming
        // chunked delivery).
        let body_chunks: Vec<Vec<u8>> = if req.url.ends_with("/buffered") {
            vec![b"hello world".to_vec()]
        } else if req.url.ends_with("/streaming") {
            vec![
                b"chunk-a".to_vec(),
                b"chunk-b".to_vec(),
                b"chunk-c".to_vec(),
            ]
        } else {
            return OutputStream::error(WaferError::new(
                ErrorCode::NotFound,
                format!("FakeNetworkBlock: unknown URL {}", req.url),
            ));
        };

        let header = ResponseHeader {
            status_code: 200,
            headers: std::collections::HashMap::new(),
        };

        OutputStream::from_producer(move |sink, _cancel| async move {
            let header_bytes = match codec::encode(&header) {
                Ok(b) => b,
                Err(e) => {
                    let _ = sink
                        .error(WaferError::new(
                            ErrorCode::Internal,
                            format!("FakeNetworkBlock: encoding header: {e}"),
                        ))
                        .await;
                    return;
                }
            };
            if sink.send_chunk(header_bytes).await.is_err() {
                return;
            }
            for chunk in body_chunks {
                if sink.send_chunk(chunk).await.is_err() {
                    return;
                }
            }
            let _ = sink.complete(vec![]).await;
        })
    }
}

// ---------------------------------------------------------------------------
// RecorderBlock — echoes the meta it was called with (SEC-01 nested-call e2e)
// ---------------------------------------------------------------------------

/// Native callee for the SEC-01 nested-`call_block` test. It serializes the
/// metadata it *received* into its response body as sorted `key=value` lines,
/// so the outer test can assert exactly which identity crossed the boundary
/// when a hostile guest forged `auth.*` on the nested call.
struct RecorderBlock;

#[async_trait]
impl Block for RecorderBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test/recorder",
            "0.1.0",
            "recorder@v1",
            "Test callee — echoes received meta as its body",
        )
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let mut lines: Vec<String> = msg
            .meta
            .iter()
            .map(|e| format!("{}={}", e.key, e.value))
            .collect();
        lines.sort();
        OutputStream::respond(lines.join("\n").into_bytes())
    }
}

// ---------------------------------------------------------------------------
// Test harness
// ---------------------------------------------------------------------------

async fn dispatch(kind: &str, url: &str) -> Result<Vec<u8>, WaferError> {
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("Wafer::build");

    wafer
        .register_block("wafer-run/network", Arc::new(FakeNetworkBlock))
        .expect("register fake network");

    let wasm = dispatch_guest_wasm();
    let block = WasmiBlock::load_from_bytes(&wasm).expect("load dispatch_guest wasm");
    wafer
        .register_block("test/dispatch-guest", Arc::new(block))
        .expect("register dispatch-guest");

    let wafer = wafer.start().await.expect("start runtime");

    let output = wafer
        .run_block(
            "test/dispatch-guest",
            Message::new(kind),
            InputStream::from_bytes(url.as_bytes().to_vec()),
        )
        .await;

    output
        .collect_buffered()
        .await
        .map(|b| b.body)
        .map_err(|t| match t {
            wafer_block::streams::output::TerminalNotResponse::Error(e) => e,
            other => WaferError::new(
                ErrorCode::Internal,
                format!("non-error terminal: {other:?}"),
            ),
        })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn buffered_network_request_round_trips() {
    let body = dispatch("test.buffered", "https://example.test/buffered")
        .await
        .expect("buffered dispatch should succeed");
    assert_eq!(
        body, b"hello world",
        "buffered SDK helper should reconstruct the full body from the codec-encoded \
         ResponseHeader + single body frame emitted by FakeNetworkBlock"
    );
}

// SEC-01: a WASM guest cannot forge host-owned identity metadata (`auth.*`).
// The guest returns a Respond whose meta claims `auth.user_roles=admin`; the
// host must strip every `auth.*` key on egress (there is no upstream identity
// to restore here), while preserving the guest's non-protected meta.
#[tokio::test]
async fn guest_cannot_forge_host_owned_identity_metadata() {
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("Wafer::build");
    let wasm = dispatch_guest_wasm();
    let block = WasmiBlock::load_from_bytes(&wasm).expect("load dispatch_guest wasm");
    wafer
        .register_block("test/dispatch-guest", Arc::new(block))
        .expect("register dispatch-guest");
    let wafer = wafer.start().await.expect("start runtime");

    let out = wafer
        .run_block(
            "test/dispatch-guest",
            Message::new("test.forge_respond"),
            InputStream::empty(),
        )
        .await;

    let buf = out
        .collect_buffered()
        .await
        .expect("forge_respond should produce a buffered Respond");
    assert_eq!(buf.body, b"forged");
    for e in &buf.meta {
        assert!(
            !e.key.to_ascii_lowercase().starts_with("auth."),
            "forged host-owned identity leaked through the boundary: {}={}",
            e.key,
            e.value
        );
    }
    assert!(
        buf.meta
            .iter()
            .any(|e| e.key == "trace_id" && e.value == "t1"),
        "the guest's non-protected meta must be preserved"
    );
}

// SEC-01 (Continue path): a guest cannot forge or alter host-owned identity on
// the continuation message it hands to the next flow step. The host snapshots
// the inbound `auth.*` values and restores them over whatever the guest wrote,
// so a restricted guest sitting before a privileged block in a flow cannot
// escalate by rewriting the principal. Non-protected meta (`trace_id`) is left
// as the guest set it.
#[tokio::test]
async fn guest_cannot_forge_identity_on_continue() {
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("Wafer::build");
    let wasm = dispatch_guest_wasm();
    let block = WasmiBlock::load_from_bytes(&wasm).expect("load dispatch_guest wasm");
    wafer
        .register_block("test/dispatch-guest", Arc::new(block))
        .expect("register dispatch-guest");
    let wafer = wafer.start().await.expect("start runtime");

    // Host-established principal on the inbound message.
    let mut inbound = Message::new("test.forge_continue");
    inbound.set_meta("auth.user_id", "real-user");
    inbound.set_meta("auth.user_roles", "viewer");

    let out = wafer
        .run_block("test/dispatch-guest", inbound, InputStream::empty())
        .await;

    // The guest returned Continue; observe the continuation message.
    let msg = match out.collect_buffered().await {
        Err(wafer_block::streams::output::TerminalNotResponse::Continue(msg)) => msg,
        other => panic!("expected a Continue terminal, got {other:?}"),
    };
    let get = |k: &str| {
        msg.meta
            .iter()
            .find(|e| e.key == k)
            .map(|e| e.value.as_str())
    };
    assert_eq!(
        get("auth.user_id"),
        Some("real-user"),
        "host-owned auth.user_id must be restored, not the forged value"
    );
    assert_eq!(
        get("auth.user_roles"),
        Some("viewer"),
        "host-owned auth.user_roles must be restored, not the forged 'admin'"
    );
    assert_eq!(
        get("trace_id"),
        Some("t1"),
        "the guest's non-protected meta must survive"
    );
}

// SEC-01 (nested `call_block` path): a guest cannot forge host-owned identity
// on the message it streams to a callee via the host-call ABI. The callee
// (`RecorderBlock`) echoes the meta it received, and the outer test asserts the
// callee saw the host's inbound principal — never the forged `attacker`/`admin`.
#[tokio::test]
async fn guest_cannot_forge_identity_on_nested_call() {
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("Wafer::build");
    wafer
        .register_block("test/recorder", Arc::new(RecorderBlock))
        .expect("register recorder");
    let wasm = dispatch_guest_wasm();
    let block = WasmiBlock::load_from_bytes(&wasm).expect("load dispatch_guest wasm");
    wafer
        .register_block("test/dispatch-guest", Arc::new(block))
        .expect("register dispatch-guest");
    let wafer = wafer.start().await.expect("start runtime");

    let mut inbound = Message::new("test.forge_nested_call");
    inbound.set_meta("auth.user_id", "real-user");
    inbound.set_meta("auth.user_roles", "viewer");

    let out = wafer
        .run_block("test/dispatch-guest", inbound, InputStream::empty())
        .await;

    // The guest responds with the recorder's echoed body: the meta the callee
    // actually received, as sorted `key=value` lines.
    let buf = out
        .collect_buffered()
        .await
        .expect("nested-call dispatch should produce a buffered Respond");
    let recorded = String::from_utf8(buf.body).expect("recorder body is utf8");
    let lines: Vec<&str> = recorded.lines().collect();

    assert!(
        lines.contains(&"auth.user_id=real-user"),
        "callee must see the host principal, not the forged one: {recorded:?}"
    );
    assert!(
        lines.contains(&"auth.user_roles=viewer"),
        "callee must see the host role, not the forged 'admin': {recorded:?}"
    );
    assert!(
        !recorded.contains("attacker") && !lines.contains(&"auth.user_roles=admin"),
        "no forged identity may reach the callee: {recorded:?}"
    );
    assert!(
        lines.contains(&"trace_id=t2"),
        "the guest's non-protected meta must reach the callee: {recorded:?}"
    );
}

#[tokio::test]
async fn streaming_network_request_yields_chunks_in_order() {
    let body = dispatch("test.streaming", "https://example.test/streaming")
        .await
        .expect("streaming dispatch should succeed");
    // The guest concatenates each chunk yielded by `next_chunk()`. If the
    // SDK streaming wrapper preserves frame boundaries and order, the
    // concatenation is exactly `chunk-a` ++ `chunk-b` ++ `chunk-c`.
    assert_eq!(
        body,
        b"chunk-achunk-bchunk-c",
        "streaming SDK should yield each frame in order; got {:?}",
        String::from_utf8_lossy(&body)
    );
}
