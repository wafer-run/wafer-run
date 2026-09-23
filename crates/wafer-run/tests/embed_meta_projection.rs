//! The embedder wire format emits only response meta — never request state.
//!
//! `embed::output_to_json` is what `wafer-ffi`, `wafer-run-node` and the Go
//! SDK receive. Blocks legitimately build a terminal from the request
//! message (it is what carries the CORS and security headers a middleware
//! set), so every arm's meta can hold `http.header.authorization`,
//! `http.header.cookie`, `auth.*` identity, `req.client.ip` and the decoded
//! query. The native HTTP boundary has always projected that down to the
//! canonical `resp.*` keys via `http_codec`; these tests pin the embedder
//! boundary to the same projection, arm by arm.

use std::sync::Arc;

use serde_json::Value;
use wafer_run::{
    embed::output_to_json, streams::output::OutputStream, InputStream, Message, Wafer,
};
use wafer_test_support::builder::WaferBuilder;

/// A request message as the HTTP codec really builds one, so the meta a leak
/// would expose is the meta a real request carries — not an invented key set.
fn request_with_secrets(method: &str) -> Message {
    let mut msg = wafer_block::http_codec::build_http_message(
        method,
        "/orgs/42",
        "token=SECRET_QUERY",
        "203.0.113.9",
        [
            ("Authorization", "Bearer SECRET_TOKEN"),
            ("Cookie", "session=SECRET_COOKIE"),
            ("Origin", "https://app.example.com"),
        ],
    );
    // Identity an auth middleware establishes upstream of the block that
    // produces the terminal.
    msg.set_meta("auth.user_email", "someone@example.com");
    msg.set_meta("auth.user_roles", "owner");
    msg
}

/// Every distinctive value [`request_with_secrets`] puts on the request.
const REQUEST_SECRETS: [&str; 6] = [
    "SECRET_TOKEN",
    "SECRET_COOKIE",
    "SECRET_QUERY",
    "203.0.113.9",
    "someone@example.com",
    "owner",
];

/// Assert the encoded terminal carries no request state: no secret value,
/// and every emitted meta key is a canonical response key.
fn assert_only_response_meta(raw: &str) {
    for secret in REQUEST_SECRETS {
        assert!(
            !raw.contains(secret),
            "{secret} leaked into the embedder JSON: {raw}"
        );
    }
    let json: Value = serde_json::from_str(raw).expect("valid JSON");
    let meta = json["meta"]
        .as_object()
        .unwrap_or_else(|| panic!("`meta` object present: {raw}"));
    for key in meta.keys() {
        assert!(
            key == "resp.status"
                || key == "resp.content_type"
                || key.starts_with("resp.header.")
                || key.starts_with("resp.cookie."),
            "non-response meta key `{key}` reached the embedder: {raw}"
        );
    }
}

#[tokio::test]
async fn respond_emits_only_response_meta() {
    let mut msg = request_with_secrets("OPTIONS");
    msg.set_meta("resp.header.X-Request-Id", "req-1");
    let raw = output_to_json(OutputStream::respond_with_meta(b"ok".to_vec(), msg.meta)).await;
    assert_only_response_meta(&raw);
    let json: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(json["meta"]["resp.header.X-Request-Id"], "req-1");
}

#[tokio::test]
async fn halt_emits_only_response_meta() {
    let mut msg = request_with_secrets("OPTIONS");
    msg.set_meta("resp.status", "413");
    msg.set_meta("resp.content_type", "text/plain; charset=utf-8");
    let raw = output_to_json(OutputStream::halt(b"too large".to_vec(), msg.meta)).await;
    assert_only_response_meta(&raw);
    let json: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(json["meta"]["resp.status"], "413");
    assert_eq!(
        json["meta"]["resp.content_type"],
        "text/plain; charset=utf-8"
    );
}

#[tokio::test]
async fn continue_emits_only_response_meta() {
    let mut msg = request_with_secrets("OPTIONS");
    msg.set_meta("resp.header.Vary", "Origin");
    let raw = output_to_json(OutputStream::continue_with(msg)).await;
    assert_only_response_meta(&raw);
    let json: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(json["kind"], "OPTIONS:/orgs/42");
    assert_eq!(json["meta"]["resp.header.Vary"], "Origin");
}

/// An error built from the request message carries the request's meta. The
/// leak assertions in [`assert_only_response_meta`] run first, so an `error`
/// arm that emitted the error's meta unfiltered fails here on the leaked
/// secret, not merely on a missing field.
#[tokio::test]
async fn error_emits_only_response_meta() {
    let mut msg = request_with_secrets("OPTIONS");
    msg.set_meta("resp.header.Retry-After", "30");
    let raw = output_to_json(OutputStream::error(wafer_run::WaferError {
        code: wafer_run::ErrorCode::ResourceExhausted,
        message: "Too many requests".into(),
        meta: msg.meta,
    }))
    .await;
    assert_only_response_meta(&raw);
    let json: Value = serde_json::from_str(&raw).unwrap();
    // The rate-limit headers the native HTTP boundary emits reach an
    // embedding host too: the projection keeps the two boundaries at parity,
    // it does not blank the field.
    assert_eq!(json["meta"]["resp.header.Retry-After"], "30");
    assert_eq!(json["error"]["code"], "ResourceExhausted");
}

/// The same projection through the real runtime and a real block:
/// `wafer-run/cors` answers an OPTIONS preflight with
/// `OutputStream::halt(_, out_msg.meta)` — the request message's whole meta —
/// and continues every other method carrying the same meta.
async fn cors_wafer() -> Arc<Wafer> {
    WaferBuilder::new()
        .with_block(
            "wafer-run/cors",
            Arc::new(wafer_block_cors::CorsBlock::new()),
        )
        .with_config(
            "wafer-run/cors",
            serde_json::json!({ "allowed_origins": "https://app.example.com" }),
        )
        .build()
        .await
        .expect("build")
}

#[tokio::test]
async fn cors_preflight_halt_emits_only_response_meta() {
    let wafer = cors_wafer().await;
    let out = wafer
        .run_block(
            "wafer-run/cors",
            request_with_secrets("OPTIONS"),
            InputStream::empty(),
        )
        .await;
    let raw = output_to_json(out).await;
    assert_only_response_meta(&raw);
    let json: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(json["action"], "halt");
    assert_eq!(
        json["meta"]["resp.header.Access-Control-Allow-Origin"], "https://app.example.com",
        "the CORS headers the block set must still reach the host: {raw}"
    );
    assert_eq!(json["meta"]["resp.status"], "204");
}

#[tokio::test]
async fn cors_continue_emits_only_response_meta() {
    let wafer = cors_wafer().await;
    let out = wafer
        .run_block(
            "wafer-run/cors",
            request_with_secrets("GET"),
            InputStream::empty(),
        )
        .await;
    let raw = output_to_json(out).await;
    assert_only_response_meta(&raw);
    let json: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(json["action"], "continue");
    assert_eq!(
        json["meta"]["resp.header.Access-Control-Allow-Origin"],
        "https://app.example.com"
    );
}
