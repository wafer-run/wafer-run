//! A step whose response meta no transport can send fails the flow closed:
//! the executor refuses it before laying it over the flow message, so a
//! responder's malformed `Content-Security-Policy` cannot displace the
//! `wafer-run/security-headers` middleware's, and the client gets a 500 that
//! still carries the middleware's headers — never a page with no CSP.
//! Rendered by the real HTTP codec (`http_codec::collect_http_response`).

use std::sync::Arc;

use wafer_block::http_codec::{build_http_message, collect_http_response, HttpResponseParts};
use wafer_block_security_headers::SecurityHeadersBlock;
use wafer_flow::WaferFlow;
use wafer_run::*;

/// Responds with a page whose CSP holds a smart quote (U+2019).
struct SmartQuoteCsp;

#[async_trait::async_trait]
impl Block for SmartQuoteCsp {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/page", "0.0.1", "http-handler@v1", "bad CSP fixture")
            .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::respond_with_meta(
            b"<p>page</p>".to_vec(),
            vec![
                MetaEntry {
                    key: "resp.header.Content-Security-Policy".into(),
                    value: "script-src \u{2019}self\u{2019}".into(),
                },
                MetaEntry {
                    key: "resp.content_type".into(),
                    value: "text/html".into(),
                },
            ],
        )
    }
}

fn header<'a>(parts: &'a HttpResponseParts, name: &str) -> Vec<&'a str> {
    parts
        .headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
        .collect()
}

#[tokio::test]
async fn a_malformed_responder_csp_is_a_500_keeping_the_middleware_csp() {
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
    w.register_block("test/page", Arc::new(SmartQuoteCsp))
        .expect("register fixture");
    let flow: WaferFlow = serde_json::from_value(serde_json::json!({
        "id": "site",
        "name": "site",
        "version": "0.0.1",
        "steps": [
            { "id": "security-headers", "block": "wafer-run/security-headers" },
            { "id": "page", "block": "test/page" },
        ],
    }))
    .expect("valid flow JSON");
    w.add_flow(flow);
    let wafer = w.start().await.expect("start runtime");

    let parts = collect_http_response(
        wafer
            .run(
                "site",
                build_http_message("GET", "/", "", "127.0.0.1", [("Host", "a.example")]),
                InputStream::empty(),
            )
            .await,
    )
    .await;

    assert_eq!(parts.status, 500, "{parts:?}");
    let csp = header(&parts, "Content-Security-Policy");
    assert_eq!(csp.len(), 1, "the middleware's CSP must survive: {parts:?}");
    assert!(
        csp[0].contains("frame-ancestors"),
        "the baseline CSP, not the responder's: {parts:?}"
    );
    assert!(csp[0].is_ascii(), "{parts:?}");
    assert_eq!(header(&parts, "X-Content-Type-Options"), vec!["nosniff"]);
    assert_eq!(header(&parts, "Content-Type"), vec!["application/json"]);
    assert_ne!(parts.body, b"<p>page</p>");
}
