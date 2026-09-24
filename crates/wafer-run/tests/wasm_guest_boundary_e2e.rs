//! The WASM guest boundary, end to end: a real compiled guest
//! (`tests/hostile_boundary_guest/`, built against the public `wafer-sdk`)
//! dispatched through the real `Wafer` runtime and `WasmiBlock`.
//!
//! - Into the guest: request meta built by the real HTTP codec
//!   (`http_codec::build_http_message`) — the only producer of request
//!   headers — loses `Cookie`, and keeps `Authorization` only because the
//!   guest declares it in `HeaderPolicy.readable`.
//! - Out of the guest: an `Error` result and a `Continue` message go through
//!   the same `HeaderPolicy.writable` allowlist as a `Respond` result, and a
//!   `Continue` cannot drop or forge the request headers the guest may not
//!   write.
//! - `stream_init` is charged against the per-call host-byte budget.

#![cfg(feature = "wasm")]

use std::{path::PathBuf, sync::Arc};

use wafer_block::{
    http_codec::build_http_message,
    streams::{input::InputStream, output::TerminalNotResponse},
    ErrorCode, Message, MetaEntry,
};
use wafer_run::{wasm::WasmiBlock, FuelLimit, ResourceLimits, Wafer};

const GUEST: &str = "test/hostile-boundary-guest";

fn guest_wasm() -> Vec<u8> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/hostile_boundary_guest/target/wasm32-wasip1/release/hostile_boundary_guest.wasm");
    std::fs::read(&p).unwrap_or_else(|e| {
        panic!(
            "failed to read hostile-boundary-guest wasm at {}: {e}\n\
             Build the fixtures first: ./scripts/build-fixtures.sh",
            p.display()
        )
    })
}

/// A started runtime with the guest registered under its own name, loaded
/// with `limits`. `seal` gives the guest its declared capabilities.
async fn start_with(limits: ResourceLimits) -> Arc<Wafer> {
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("Wafer::build");
    let block = WasmiBlock::load_approving_declaration(&guest_wasm(), limits)
        .expect("load hostile-boundary-guest wasm");
    wafer
        .register_block(GUEST, Arc::new(block))
        .expect("register hostile-boundary-guest");
    wafer.start().await.expect("start runtime")
}

async fn start() -> Arc<Wafer> {
    start_with(ResourceLimits::default()).await
}

/// A browser request to the guest's route carrying a session cookie and a
/// bearer token, as the HTTP codec turns it into a message.
fn request_with_credentials(path: &str) -> Message {
    build_http_message(
        "GET",
        path,
        "",
        "127.0.0.1",
        [
            ("Cookie", "session=admin-session"),
            ("Authorization", "Bearer user-token"),
            ("Accept", "text/html"),
        ],
    )
}

fn get<'a>(meta: &'a [MetaEntry], key: &str) -> Option<&'a str> {
    meta.iter().find(|e| e.key == key).map(|e| e.value.as_str())
}

fn assert_no_hostile_response_headers(meta: &[MetaEntry]) {
    for key in [
        "resp.set_cookie.s",
        "resp.header.location",
        "resp.header.access-control-allow-origin",
    ] {
        assert_eq!(get(meta, key), None, "{key} crossed the boundary: {meta:?}");
    }
    assert_eq!(
        get(meta, "resp.header.x-guest"),
        Some("kept"),
        "a non-sensitive header must pass: {meta:?}"
    );
}

#[tokio::test]
async fn guest_sees_only_the_request_headers_it_declares() {
    let wafer = start().await;
    let out = wafer
        .run_block(
            GUEST,
            request_with_credentials("/echo_meta"),
            InputStream::empty(),
        )
        .await
        .collect_buffered()
        .await
        .unwrap_or_else(|e| panic!("echo_meta must respond: {e:?}"));
    let seen: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&out.body).expect("guest echoes its meta as JSON");

    assert_eq!(
        seen.get("http.header.cookie"),
        None,
        "the session cookie reached a guest that did not declare it: {seen:?}"
    );
    assert_eq!(
        seen.get("http.header.authorization")
            .and_then(|v| v.as_str()),
        Some("Bearer user-token"),
        "the guest declares `authorization` in headers.readable"
    );
    assert_eq!(
        seen.get("http.header.accept").and_then(|v| v.as_str()),
        Some("text/html")
    );
}

