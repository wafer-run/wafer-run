//! Static file server block for wafer-run.
//!
//! Provides `wafer-run/web`, an HTTP-handler block that serves assets out of
//! a `wafer-run/storage` folder with sensible defaults for caching, clean
//! URLs, SPA fallback, and MIME-type detection. The block is registered into
//! the runtime at link time via [`wafer_block::register_static_block!`]; consumers
//! enable it by depending on this crate and supplying a `wafer-run/web`
//! block-config entry.

#![warn(missing_docs)]

use std::{path::Path, sync::OnceLock};

use wafer_block::*;
use wafer_core::clients::storage as store;

/// HTTP-handler block that serves static files from `wafer-run/storage`
/// with caching, clean-URL resolution, and optional SPA fallback.
///
/// Configure via `add_block_config("wafer-run/web", json!({...}))`:
///   - `web_root`: storage folder name (default: "public")
///   - `web_prefix`: URL prefix to strip (default: "")
///   - `web_spa`: serve index.html for missing paths (default: false)
///   - `web_index`: index file name (default: "index.html")
///   - `cache_max_age`: Cache-Control max-age for static assets (default: 3600)
///   - `immutable_max_age`: max-age for hashed assets (default: 31536000)
pub struct WebBlock {
    config: OnceLock<WebConfig>,
}

impl Default for WebBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl WebBlock {
    /// Construct a `WebBlock` with no resolved config; the runtime fills it
    /// in via the `Init` lifecycle event before the first request is served.
    pub fn new() -> Self {
        Self {
            config: OnceLock::new(),
        }
    }

    async fn serve_file(ctx: &dyn Context, msg: &Message, config: &WebConfig) -> OutputStream {
        let mut req_path = msg.path().to_string();

        // Strip prefix
        if !config.prefix.is_empty() {
            if let Some(stripped) = req_path.strip_prefix(&config.prefix) {
                req_path = stripped.to_string();
            }
        }

        // Default to index
        if req_path.is_empty() || req_path == "/" {
            req_path = format!("/{}", config.index_file);
        }

        // Clean path to prevent traversal
        let clean = clean_path(&req_path);

        // Block dotfiles (except .well-known for ACME, OAuth, etc.)
        if clean
            .split('/')
            .any(|seg| seg.starts_with('.') && seg.len() > 1 && seg != ".well-known")
        {
            return OutputStream::error(WaferError {
                code: ErrorCode::NotFound,
                message: "Not found".to_string(),
                meta: vec![],
            });
        }

        // Storage key: strip leading slash
        let key = clean.trim_start_matches('/');

        // Three-attempt fallback chain: bare key -> `<key>.html` -> `<key>/<index>`.
        // The helper retries only on `ErrorCode::NotFound`; any other storage
        // error (PermissionDenied, Internal, …) propagates immediately so the
        // caller sees the real failure instead of a synthetic 404.
        match try_serve_static(ctx, &config.folder, key, &config.index_file).await {
            Ok((data, info)) => {
                // Use content_type from storage metadata, fall back to extension-based detection
                let content_type = if info.content_type.is_empty()
                    || info.content_type == "application/octet-stream"
                {
                    wafer_core::mime::mime_for_ext(Path::new(key)).to_string()
                } else {
                    info.content_type
                };

                let cc = cache_control(key, &content_type, config);
                OutputStream::respond_with_meta(
                    data,
                    vec![
                        MetaEntry {
                            key: META_RESP_CONTENT_TYPE.to_string(),
                            value: content_type,
                        },
                        MetaEntry {
                            key: "resp.header.Cache-Control".to_string(),
                            value: cc,
                        },
                    ],
                )
            }
            Err(e) if e.code == ErrorCode::NotFound && config.spa => {
                serve_index_spa(ctx, config).await
            }
            Err(e) => OutputStream::error(e),
        }
    }
}

#[derive(Clone)]
struct WebConfig {
    folder: String,
    prefix: String,
    spa: bool,
    index_file: String,
    cache_max_age: u32,
    immutable_max_age: u32,
}

