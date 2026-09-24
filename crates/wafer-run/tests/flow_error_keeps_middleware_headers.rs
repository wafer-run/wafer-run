//! A flow that stops early — a step's `Error`, `Halt` or `Drop`, or an error
//! the executor raises itself — keeps the response headers its completed
//! steps set, driven through the real `wafer-run/security-headers` and
//! `wafer-run/cors` blocks and rendered by the real HTTP codec
//! (`http_codec::collect_http_response`), the path every HTTP adapter takes.
//!
//! What is carried is exactly the flow message's `resp.header.*` /
//! `resp.set_cookie.*` entries when the flow stopped: not the stopping step's
//! own partial output, not what an earlier step removed, not a parallel
//! branch's message, and never `resp.status` / `resp.content_type`. The
//! terminal's own headers win.

use std::sync::Arc;

use wafer_block::{
    http_codec::{build_http_message, collect_http_response, HttpResponseParts},
    streams::output::TerminalNotResponse,
};
use wafer_block_cors::CorsBlock;
use wafer_block_security_headers::SecurityHeadersBlock;
use wafer_flow::WaferFlow;
use wafer_run::*;

const ORIGIN: &str = "https://a.example";

// ---------------------------------------------------------------------------
// Fixture blocks
// ---------------------------------------------------------------------------

fn meta(key: &str, value: &str) -> MetaEntry {
    MetaEntry {
        key: key.to_string(),
        value: value.to_string(),
    }
}

fn info(name: &str) -> BlockInfo {
    BlockInfo::new(name, "0.0.1", "http-handler@v1", "flow header fixture")
        .instance_mode(InstanceMode::Singleton)
}

/// Rejects the request as unauthenticated, setting its own
/// `WWW-Authenticate` and (in lowercase) an `x-frame-options` that must win
/// over the security-headers block's `X-Frame-Options`.
struct Unauthenticated;

#[async_trait::async_trait]
impl Block for Unauthenticated {
    fn info(&self) -> BlockInfo {
        info("test/unauthenticated")
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        let mut err = WaferError::new(ErrorCode::Unauthenticated, "sign in first");
        err.meta
            .push(meta("resp.header.WWW-Authenticate", "Bearer"));
        err.meta
            .push(meta("resp.header.x-frame-options", "SAMEORIGIN"));
        OutputStream::error(err)
    }
}

/// Answers `429` and halts the flow.
struct Throttle;

#[async_trait::async_trait]
impl Block for Throttle {
    fn info(&self) -> BlockInfo {
        info("test/throttle")
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::halt(
            b"slow down".to_vec(),
            vec![
                meta("resp.status", "429"),
                meta("resp.header.Retry-After", "5"),
            ],
        )
    }
}

/// Drops the request.
struct Dropper;

#[async_trait::async_trait]
impl Block for Dropper {
    fn info(&self) -> BlockInfo {
        info("test/dropper")
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::drop_request()
    }
}

/// Streams a header and part of a body, then fails.
struct PartialThenError;

#[async_trait::async_trait]
impl Block for PartialThenError {
    fn info(&self) -> BlockInfo {
        info("test/partial-then-error")
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        let (out, sink, _cancel) = OutputStream::new_streaming();
        sink.send_meta(meta("resp.header.X-Partial", "1"))
            .await
            .expect("consumer is alive");
        sink.send_chunk(b"half a bo".to_vec())
            .await
            .expect("consumer is alive");
        sink.error(WaferError::new(ErrorCode::Internal, "stream broke"))
            .await
            .expect("consumer is alive");
        out
    }
}

/// Middleware that removes the security-headers block's `X-Frame-Options`,
/// sets a session cookie, and sets a status and content type — which
/// describe a body this flow never produces.
struct Rewrite;

#[async_trait::async_trait]
impl Block for Rewrite {
    fn info(&self) -> BlockInfo {
        info("test/rewrite")
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let mut next = msg;
        next.meta
            .retain(|e| !e.key.eq_ignore_ascii_case("resp.header.X-Frame-Options"));
        next.set_meta("resp.set_cookie.sid", "sid=refreshed; Path=/; HttpOnly");
        next.set_meta("resp.status", "200");
        next.set_meta("resp.content_type", "text/html");
        OutputStream::continue_with(next)
    }
}