/// The declaration is data an operator can read: the guest's `BlockInfo`
/// (what the inspector serves at `/blocks/{name}`) and the runtime's
/// effective capabilities both carry it.
#[tokio::test]
async fn the_readable_declaration_is_visible_to_operators() {
    let wafer = start().await;
    let info = wafer
        .block_infos()
        .into_iter()
        .find(|b| b.name == GUEST)
        .expect("guest is registered");
    let declared = info.capabilities.expect("guest declares capabilities");
    assert_eq!(declared.headers.readable, vec!["authorization".to_string()]);
    let effective = wafer
        .effective_capabilities(GUEST)
        .expect("guest has effective capabilities");
    assert_eq!(
        effective.headers.readable,
        vec!["authorization".to_string()]
    );
    assert!(effective.headers.writable.is_empty());
}

#[tokio::test]
async fn an_error_result_goes_through_the_outbound_header_allowlist() {
    let wafer = start().await;
    let out = wafer
        .run_block(
            GUEST,
            Message::new("test.error_with_headers"),
            InputStream::empty(),
        )
        .await
        .collect_buffered()
        .await;
    let Err(TerminalNotResponse::Error(err)) = out else {
        panic!("expected an Error terminal, got {out:?}");
    };
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert_no_hostile_response_headers(&err.meta);
}

#[tokio::test]
async fn a_continue_message_goes_through_the_allowlist_and_keeps_host_headers() {
    let wafer = start().await;
    let mut msg = request_with_credentials("/next");
    msg.kind = "test.continue_with_headers".to_string();
    let out = wafer
        .run_block(GUEST, msg, InputStream::empty())
        .await
        .collect_buffered()
        .await;
    let Err(TerminalNotResponse::Continue(next)) = out else {
        panic!("expected a Continue terminal, got {out:?}");
    };
    assert_no_hostile_response_headers(&next.meta);
    // The next step sees the request's own credentials, not the guest's
    // forgeries — and not the absence of the cookie the guest never saw.
    assert_eq!(
        get(&next.meta, "http.header.cookie"),
        Some("session=admin-session")
    );
    assert_eq!(
        get(&next.meta, "http.header.authorization"),
        Some("Bearer user-token")
    );
    assert_eq!(
        next.meta
            .iter()
            .filter(|e| e.key == "http.header.cookie" || e.key == "http.header.authorization")
            .count(),
        2,
        "each credential header exactly once: {:?}",
        next.meta
    );
}

/// Every `stream_init` copies its message out of guest memory into a stream
/// that lives until it closes, so the copies are charged against the per-call
/// host-byte budget: with a 16 MiB budget, 1 MiB messages are refused well
/// before the guest's 64 attempts (or the live-stream cap) run out.
#[tokio::test]
async fn stream_init_is_charged_against_the_host_byte_budget() {
    let wafer = start_with(ResourceLimits {
        fuel: FuelLimit::Unmetered,
        max_host_bytes: 16 * 1024 * 1024,
        max_live_streams: 64,
        ..ResourceLimits::default()
    })
    .await;
    let out = wafer
        .run_block(
            GUEST,
            Message::new("test.stream_init_flood"),
            InputStream::empty(),
        )
        .await
        .collect_buffered()
        .await
        .unwrap_or_else(|e| panic!("stream_init_flood must respond: {e:?}"));
    let report: serde_json::Value = serde_json::from_slice(&out.body).expect("JSON report");
    assert_eq!(
        report["refused"].as_str(),
        Some("ResourceExhausted"),
        "stream_init was never refused: {report}"
    );
    let opened = report["opened"].as_u64().expect("opened count");
    assert!(
        opened < 16,
        "16 MiB budget admitted {opened} one-MiB stream messages"
    );
}
