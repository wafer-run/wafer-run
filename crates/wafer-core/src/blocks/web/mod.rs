use std::path::{Path, PathBuf};
use std::sync::Arc;
use wafer_run::*;

/// WebBlock serves static files with intelligent caching and SPA support.
/// Configure via node config: {"web_root": "./dist", "web_prefix": "/site", "web_spa": true}
pub struct WebBlock {
    default_root: String,
    default_prefix: String,
    default_spa: bool,
    default_index: String,
    cache_max_age: u32,
    immutable_max_age: u32,
}

impl WebBlock {
    pub fn new() -> Self {
        Self {
            default_root: "./public".to_string(),
            default_prefix: String::new(),
            default_spa: false,
            default_index: "index.html".to_string(),
            cache_max_age: 3600,
            immutable_max_age: 31536000,
        }
    }

    fn get_config<'a>(&'a self, ctx: &'a dyn Context) -> WebConfig {
        WebConfig {
            root: ctx
                .config_get("web_root")
                .unwrap_or(&self.default_root)
                .to_string(),
            prefix: ctx
                .config_get("web_prefix")
                .unwrap_or(&self.default_prefix)
                .to_string(),
            spa: ctx
                .config_get("web_spa")
                .and_then(|s| s.parse::<bool>().ok())
                .unwrap_or(self.default_spa),
            index_file: ctx
                .config_get("web_index")
                .unwrap_or(&self.default_index)
                .to_string(),
            cache_max_age: ctx
                .config_get("cache_max_age")
                .and_then(|s| s.parse().ok())
                .unwrap_or(self.cache_max_age),
            immutable_max_age: ctx
                .config_get("immutable_max_age")
                .and_then(|s| s.parse().ok())
                .unwrap_or(self.immutable_max_age),
        }
    }

    fn serve_file(msg: &mut Message, config: &WebConfig) -> Result_ {
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

        // Block dotfiles
        if clean.split('/').any(|seg| seg.starts_with('.') && seg.len() > 1) {
            return err_not_found(msg.clone(), "Not found");
        }

        // Resolve absolute path
        let abs_root = match std::fs::canonicalize(&config.root) {
            Ok(p) => p,
            Err(_) => return err_not_found(msg.clone(), "Web root not found"),
        };

        let file_path = abs_root.join(clean.trim_start_matches('/'));

        // Resolve symlinks and verify still within root
        let resolved = match std::fs::canonicalize(&file_path) {
            Ok(p) => p,
            Err(_) => {
                // If SPA mode, serve index.html for non-existent paths
                if config.spa {
                    let index_path = abs_root.join(&config.index_file);
                    return serve_index_spa(msg, &index_path);
                }
                return err_not_found(msg.clone(), "File not found");
            }
        };

        if !resolved.starts_with(&abs_root) {
            return err_not_found(msg.clone(), "Not found");
        }

        // Handle directories
        if resolved.is_dir() {
            let index = resolved.join(&config.index_file);
            if index.exists() {
                return serve_static_file(msg, &index, config);
            }
            return err_not_found(msg.clone(), "Not found");
        }

        serve_static_file(msg, &resolved, config)
    }
}