impl WebConfig {
    fn from_block_config(config: &BlockConfig) -> Self {
        let str_or = |key: &str, default: &str| -> String {
            let v = config.str(key);
            if v.is_empty() {
                default.to_string()
            } else {
                v.to_string()
            }
        };
        Self {
            folder: str_or("web_root", "public"),
            prefix: config.str("web_prefix").to_string(),
            spa: config.str("web_spa").parse::<bool>().unwrap_or(false),
            index_file: str_or("web_index", "index.html"),
            cache_max_age: config.str("cache_max_age").parse().unwrap_or(3600),
            immutable_max_age: config.str("immutable_max_age").parse().unwrap_or(31536000),
        }
    }
}

fn clean_path(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => continue,
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    format!("/{}", parts.join("/"))
}

fn is_hashed_asset(key: &str) -> bool {
    // Known hashed-asset directories
    let hashed_dirs = ["/assets/", "/_next/static/", "/static/js/", "/static/css/"];
    for dir in &hashed_dirs {
        if key.contains(dir) {
            return true;
        }
    }

    // Check filename for hash pattern: name.hash.ext or name-hash.ext
    let path = Path::new(key);
    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
        for part in stem.split(&['.', '-'][..]) {
            if part.len() >= 6
                && part.len() <= 32
                && part.chars().all(|c| c.is_ascii_alphanumeric())
                && part.chars().any(|c| c.is_ascii_digit())
                && part.chars().any(|c| c.is_ascii_alphabetic())
            {
                return true;
            }
        }
    }

    false
}

fn cache_control(key: &str, content_type: &str, config: &WebConfig) -> String {
    // HTML: always revalidate
    if content_type.starts_with("text/html") {
        return "no-cache".to_string();
    }

    // Hashed assets: immutable
    if is_hashed_asset(key) {
        return format!("public, max-age={}, immutable", config.immutable_max_age);
    }

    // Everything else: standard cache
    format!("public, max-age={}", config.cache_max_age)
}

/// Attempts to fetch a static asset by `key` from `folder`, falling back to
/// `<key>.html` and `<key>/<index_file>` only when the previous attempt returned
/// `ErrorCode::NotFound`. Any other error code propagates immediately.
///
/// This contains the original "extension-less key fallback" logic from `handle()`
/// but threads error-code information through the chain so callers can
/// distinguish "file genuinely missing" from "storage refused / errored".
async fn try_serve_static(
    ctx: &dyn Context,
    folder: &str,
    key: &str,
    index_file: &str,
) -> Result<(Vec<u8>, store::ObjectInfo), WaferError> {
    // First attempt: the bare key.
    match store::get(ctx, folder, key).await {
        Ok(r) => return Ok(r),
        Err(e) if e.code != ErrorCode::NotFound => return Err(e),
        Err(_) => {}
    }

    // NotFound on bare key. Only retry with `.html` / directory index if the
    // key looks like a clean URL (no extension, non-empty).
    if key.is_empty() || Path::new(key).extension().is_some() {
        return Err(WaferError {
            code: ErrorCode::NotFound,
            message: format!("static asset not found: {key}"),
            meta: vec![],
        });
    }

    // Second attempt: `<key>.html`.
    let html_key = format!("{key}.html");
    match store::get(ctx, folder, &html_key).await {
        Ok(r) => return Ok(r),
        Err(e) if e.code != ErrorCode::NotFound => return Err(e),
        Err(_) => {}
    }

    // Third attempt: `<key>/<index_file>`. Whatever this returns is final.
    store::get(ctx, folder, &format!("{key}/{index_file}")).await
}

async fn serve_index_spa(ctx: &dyn Context, config: &WebConfig) -> OutputStream {
    match store::get(ctx, &config.folder, &config.index_file).await {
        Ok((data, _)) => OutputStream::respond_with_meta(
            data,
            vec![
                MetaEntry {
                    key: META_RESP_CONTENT_TYPE.to_string(),
                    value: "text/html; charset=utf-8".to_string(),
                },
                MetaEntry {
                    key: "resp.header.Cache-Control".to_string(),
                    value: "no-cache".to_string(),
                },
            ],
        ),
        Err(_) => OutputStream::error(WaferError {
            code: ErrorCode::NotFound,
            message: "Index file not found".to_string(),
            meta: vec![],
        }),
    }
}