/// Middleware that marks the message with `X-Inner`.
struct Mark;

#[async_trait::async_trait]
impl Block for Mark {
    fn info(&self) -> BlockInfo {
        info("test/mark")
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let mut next = msg;
        next.set_meta("resp.header.X-Inner", "1");
        OutputStream::continue_with(next)
    }
}

/// Responds `200 ok` (must never be reached in the budget test).
struct Ok200;

#[async_trait::async_trait]
impl Block for Ok200 {
    fn info(&self) -> BlockInfo {
        info("test/ok")
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::respond(b"ok".to_vec())
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A runtime with the real middleware blocks, every fixture, and `flows`.
async fn start(flows: &[serde_json::Value]) -> Arc<Wafer> {
    let mut w = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer build is infallible");
    let blocks: Vec<(&str, Arc<dyn Block>)> = vec![
        (
            "wafer-run/security-headers",
            Arc::new(SecurityHeadersBlock::new()),
        ),
        ("wafer-run/cors", Arc::new(CorsBlock::new())),
        ("test/unauthenticated", Arc::new(Unauthenticated)),
        ("test/throttle", Arc::new(Throttle)),
        ("test/dropper", Arc::new(Dropper)),
        ("test/partial-then-error", Arc::new(PartialThenError)),
        ("test/rewrite", Arc::new(Rewrite)),
        ("test/mark", Arc::new(Mark)),
        ("test/ok", Arc::new(Ok200)),
    ];
    for (name, block) in blocks {
        w.register_block(name, block).expect("register fixture");
    }
    for flow in flows {
        let flow: WaferFlow = serde_json::from_value(flow.clone()).expect("valid flow JSON");
        w.add_flow(flow);
    }
    w.start().await.expect("start runtime")
}

fn security_headers_step() -> serde_json::Value {
    serde_json::json!({ "id": "security-headers", "block": "wafer-run/security-headers" })
}

fn cors_step() -> serde_json::Value {
    serde_json::json!({
        "id": "cors",
        "block": "wafer-run/cors",
        "config": { "allowed_origins": ORIGIN },
    })
}

fn flow(id: &str, steps: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "name": id,
        "version": "0.0.1",
        "steps": serde_json::Value::Array(steps),
        "config": { "on_error": "stop" },
    })
}

/// A cross-origin browser request, as the HTTP codec builds it.
fn cross_origin_request() -> Message {
    build_http_message("GET", "/b/thing", "", "127.0.0.1", [("Origin", ORIGIN)])
}

/// Every value of header `name` (case-insensitive) in `parts`.
fn header<'a>(parts: &'a HttpResponseParts, name: &str) -> Vec<&'a str> {
    parts
        .headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
        .collect()
}

/// The CORS and security headers the two middleware steps set.
fn assert_middleware_headers(parts: &HttpResponseParts) {
    assert_eq!(
        header(parts, "Access-Control-Allow-Origin"),
        vec![ORIGIN],
        "CORS header lost: {parts:?}"
    );
    assert_eq!(header(parts, "Vary"), vec!["Origin"], "{parts:?}");
    assert_eq!(
        header(parts, "X-Content-Type-Options"),
        vec!["nosniff"],
        "security header lost: {parts:?}"
    );
    assert_eq!(
        header(parts, "Content-Security-Policy").len(),
        1,
        "CSP lost: {parts:?}"
    );
}

async fn run_http(wafer: &Wafer, flow_id: &str) -> HttpResponseParts {
    collect_http_response(
        wafer
            .run(flow_id, cross_origin_request(), InputStream::empty())
            .await,
    )
    .await
}

// ---------------------------------------------------------------------------
// Error / Halt / Drop
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_error_keeps_the_cors_and_security_headers_of_the_steps_before_it() {
    let wafer = start(&[flow(
        "api",
        vec![
            security_headers_step(),
            cors_step(),
            serde_json::json!({ "id": "handler", "block": "test/unauthenticated" }),
        ],
    )])
    .await;

    let parts = run_http(&wafer, "api").await;

    assert_eq!(parts.status, 401);
    assert_middleware_headers(&parts);
    assert_eq!(header(&parts, "WWW-Authenticate"), vec!["Bearer"]);
    assert_eq!(
        header(&parts, "X-Frame-Options"),
        vec!["SAMEORIGIN"],
        "the error's own header must win, case-insensitively, and appear once"
    );
    assert_eq!(header(&parts, "Content-Type"), vec!["application/json"]);
}

