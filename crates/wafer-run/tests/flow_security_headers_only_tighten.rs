//! A responding step cannot loosen the security headers the real
//! `wafer-run/security-headers` middleware set, on a completed flow or
//! across a `next` transfer; it can tighten them. Rendered by the real HTTP
//! codec (`http_codec::collect_http_response`), the path every HTTP adapter
//! takes.

use std::sync::Arc;

use wafer_block::http_codec::{build_http_message, collect_http_response, HttpResponseParts};
use wafer_block_security_headers::SecurityHeadersBlock;
use wafer_flow::WaferFlow;
use wafer_run::*;

fn meta(key: &str, value: &str) -> MetaEntry {
    MetaEntry {
        key: key.to_string(),
        value: value.to_string(),
    }
}

/// Responds `200` with its own `headers`.
struct Responder {
    name: &'static str,
    headers: &'static [(&'static str, &'static str)],
}

#[async_trait::async_trait]
impl Block for Responder {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(self.name, "0.0.1", "http-handler@v1", "header fixture")
            .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::respond_with_meta(
            b"ok".to_vec(),
            self.headers
                .iter()
                .map(|(name, value)| meta(&format!("resp.header.{name}"), value))
                .collect(),
        )
    }
}

/// Every header the security-headers block sets, each looser than its
/// value (in lower case, so identity is by name, not key spelling).
const LOOSER: &[(&str, &str)] = &[
    ("x-frame-options", "SAMEORIGIN"),
    ("content-security-policy", "default-src * 'unsafe-eval'"),
    ("x-content-type-options", "sniff"),
    ("referrer-policy", "unsafe-url"),
    ("strict-transport-security", "max-age=0"),
    // `usb` is a feature the block does not name.
    (
        "permissions-policy",
        "camera=*, microphone=*, geolocation=*, usb=*",
    ),
];

/// Stricter than the block's: a download served sandboxed, never sending a
/// referrer, pinning HTTPS for longer (without restating the block's
/// `includeSubDomains; preload`).
const STRICTER: &[(&str, &str)] = &[
    ("Content-Security-Policy", "default-src 'none'; sandbox"),
    ("Referrer-Policy", "no-referrer"),
    ("Strict-Transport-Security", "max-age=63072000"),
];

async fn start(flows: &[serde_json::Value]) -> Arc<Wafer> {
    let mut w = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer build is infallible");
    w.register_block(
        "wafer-run/security-headers",
        Arc::new(SecurityHeadersBlock::new()),
    )
    .expect("register security-headers");
    for (name, headers) in [
        ("test/plain", &[][..]),
        ("test/looser", LOOSER),
        ("test/stricter", STRICTER),
    ] {
        w.register_block(name, Arc::new(Responder { name, headers }))
            .expect("register fixture");
    }
    for flow in flows {
        let flow: WaferFlow = serde_json::from_value(flow.clone()).expect("valid flow JSON");
        w.add_flow(flow).unwrap();
    }
    w.start().await.expect("start runtime")
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

fn step(id: &str, block: &str) -> serde_json::Value {
    serde_json::json!({ "id": id, "block": block })
}

fn security_headers() -> serde_json::Value {
    step("security-headers", "wafer-run/security-headers")
}

async fn run_http(wafer: &Wafer, flow_id: &str) -> HttpResponseParts {
    let request = build_http_message("GET", "/b/thing", "", "127.0.0.1", [("Accept", "*/*")]);
    collect_http_response(wafer.run(flow_id, request, InputStream::empty()).await).await
}

/// The one value of header `name` (case-insensitive) in `parts`.
fn header(parts: &HttpResponseParts, name: &str) -> String {
    let values: Vec<&str> = parts
        .headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
        .collect();
    assert_eq!(values.len(), 1, "exactly one {name}: {parts:?}");
    values[0].to_string()
}

/// The block's own values, from a flow whose responder sets none.
async fn middleware_values(wafer: &Wafer) -> HttpResponseParts {
    let parts = run_http(wafer, "plain").await;
    assert_eq!(parts.status, 200, "{parts:?}");
    parts
}

#[tokio::test]
async fn a_responder_cannot_loosen_the_middlewares_security_headers() {
    let wafer = start(&[
        flow("plain", vec![security_headers(), step("r", "test/plain")]),
        flow("looser", vec![security_headers(), step("r", "test/looser")]),
    ])
    .await;
    let middleware = middleware_values(&wafer).await;

    let parts = run_http(&wafer, "looser").await;

    assert_eq!(parts.status, 200, "{parts:?}");
    for name in [
        "X-Frame-Options",
        "X-Content-Type-Options",
        "Referrer-Policy",
        "Strict-Transport-Security",
    ] {
        assert_eq!(
            header(&parts, name),
            header(&middleware, name),
            "{name}: {parts:?}"
        );
    }
    // Both policies are sent, and a browser enforces each: the responder's
    // cannot admit what the block's refuses.
    assert_eq!(
        header(&parts, "Content-Security-Policy"),
        format!(
            "{}, default-src * 'unsafe-eval'",
            header(&middleware, "Content-Security-Policy")
        )
    );
    assert_eq!(
        header(&parts, "Permissions-Policy"),
        "camera=(), microphone=(), geolocation=(), usb=(self)"
    );
}

#[tokio::test]
async fn a_responder_can_tighten_the_middlewares_security_headers() {
    let wafer = start(&[
        flow("plain", vec![security_headers(), step("r", "test/plain")]),
        flow(
            "stricter",
            vec![security_headers(), step("r", "test/stricter")],
        ),
    ])
    .await;
    let middleware = middleware_values(&wafer).await;

    let parts = run_http(&wafer, "stricter").await;

    assert_eq!(header(&parts, "Referrer-Policy"), "no-referrer");
    assert_eq!(
        header(&parts, "Strict-Transport-Security"),
        "max-age=63072000; includeSubDomains; preload"
    );
    assert_eq!(
        header(&parts, "Content-Security-Policy"),
        format!(
            "{}, default-src 'none'; sandbox",
            header(&middleware, "Content-Security-Policy")
        )
    );
}

/// A `next` transfer's target is held to the transferring flow's
/// middleware headers too.
#[tokio::test]
async fn a_transfer_target_cannot_loosen_the_middlewares_security_headers() {
    let mut outer_headers = security_headers();
    outer_headers["next"] = serde_json::json!([{ "flow": "inner" }]);
    let wafer = start(&[
        flow("plain", vec![security_headers(), step("r", "test/plain")]),
        flow("outer", vec![outer_headers]),
        flow("inner", vec![step("r", "test/looser")]),
    ])
    .await;
    let middleware = middleware_values(&wafer).await;

    let parts = run_http(&wafer, "outer").await;

    assert_eq!(parts.status, 200, "{parts:?}");
    assert_eq!(header(&parts, "X-Frame-Options"), "DENY");
    assert_eq!(
        header(&parts, "Referrer-Policy"),
        header(&middleware, "Referrer-Policy")
    );
    assert!(
        header(&parts, "Content-Security-Policy")
            .starts_with(&header(&middleware, "Content-Security-Policy")),
        "{parts:?}"
    );
}