#[wafer_async_trait]
impl Block for WebBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/web",
            "0.0.1",
            "http-handler@v1",
            "Static file server with caching and SPA support",
        )
        .infrastructure()
        .requires(vec!["wafer-run/storage".into()])
        .flow_config(vec![
            ConfigVar::new(
                "web_root",
                "Storage folder under which static files are served.",
                "public",
            )
            .name("Web Root"),
            ConfigVar::new(
                "web_prefix",
                "Optional URL path prefix that must precede every served \
                 file (stripped before the storage lookup).",
                "",
            )
            .name("URL Prefix"),
            ConfigVar::new(
                "web_spa",
                "When true, requests for non-existent files fall back to \
                 the index file instead of 404 (single-page-app mode).",
                "false",
            )
            .name("SPA Mode"),
            ConfigVar::new(
                "web_index",
                "File served at the prefix root and as the SPA fallback.",
                "index.html",
            )
            .name("Index File"),
            ConfigVar::new(
                "cache_max_age",
                "Cache-Control max-age (seconds) for unhashed assets.",
                "3600",
            )
            .name("Cache Max Age"),
            ConfigVar::new(
                "immutable_max_age",
                "Cache-Control max-age (seconds) for content-hashed assets, \
                 which also receive the `immutable` directive.",
                "31536000",
            )
            .name("Immutable Max Age"),
        ])
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        // Only handle GET requests
        let action = msg.action().to_string();
        if !action.is_empty() && action != "retrieve" {
            return OutputStream::error(WaferError {
                code: ErrorCode::Unimplemented,
                message: "Only retrieve action is supported".to_string(),
                meta: vec![],
            });
        }

        let config = self.config.get().cloned().unwrap_or_else(|| WebConfig {
            folder: "public".to_string(),
            prefix: String::new(),
            spa: false,
            index_file: "index.html".to_string(),
            cache_max_age: 3600,
            immutable_max_age: 31536000,
        });

        Self::serve_file(ctx, &msg, &config).await
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if matches!(event.event_type, LifecycleType::Init) {
            let block_config = BlockConfig::from_event(&event);
            let config = WebConfig::from_block_config(&block_config);
            tracing::info!(
                folder = %config.folder,
                spa = config.spa,
                "wafer-run/web configured"
            );
            self.config.set(config).ok();
        }
        Ok(())
    }
}