#[tokio::test]
async fn a_halt_keeps_the_headers_and_its_own_status_and_body() {
    let wafer = start(&[flow(
        "api",
        vec![
            security_headers_step(),
            cors_step(),
            serde_json::json!({ "id": "limit", "block": "test/throttle" }),
            serde_json::json!({ "id": "never", "block": "test/ok" }),
        ],
    )])
    .await;

    let out = wafer
        .run("api", cross_origin_request(), InputStream::empty())
        .await;
    let Err(TerminalNotResponse::Halt(buf)) = out.collect_buffered().await else {
        panic!("the flow must still end in a Halt terminal");
    };
    let parts = wafer_block::http_codec::buffered_to_http_response(buf);

    assert_eq!(parts.status, 429);
    assert_eq!(parts.body, b"slow down");
    assert_middleware_headers(&parts);
    assert_eq!(header(&parts, "Retry-After"), vec!["5"]);
}

#[tokio::test]
async fn a_drop_keeps_the_headers_on_its_204() {
    let wafer = start(&[flow(
        "api",
        vec![
            security_headers_step(),
            cors_step(),
            serde_json::json!({ "id": "drop", "block": "test/dropper" }),
        ],
    )])
    .await;

    let out = wafer
        .run("api", cross_origin_request(), InputStream::empty())
        .await;
    let Err(TerminalNotResponse::Drop { meta }) = out.collect_buffered().await else {
        panic!("the flow must still end in a Drop terminal");
    };
    let parts = collect_http_response(OutputStream::drop_request_with_meta(meta)).await;

    assert_eq!(parts.status, 204);
    assert!(parts.body.is_empty());
    assert_middleware_headers(&parts);
    assert!(header(&parts, "Content-Type").is_empty(), "{parts:?}");
}

// ---------------------------------------------------------------------------
// Exactly which meta survives
// ---------------------------------------------------------------------------

/// Carried: what the completed steps left on the message (the CORS headers,
/// the refreshed cookie). Not carried: the header a completed step removed,
/// the failing step's own streamed header, and the status / content type a
/// middleware set.
#[tokio::test]
async fn only_what_the_completed_steps_left_on_the_message_is_carried() {
    let wafer = start(&[flow(
        "api",
        vec![
            security_headers_step(),
            cors_step(),
            serde_json::json!({ "id": "rewrite", "block": "test/rewrite" }),
            serde_json::json!({ "id": "handler", "block": "test/partial-then-error" }),
        ],
    )])
    .await;

    let parts = run_http(&wafer, "api").await;

    assert_eq!(
        parts.status, 500,
        "a middleware's resp.status must not mask the error"
    );
    assert_eq!(header(&parts, "Content-Type"), vec!["application/json"]);
    assert_middleware_headers(&parts);
    assert_eq!(
        header(&parts, "Set-Cookie"),
        vec!["sid=refreshed; Path=/; HttpOnly"]
    );
    assert!(
        header(&parts, "X-Frame-Options").is_empty(),
        "a header a completed step removed came back: {parts:?}"
    );
    assert!(
        header(&parts, "X-Partial").is_empty(),
        "the failing step's own partial output leaked: {parts:?}"
    );
}

/// The executor's own errors stop the flow too — here the step budget.
#[tokio::test]
async fn an_executor_error_keeps_the_headers() {
    let mut api = flow(
        "api",
        vec![
            security_headers_step(),
            cors_step(),
            serde_json::json!({ "id": "never", "block": "test/ok" }),
        ],
    );
    api["config"]["max_steps"] = serde_json::json!(2);
    let wafer = start(&[api]).await;

    let parts = run_http(&wafer, "api").await;

    assert_eq!(parts.status, 429, "{parts:?}");
    assert_middleware_headers(&parts);
}

// ---------------------------------------------------------------------------
// Nested flows
// ---------------------------------------------------------------------------

