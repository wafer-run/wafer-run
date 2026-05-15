mod mime;

use std::{path::Path, sync::OnceLock};

use wafer_block::*;
use wafer_core::clients::storage as store;

/// WebBlock serves static files from wafer-run/storage with caching and SPA support.
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

        // Try the exact key first, then with .html suffix for clean URLs,
        // then directory index (path/index.html) for sub-app entry points.
        let result = match store::get(ctx, &config.folder, key).await {
            Ok(r) => Ok(r),
            Err(_) if !key.is_empty() && Path::new(key).extension().is_none() => {
                let html_key = format!("{key}.html");
                match store::get(ctx, &config.folder, &html_key).await {
                    Ok(r) => Ok(r),
                    Err(_) => {
                        let index_key = format!("{}/{}", key, config.index_file);
                        store::get(ctx, &config.folder, &index_key).await
                    }
                }
            }
            Err(e) => Err(e),
        };

        match result {
            Ok((data, info)) => {
                // Use content_type from storage metadata, fall back to extension-based detection
                let content_type = if info.content_type.is_empty()
                    || info.content_type == "application/octet-stream"
                {
                    mime::mime_for_ext(Path::new(key)).to_string()
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
            Err(_) => {
                // File not found — if SPA mode, serve index
                if config.spa {
                    return serve_index_spa(ctx, config).await;
                }
                OutputStream::error(WaferError {
                    code: ErrorCode::NotFound,
                    message: "File not found".to_string(),
                    meta: vec![],
                })
            }
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

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for WebBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/web",
            "0.0.1",
            "http-handler@v1",
            "Static file server with caching and SPA support",
        )
        .instance_mode(InstanceMode::Singleton)
        .requires(vec!["wafer-run/storage".into()])
        .category(BlockCategory::Infrastructure)
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

wafer_run::register_static_block!("wafer-run/web", WebBlock);