wafer_block::register_static_block!("wafer-run/web", WebBlock);

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use chrono::Utc;
    use wafer_block::{codec, wire::storage::GetRequest};

    use super::*;

    /// Mock Context that returns scripted storage::get results.
    /// `responses` is consumed front-to-back per call_block invocation.
    /// Each entry is either `Ok(())` (respond with a two-frame ObjectInfo +
    /// body shape) or `Err(WaferError)` (respond with an error terminal).
    #[derive(Clone)]
    struct ScriptedStorageCtx {
        responses: Arc<Mutex<Vec<Result<(), WaferError>>>>,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl ScriptedStorageCtx {
        fn new(responses: Vec<Result<(), WaferError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses)),
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[wafer_async_trait]
    impl Context for ScriptedStorageCtx {
        async fn call_block(
            &self,
            _block_name: &str,
            _msg: Message,
            input: InputStream,
        ) -> OutputStream {
            // Decode the storage GetRequest payload (lives on the InputStream,
            // per `call_service_streaming`) to capture the key asked for.
            let body = input.collect_to_bytes().await;
            if let Ok(req) = codec::decode::<GetRequest>(&body) {
                self.calls.lock().unwrap().push(req.key);
            }
            let next = self.responses.lock().unwrap().remove(0);
            match next {
                Ok(()) => {
                    // Two-frame response: ObjectInfo header chunk + body chunk
                    // + Complete. `store::get` consumes this via
                    // `buffered_header_and_body`.
                    let info = store::ObjectInfo {
                        key: String::new(),
                        size: 13,
                        content_type: "text/plain".to_string(),
                        last_modified: Utc::now(),
                    };
                    let header_bytes = codec::encode(&info).expect("encode ObjectInfo");
                    OutputStream::from_producer(move |sink, _cancel| async move {
                        sink.send_chunk(header_bytes).await.ok();
                        sink.send_chunk(b"file contents".to_vec()).await.ok();
                        let _ = sink.complete(vec![]).await;
                    })
                }
                Err(e) => OutputStream::error(e),
            }
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

    fn err(code: ErrorCode, msg: &str) -> WaferError {
        WaferError {
            code,
            message: msg.to_string(),
            meta: vec![],
        }
    }

    #[tokio::test]
    async fn try_serve_static_propagates_permission_denied_on_bare_key() {
        let ctx =
            ScriptedStorageCtx::new(vec![Err(err(ErrorCode::PermissionDenied, "WRAP denied"))]);
        let result = try_serve_static(&ctx, "public", "page", "index.html").await;
        let e = result.expect_err("must propagate PermissionDenied");
        assert_eq!(e.code, ErrorCode::PermissionDenied);
        assert_eq!(ctx.calls(), vec!["page".to_string()]);
    }

    #[tokio::test]
    async fn try_serve_static_retries_on_not_found_extensionless_key() {
        let ctx = ScriptedStorageCtx::new(vec![
            Err(err(ErrorCode::NotFound, "missing")),
            Err(err(ErrorCode::NotFound, "missing")),
            Ok(()),
        ]);
        let result = try_serve_static(&ctx, "public", "page", "index.html").await;
        assert!(result.is_ok(), "third attempt should succeed");
        assert_eq!(
            ctx.calls(),
            vec![
                "page".to_string(),
                "page.html".to_string(),
                "page/index.html".to_string()
            ],
        );
    }

    #[tokio::test]
    async fn try_serve_static_does_not_retry_for_keys_with_extension() {
        let ctx = ScriptedStorageCtx::new(vec![Err(err(ErrorCode::NotFound, "missing"))]);
        let result = try_serve_static(&ctx, "public", "app.js", "index.html").await;
        let e = result.expect_err("must propagate NotFound without retry");
        assert_eq!(e.code, ErrorCode::NotFound);
        assert_eq!(ctx.calls(), vec!["app.js".to_string()]);
    }

    #[tokio::test]
    async fn try_serve_static_propagates_internal_error_mid_chain() {
        let ctx = ScriptedStorageCtx::new(vec![
            Err(err(ErrorCode::NotFound, "missing")),
            Err(err(ErrorCode::Internal, "disk read fail")),
        ]);
        let result = try_serve_static(&ctx, "public", "page", "index.html").await;
        let e = result.expect_err("Internal must propagate");
        assert_eq!(e.code, ErrorCode::Internal);
        assert_eq!(
            ctx.calls(),
            vec!["page".to_string(), "page.html".to_string()],
        );
    }

    /// Integration-level guard on the outer match in `WebBlock::handle()`:
    /// with SPA mode enabled, a non-`NotFound` storage error (here
    /// `PermissionDenied`) must propagate as the real error and must NOT
    /// trigger the `serve_index_spa` fallthrough (which would mask it as a
    /// `NotFound` 404). Only `ErrorCode::NotFound && config.spa` may fall
    /// through to the SPA index.
    #[tokio::test]
    async fn handle_falls_through_to_spa_only_on_not_found() {
        // Configure the block with SPA mode enabled via a synthetic Init
        // lifecycle event carrying `web_spa: "true"`.
        let block = WebBlock::new();
        // Inline JSON so we don't need a `serde_json` dev-dep for one literal.
        let init_data = br#"{"web_spa":"true"}"#.to_vec();
        block
            .lifecycle(
                &ScriptedStorageCtx::new(vec![]),
                LifecycleEvent {
                    event_type: LifecycleType::Init,
                    data: init_data,
                },
            )
            .await
            .expect("Init lifecycle should succeed");

        // Storage returns PermissionDenied on the very first lookup. The
        // helper must propagate without retry, and the outer match must NOT
        // fall through to `serve_index_spa` despite `config.spa == true`.
        let ctx =
            ScriptedStorageCtx::new(vec![Err(err(ErrorCode::PermissionDenied, "WRAP denied"))]);
        let mut msg = Message::new("retrieve");
        msg.set_meta(crate::meta::META_REQ_RESOURCE, "/private/page");
        msg.set_meta(crate::meta::META_REQ_ACTION, "retrieve");

        let out = block.handle(&ctx, msg, InputStream::empty()).await;
        let terminal = out
            .collect_buffered()
            .await
            .expect_err("PermissionDenied must surface as an error terminal");
        let propagated: WaferError = terminal.into();
        assert_eq!(
            propagated.code,
            ErrorCode::PermissionDenied,
            "outer match must propagate the real storage error, not synthesize a NotFound via SPA fallthrough",
        );
        // Only one storage round-trip — the helper did not retry on
        // non-`NotFound`, and the SPA fallthrough did not fire a second one.
        assert_eq!(ctx.calls(), vec!["private/page".to_string()]);
    }
}