/// A `next` transfer hands the message, headers included, to the target
/// flow, whose own boundary carries them: each header appears once.
#[tokio::test]
async fn a_transferred_flow_error_keeps_the_outer_and_inner_headers() {
    let mut cors = cors_step();
    cors["next"] = serde_json::json!([{ "flow": "inner" }]);
    let wafer = start(&[
        flow("outer", vec![security_headers_step(), cors]),
        flow(
            "inner",
            vec![
                serde_json::json!({ "id": "mark", "block": "test/mark" }),
                serde_json::json!({ "id": "handler", "block": "test/unauthenticated" }),
            ],
        ),
    ])
    .await;

    let parts = run_http(&wafer, "outer").await;

    assert_eq!(parts.status, 401);
    assert_middleware_headers(&parts);
    assert_eq!(header(&parts, "X-Inner"), vec!["1"]);
}

/// A parallel branch's message changes are discarded at the join, on
/// failure as on success: the failure carries the headers set before the
/// parallel step, not the branch's.
#[tokio::test]
async fn a_failing_parallel_branch_carries_the_headers_from_before_the_fork() {
    let wafer = start(&[flow(
        "api",
        vec![
            security_headers_step(),
            serde_json::json!({
                "id": "fan",
                "block": "test/mark",
                "parallel": [{ "steps": [
                    cors_step(),
                    { "id": "handler", "block": "test/unauthenticated" },
                ] }],
            }),
        ],
    )])
    .await;

    let parts = run_http(&wafer, "api").await;

    assert_eq!(parts.status, 401);
    assert_eq!(header(&parts, "X-Content-Type-Options"), vec!["nosniff"]);
    assert!(
        header(&parts, "Access-Control-Allow-Origin").is_empty(),
        "a branch's message change escaped the branch: {parts:?}"
    );
    assert!(
        header(&parts, "X-Inner").is_empty(),
        "the parallel step's own block never ran: {parts:?}"
    );
}

// ---------------------------------------------------------------------------
// The WASM guest egress filter still applies
// ---------------------------------------------------------------------------

/// A guest's `Error` goes through the guest egress allowlist before the
/// executor sees it; carrying the host middleware's headers must not undo
/// that. The guest's forged `Access-Control-Allow-Origin: *`, `Location` and
/// cookie stay stripped, its harmless header passes, and the CORS header on
/// the response is the host middleware's.
#[cfg(feature = "wasm")]
#[tokio::test]
async fn a_guest_error_keeps_host_headers_and_stays_sanitized() {
    const GUEST: &str = "test/hostile-boundary-guest";
    let wasm_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(
        "tests/hostile_boundary_guest/target/wasm32-wasip1/release/hostile_boundary_guest.wasm",
    );
    let bytes = std::fs::read(&wasm_path).unwrap_or_else(|e| {
        panic!(
            "failed to read {}: {e}\nBuild the fixtures first: ./scripts/build-fixtures.sh",
            wasm_path.display()
        )
    });

    let mut w = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer build is infallible");
    w.register_block("wafer-run/cors", Arc::new(CorsBlock::new()))
        .expect("register cors");
    w.register_block(
        GUEST,
        Arc::new(WasmiBlock::load_from_bytes(&bytes).expect("load guest")),
    )
    .expect("register guest");
    w.add_flow(
        serde_json::from_value(flow(
            "api",
            vec![
                cors_step(),
                serde_json::json!({ "id": "guest", "block": GUEST }),
            ],
        ))
        .expect("valid flow JSON"),
    );
    let wafer = w.start().await.expect("start runtime");

    let mut msg = cross_origin_request();
    msg.kind = "test.error_with_headers".to_string();
    let out = wafer.run("api", msg, InputStream::empty()).await;
    let Err(TerminalNotResponse::Error(err)) = out.collect_buffered().await else {
        panic!("the guest must fail the flow");
    };
    let values = |key: &str| -> Vec<&str> {
        err.meta
            .iter()
            .filter(|e| e.key.eq_ignore_ascii_case(key))
            .map(|e| e.value.as_str())
            .collect()
    };

    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert_eq!(
        values("resp.header.access-control-allow-origin"),
        vec![ORIGIN],
        "the host CORS header must be the only ACAO: {:?}",
        err.meta
    );
    assert!(values("resp.set_cookie.s").is_empty(), "{:?}", err.meta);
    assert!(values("resp.header.location").is_empty(), "{:?}", err.meta);
    assert_eq!(values("resp.header.x-guest"), vec!["kept"]);
}