struct WebConfig {
    root: String,
    prefix: String,
    spa: bool,
    index_file: String,
    cache_max_age: u32,
    immutable_max_age: u32,
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

fn mime_for_ext(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8".to_string(),
        "css" => "text/css; charset=utf-8".to_string(),
        "js" | "mjs" => "application/javascript; charset=utf-8".to_string(),
        "json" => "application/json; charset=utf-8".to_string(),
        "xml" => "application/xml; charset=utf-8".to_string(),
        "svg" => "image/svg+xml".to_string(),
        "png" => "image/png".to_string(),
        "jpg" | "jpeg" => "image/jpeg".to_string(),
        "gif" => "image/gif".to_string(),
        "webp" => "image/webp".to_string(),
        "avif" => "image/avif".to_string(),
        "ico" => "image/x-icon".to_string(),
        "woff" => "font/woff".to_string(),
        "woff2" => "font/woff2".to_string(),
        "ttf" => "font/ttf".to_string(),
        "otf" => "font/otf".to_string(),
        "eot" => "application/vnd.ms-fontobject".to_string(),
        "pdf" => "application/pdf".to_string(),
        "zip" => "application/zip".to_string(),
        "wasm" => "application/wasm".to_string(),
        "map" => "application/json".to_string(),
        "txt" => "text/plain; charset=utf-8".to_string(),
        "md" => "text/markdown; charset=utf-8".to_string(),
        "csv" => "text/csv; charset=utf-8".to_string(),
        "mp4" => "video/mp4".to_string(),
        "webm" => "video/webm".to_string(),
        "mp3" => "audio/mpeg".to_string(),
        "ogg" => "audio/ogg".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

fn is_hashed_asset(path: &Path) -> bool {
    let path_str = path.to_string_lossy();

    // Known hashed-asset directories
    let hashed_dirs = ["/assets/", "/_next/static/", "/static/js/", "/static/css/"];
    for dir in &hashed_dirs {
        if path_str.contains(dir) {
            return true;
        }
    }

    // Check filename for hash pattern: name.hash.ext or name-hash.ext
    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
        // Check if there's a segment that looks like a hash (6-32 hex/alphanum chars)
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

fn cache_control(path: &Path, content_type: &str, config: &WebConfig) -> String {
    // HTML: always revalidate
    if content_type.starts_with("text/html") {
        return "no-cache".to_string();
    }

    // Hashed assets: immutable
    if is_hashed_asset(path) {
        return format!(
            "public, max-age={}, immutable",
            config.immutable_max_age
        );
    }

    // Everything else: standard cache
    format!("public, max-age={}", config.cache_max_age)
}

fn serve_static_file(msg: &mut Message, path: &PathBuf, config: &WebConfig) -> Result_ {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => return err_not_found(msg.clone(), "File not found"),
    };

    let content_type = mime_for_ext(path);
    let cc = cache_control(path, &content_type, config);

    let mut m = msg.clone();
    m.set_meta("resp.header.Cache-Control", &cc);

    respond(m, 200, data, &content_type)
}

fn serve_index_spa(msg: &mut Message, index_path: &PathBuf) -> Result_ {
    let data = match std::fs::read(index_path) {
        Ok(d) => d,
        Err(_) => return err_not_found(msg.clone(), "Index file not found"),
    };

    let mut m = msg.clone();
    m.set_meta("resp.header.Cache-Control", "no-cache");

    respond(m, 200, data, "text/html; charset=utf-8")
}

impl Block for WebBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "@wafer/web".to_string(),
            version: "0.1.0".to_string(),
            interface: "handler@v1".to_string(),
            summary: "Static file server with caching and SPA support".to_string(),
            instance_mode: InstanceMode::Singleton,
            allowed_modes: vec![InstanceMode::PerNode],
            admin_ui: None,
        }
    }

    fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
        // Only handle GET requests
        let action = msg.action();
        if !action.is_empty() && action != "retrieve" {
            return error(msg.clone(), 405, "method_not_allowed", "Only GET is supported");
        }

        let config = self.get_config(ctx);
        Self::serve_file(msg, &config)
    }

    fn lifecycle(
        &self,
        ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if matches!(event.event_type, LifecycleType::Start) {
            // Validate web root exists on startup
            let root = ctx
                .config_get("web_root")
                .unwrap_or(&self.default_root);

            if !Path::new(root).exists() {
                tracing::warn!("Web root '{}' does not exist", root);
            }
        }
        Ok(())
    }
}

pub fn register(w: &mut Wafer) {
    w.register_block("@wafer/web", Arc::new(WebBlock::new()));
}
