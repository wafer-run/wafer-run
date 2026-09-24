//! Cookies written through `wafer_block::response` — `cookie_meta` on a
//! middleware's message, `ResponseBuilder::set_cookie` on a response — are
//! keyed by the cookie they set, so producers that never coordinate cannot
//! overwrite each other's cookie, whether the merge is a middleware's own
//! `Message::set_meta` or the flow executor's. Rendered by the real HTTP
//! codec (`http_codec::collect_http_response`).

use std::sync::Arc;

use wafer_block::{
    http_codec::{build_http_message, collect_http_response},
    response::{cookie_meta, ResponseBuilder},
};
use wafer_flow::WaferFlow;
use wafer_run::*;

fn info(name: &str) -> BlockInfo {
    BlockInfo::new(name, "0.0.1", "http-handler@v1", "cookie key fixture")
        .instance_mode(InstanceMode::Singleton)
}

/// Middleware that sets one cookie on the flow message and continues.
struct CookieMiddleware(&'static str, &'static str);

#[async_trait::async_trait]
impl Block for CookieMiddleware {
    fn info(&self) -> BlockInfo {
        info(self.0)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let mut next = msg;
        let entry = cookie_meta(self.1);
        next.set_meta(entry.key, entry.value);
        OutputStream::continue_with(next)
    }
}

/// Responds through `ResponseBuilder` with one cookie.
struct CookieResponder;

#[async_trait::async_trait]
impl Block for CookieResponder {
    fn info(&self) -> BlockInfo {
        info("test/responder")
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        ResponseBuilder::new()
            .set_cookie("theme=dark; Path=/")
            .body(b"ok".to_vec(), "text/plain")
    }
}

#[tokio::test]
async fn cookies_from_independent_producers_all_reach_the_response() {
    let mut w = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer build is infallible");
    let blocks: Vec<(&str, Arc<dyn Block>)> = vec![
        (
            "test/session",
            Arc::new(CookieMiddleware("test/session", "sid=s1; Path=/; HttpOnly")),
        ),
        (
            "test/consent",
            Arc::new(CookieMiddleware("test/consent", "consent=yes; Path=/")),
        ),
        ("test/responder", Arc::new(CookieResponder)),
    ];
    for (name, block) in blocks {
        w.register_block(name, block).expect("register fixture");
    }
    let flow: WaferFlow = serde_json::from_value(serde_json::json!({
        "id": "site",
        "name": "site",
        "version": "0.0.1",
        "steps": [
            { "id": "session", "block": "test/session" },
            { "id": "consent", "block": "test/consent" },
            { "id": "page", "block": "test/responder" },
        ],
    }))
    .expect("valid flow JSON");
    w.add_flow(flow).unwrap();
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

    let cookies: Vec<&str> = parts
        .headers
        .iter()
        .filter(|(name, _)| name == "Set-Cookie")
        .map(|(_, value)| value.as_str())
        .collect();
    assert_eq!(parts.status, 200, "{parts:?}");
    assert_eq!(
        cookies,
        vec![
            "sid=s1; Path=/; HttpOnly",
            "consent=yes; Path=/",
            "theme=dark; Path=/",
        ],
        "every producer's cookie must survive: {parts:?}"
    );
}
