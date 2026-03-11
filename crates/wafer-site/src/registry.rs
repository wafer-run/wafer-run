//! Package Registry: Go-module-style, GitHub-backed versions for blocks, flows, and interfaces.
//!
//! GitHub is the source of truth. There is no registration step — packages
//! are auto-indexed the first time someone looks them up, just like Go modules.
//! Packages can be blocks (.wasm), flows (.flow.json), interfaces (.interface.json),
//! or any combination (stored as a comma-separated `package_type` string).
//!
//! GET  /registry                                                            — browse (HTML UI)
//! GET  /registry/search?q=term&type=block|flow|interface                   — search indexed packages
//! GET  /registry/packages/{owner}/{repo}                                    — package details + versions (auto-indexes)
//! GET  /registry/packages/{owner}/{repo}/versions                           — version list from GitHub (auto-indexes)
//! GET  /registry/packages/{owner}/{repo}/download/{version}?type=block|flow|interface — redirect to GitHub asset

use std::collections::HashMap;
use std::sync::Arc;
use wafer_core::clients::database as db;
use wafer_core::clients::database::{Filter, FilterOp, ListOptions, Record, SortField};
use wafer_core::clients::network;
use wafer_run::*;

/// Cache TTL in seconds (default 5 minutes).
fn cache_ttl_secs() -> u64 {
    std::env::var("GITHUB_CACHE_TTL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300)
}

/// GitHub personal access token for higher rate limits.
fn github_token() -> Option<String> {
    std::env::var("GITHUB_TOKEN").ok().filter(|t| !t.is_empty())
}

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    has_wasm_asset: bool,
    #[serde(default)]
    wasm_download_url: Option<String>,
    #[serde(default)]
    has_native_asset: bool,
    #[serde(default)]
    native_download_url: Option<String>,
    #[serde(default)]
    has_chain_asset: bool,
    #[serde(default)]
    chain_download_url: Option<String>,
    #[serde(default)]
    has_interface_asset: bool,
    #[serde(default)]
    interface_download_url: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
}

// ---------------------------------------------------------------------------
// Block
// ---------------------------------------------------------------------------

pub struct RegistryBlock;

impl RegistryBlock {
    pub fn new() -> Self {
        Self
    }

    fn handle_browse(msg: &mut Message) -> Result_ {
        let html = include_str!("../content/registry.html");
        msg.set_meta(
            "resp.header.Content-Security-Policy",
            "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; img-src 'self' data: blob:; font-src 'self' https://fonts.gstatic.com; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'",
        );
        respond(msg, html.as_bytes().to_vec(), "text/html")
    }

    async fn handle_search(msg: &mut Message, ctx: &dyn Context) -> Result_ {
        let query = msg.query("q").to_string();
        let type_filter = msg.query("type").to_string();
        let (page, page_size, _) = msg.pagination_params(20);

        let mut filters = Vec::new();
        if !query.is_empty() {
            let escaped = query.replace('%', "\\%").replace('_', "\\_");
            filters.push(Filter {
                field: "name".to_string(),
                operator: FilterOp::Like,
                value: serde_json::Value::String(format!("%{}%", escaped)),
            });
        }

        let opts = ListOptions {
            filters,
            sort: vec![SortField { field: "download_count".to_string(), desc: true }],
            limit: page_size as i64,
            offset: ((page - 1) * page_size) as i64,
        };

        match db::list(ctx, "packages", &opts).await {
            Ok(result) => {
                let blocked = Self::get_blocked_patterns(ctx).await;
                let filtered: Vec<_> = result.records.into_iter().filter(|pkg| {
                    let name = pkg.data.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    if Self::matches_any_pattern(name, &blocked) { return false; }
                    if !type_filter.is_empty() {
                        let pkg_type = pkg.data.get("package_type").and_then(|v| v.as_str()).unwrap_or("block");
                        let types: Vec<&str> = if pkg_type == "both" { vec!["block", "flow"] } else { pkg_type.split(',').collect() };
                        types.contains(&type_filter.as_str())
                    } else { true }
                }).collect();
                let filtered_count = filtered.len() as i64;
                json_respond(msg, &serde_json::json!({
                    "packages": filtered, "total": filtered_count,
                    "page": page, "page_size": page_size, "query": query
                }))
            }
            Err(_) => json_respond(msg, &serde_json::json!({"packages": []})),
        }
    }

    async fn ensure_package(ctx: &dyn Context, name: &str) -> std::result::Result<Record, String> {
        if Self::is_blocked(ctx, name).await {
            return Err(format!("Package '{}' is blocked", name));
        }
        if let Ok(record) = db::get_by_field(ctx, "packages", "name", serde_json::Value::String(name.to_string())).await {
            if record.data.get("package_type").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                let versions = Self::fetch_versions_cached(ctx, name).await;
                let pkg_type = Self::detect_package_type(&versions);
                let runtime_type = Self::detect_runtime_type(&versions);
                let mut update = HashMap::new();
                update.insert("package_type".to_string(), serde_json::Value::String(pkg_type));
                update.insert("runtime_type".to_string(), serde_json::Value::String(runtime_type));
                let _ = db::update(ctx, "packages", &record.id, update).await;
            }
            return Ok(record);
        }
        if !Self::validate_package_name(name) {
            return Err(format!("Invalid package name: {}", name));
        }
        let description = Self::fetch_repo_description(ctx, name).await
            .ok_or_else(|| format!("Repository '{}' not found on GitHub", name))?;
        let repo_url = format!("https://{}", name);
        let mut record = HashMap::new();
        record.insert("name".to_string(), serde_json::Value::String(name.to_string()));
        record.insert("description".to_string(), serde_json::Value::String(description));
        record.insert("repo_url".to_string(), serde_json::Value::String(repo_url));
        record.insert("owner_id".to_string(), serde_json::Value::String(String::new()));
        record.insert("download_count".to_string(), serde_json::Value::Number(0.into()));
        let versions = Self::fetch_versions_cached(ctx, name).await;
        let pkg_type = Self::detect_package_type(&versions);
        let runtime_type = Self::detect_runtime_type(&versions);
        record.insert("package_type".to_string(), serde_json::Value::String(pkg_type));
        record.insert("runtime_type".to_string(), serde_json::Value::String(runtime_type));
        db::create(ctx, "packages", record).await.map_err(|e| format!("Failed to index package: {}", e))
    }

    async fn fetch_repo_description(ctx: &dyn Context, name: &str) -> Option<String> {
        let (owner, repo) = Self::parse_owner_repo(name)?;
        let url = format!("https://api.github.com/repos/{}/{}", owner, repo);
        let mut headers = HashMap::new();
        headers.insert("User-Agent".to_string(), "wafer-registry/0.1".to_string());
        headers.insert("Accept".to_string(), "application/vnd.github+json".to_string());
        if let Some(token) = github_token() {
            headers.insert("Authorization".to_string(), format!("Bearer {}", token));
        }
        let response = network::do_request(ctx, "GET", &url, &headers, None).await.ok()?;
        if response.status_code != 200 { return None; }
        let data: serde_json::Value = serde_json::from_slice(&response.body).ok()?;
        Some(data.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string())
    }

    async fn handle_get_package(msg: &mut Message, ctx: &dyn Context, name: &str) -> Result_ {
        let pkg = match Self::ensure_package(ctx, name).await {
            Ok(r) => r,
            Err(e) => return err_not_found(msg, &e),
        };
        let versions = Self::fetch_versions_cached(ctx, name).await;
        json_respond(msg, &serde_json::json!({"package": pkg, "versions": versions}))
    }

    async fn handle_get_versions(msg: &mut Message, ctx: &dyn Context, name: &str) -> Result_ {
        if let Err(e) = Self::ensure_package(ctx, name).await {
            return err_not_found(msg, &e);
        }
        let versions = Self::fetch_versions_cached(ctx, name).await;
        json_respond(msg, &serde_json::json!({"package": name, "versions": versions}))
    }

    async fn handle_download(msg: &mut Message, ctx: &dyn Context, name: &str, version: &str) -> Result_ {
        if let Err(e) = Self::ensure_package(ctx, name).await {
            return err_not_found(msg, &e);
        }
        let (owner, repo) = match Self::parse_owner_repo(name) {
            Some(pair) => pair,
            None => return err_bad_request(msg, "Invalid package name"),
        };
        let asset_type = msg.query("type").to_string();
        let versions = Self::fetch_versions_cached(ctx, name).await;
        let asset_url = match asset_type.as_str() {
            "flow" => versions.iter().find(|r| r.tag_name == version).and_then(|r| r.chain_download_url.clone())
                .unwrap_or_else(|| format!("https://github.com/{}/{}/releases/download/{}/{}.flow.json", owner, repo, version, repo)),
            "interface" => versions.iter().find(|r| r.tag_name == version).and_then(|r| r.interface_download_url.clone())
                .unwrap_or_else(|| format!("https://github.com/{}/{}/releases/download/{}/{}.interface.json", owner, repo, version, repo)),
            _ => versions.iter().find(|r| r.tag_name == version).and_then(|r| r.wasm_download_url.clone())
                .unwrap_or_else(|| format!("https://github.com/{}/{}/releases/download/{}/{}.wasm", owner, repo, version, repo)),
        };
        let _ = db::exec_raw(ctx, "UPDATE packages SET download_count = download_count + 1 WHERE name = ?", &[serde_json::Value::String(name.to_string())]).await;
        ResponseBuilder::new(msg).status(302).set_header("Location", &asset_url).body(vec![], "text/plain")
    }

    async fn fetch_versions_cached(ctx: &dyn Context, name: &str) -> Vec<GitHubRelease> {
        let (owner, repo) = match Self::parse_owner_repo(name) {
            Some(pair) => pair,
            None => return Vec::new(),
        };
        let ttl = cache_ttl_secs();
        let cached = db::get_by_field(ctx, "github_tag_cache", "package_name", serde_json::Value::String(name.to_string())).await.ok();
        if let Some(ref cache_record) = cached {
            let fetched_at = cache_record.data.get("fetched_at").and_then(|v| v.as_str()).unwrap_or("");
            if Self::is_cache_fresh(fetched_at, ttl) {
                if let Some(tags_json) = cache_record.data.get("tags_json").and_then(|v| v.as_str()) {
                    if let Ok(releases) = serde_json::from_str::<Vec<GitHubRelease>>(tags_json) {
                        return releases;
                    }
                }
            }
        }
        let etag = cached.as_ref().and_then(|r| r.data.get("etag")).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let url = format!("https://api.github.com/repos/{}/{}/releases?per_page=100", owner, repo);
        let mut headers = HashMap::new();
        headers.insert("User-Agent".to_string(), "wafer-registry/0.1".to_string());
        headers.insert("Accept".to_string(), "application/vnd.github+json".to_string());
        if let Some(token) = github_token() {
            headers.insert("Authorization".to_string(), format!("Bearer {}", token));
        }
        if !etag.is_empty() {
            headers.insert("If-None-Match".to_string(), etag.clone());
        }
        match network::do_request(ctx, "GET", &url, &headers, None).await {
            Ok(response) => {
                if response.status_code == 304 {
                    if let Some(ref cache_record) = cached {
                        let mut update = HashMap::new();
                        update.insert("fetched_at".to_string(), serde_json::Value::String(Self::now_timestamp()));
                        let _ = db::update(ctx, "github_tag_cache", &cache_record.id, update).await;
                        if let Some(tags_json) = cache_record.data.get("tags_json").and_then(|v| v.as_str()) {
                            if let Ok(releases) = serde_json::from_str::<Vec<GitHubRelease>>(tags_json) {
                                return releases;
                            }
                        }
                    }
                    return Vec::new();
                }
                if response.status_code != 200 { return Self::return_stale_cache(&cached); }
                let releases = Self::parse_github_releases(&response.body, &repo);
                let new_etag = response.headers.get("etag").and_then(|v| v.first()).cloned().unwrap_or_default();
                let tags_json = serde_json::to_string(&releases).unwrap_or_default();
                let mut cache_data = HashMap::new();
                cache_data.insert("package_name".to_string(), serde_json::Value::String(name.to_string()));
                cache_data.insert("tags_json".to_string(), serde_json::Value::String(tags_json));
                cache_data.insert("fetched_at".to_string(), serde_json::Value::String(Self::now_timestamp()));
                cache_data.insert("etag".to_string(), serde_json::Value::String(new_etag));
                let _ = db::upsert(ctx, "github_tag_cache", "package_name", serde_json::Value::String(name.to_string()), cache_data).await;
                releases
            }
            Err(_) => Self::return_stale_cache(&cached),
        }
    }

    fn parse_github_releases(body: &[u8], _repo: &str) -> Vec<GitHubRelease> {
        let raw: Vec<serde_json::Value> = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };
        raw.iter().map(|release| {
            let tag_name = release.get("tag_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let published_at = release.get("published_at").and_then(|v| v.as_str()).map(|s| s.to_string());
            let assets = release.get("assets").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let wasm_asset = assets.iter().find(|a| a.get("name").and_then(|v| v.as_str()).map(|n| n.ends_with(".wasm")).unwrap_or(false));
            let has_wasm_asset = wasm_asset.is_some();
            let wasm_download_url = wasm_asset.and_then(|a| a.get("browser_download_url")).and_then(|v| v.as_str()).map(|s| s.to_string());
            // Native assets: .so, .dylib, .dll, or .tar.gz (Rust crate archives)
            let native_asset = assets.iter().find(|a| {
                a.get("name").and_then(|v| v.as_str()).map(|n| {
                    n.ends_with(".so") || n.ends_with(".dylib") || n.ends_with(".dll")
                        || n.ends_with(".tar.gz") || n.ends_with(".crate")
                }).unwrap_or(false)
            });
            let has_native_asset = native_asset.is_some();
            let native_download_url = native_asset.and_then(|a| a.get("browser_download_url")).and_then(|v| v.as_str()).map(|s| s.to_string());
            let chain_asset = assets.iter().find(|a| a.get("name").and_then(|v| v.as_str()).map(|n| n.ends_with(".flow.json")).unwrap_or(false));
            let has_chain_asset = chain_asset.is_some();
            let chain_download_url = chain_asset.and_then(|a| a.get("browser_download_url")).and_then(|v| v.as_str()).map(|s| s.to_string());
            let interface_asset = assets.iter().find(|a| a.get("name").and_then(|v| v.as_str()).map(|n| n.ends_with(".interface.json")).unwrap_or(false));
            let has_interface_asset = interface_asset.is_some();
            let interface_download_url = interface_asset.and_then(|a| a.get("browser_download_url")).and_then(|v| v.as_str()).map(|s| s.to_string());
            GitHubRelease { tag_name, has_wasm_asset, wasm_download_url, has_native_asset, native_download_url, has_chain_asset, chain_download_url, has_interface_asset, interface_download_url, published_at }
        }).collect()
    }

    fn detect_package_type(releases: &[GitHubRelease]) -> String {
        let has_wasm = releases.iter().any(|r| r.has_wasm_asset);
        let has_chain = releases.iter().any(|r| r.has_chain_asset);
        let has_interface = releases.iter().any(|r| r.has_interface_asset);
        let mut types = Vec::new();
        if has_wasm { types.push("block"); }
        if has_chain { types.push("flow"); }
        if has_interface { types.push("interface"); }
        if types.is_empty() { "block".to_string() } else { types.join(",") }
    }

    /// Detect the runtime type (native, wasm, or both) from release assets.
    fn detect_runtime_type(releases: &[GitHubRelease]) -> String {
        let has_wasm = releases.iter().any(|r| r.has_wasm_asset);
        let has_native = releases.iter().any(|r| r.has_native_asset);
        match (has_native, has_wasm) {
            (true, true) => "both".to_string(),
            (true, false) => "native".to_string(),
            (false, true) => "wasm".to_string(),
            (false, false) => "wasm".to_string(), // default assumption
        }
    }

    fn return_stale_cache(cached: &Option<Record>) -> Vec<GitHubRelease> {
        if let Some(ref cache_record) = cached {
            if let Some(tags_json) = cache_record.data.get("tags_json").and_then(|v| v.as_str()) {
                if let Ok(releases) = serde_json::from_str::<Vec<GitHubRelease>>(tags_json) {
                    return releases;
                }
            }
        }
        Vec::new()
    }

    async fn get_blocked_patterns(ctx: &dyn Context) -> Vec<String> {
        let opts = ListOptions { filters: Vec::new(), sort: Vec::new(), limit: 0, offset: 0 };
        match db::list(ctx, "blocked_packages", &opts).await {
            Ok(result) => result.records.iter().filter_map(|r| r.data.get("name_pattern").and_then(|v| v.as_str()).map(|s| s.to_string())).collect(),
            Err(_) => Vec::new(),
        }
    }

    async fn is_blocked(ctx: &dyn Context, name: &str) -> bool {
        let patterns = Self::get_blocked_patterns(ctx).await;
        Self::matches_any_pattern(name, &patterns)
    }

    fn matches_any_pattern(name: &str, patterns: &[String]) -> bool {
        for pattern in patterns {
            if pattern == name { return true; }
            if let Some(prefix) = pattern.strip_suffix("/*") {
                if name.starts_with(prefix) && name.len() > prefix.len() && name.as_bytes()[prefix.len()] == b'/' {
                    return true;
                }
            }
        }
        false
    }

    fn validate_package_name(name: &str) -> bool {
        let parts: Vec<&str> = name.split('/').collect();
        if parts.len() != 3 || parts[0] != "github.com" { return false; }
        let valid_segment = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.');
        valid_segment(parts[1]) && valid_segment(parts[2])
    }

    fn parse_owner_repo(name: &str) -> Option<(String, String)> {
        let parts: Vec<&str> = name.split('/').collect();
        if parts.len() != 3 || parts[0] != "github.com" { return None; }
        Some((parts[1].to_string(), parts[2].to_string()))
    }

    fn is_cache_fresh(fetched_at: &str, ttl_secs: u64) -> bool {
        if fetched_at.is_empty() { return false; }
        let fetched: u64 = match fetched_at.parse() { Ok(v) => v, Err(_) => return false };
        let now = Self::unix_now();
        now.saturating_sub(fetched) < ttl_secs
    }

    fn unix_now() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
    }

    fn now_timestamp() -> String { Self::unix_now().to_string() }
}

#[async_trait::async_trait]
impl Block for RegistryBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "@wafer-site/registry".to_string(),
            version: "0.4.0".to_string(),
            interface: "handler@v1".to_string(),
            summary: "Package registry: Go-module-style, GitHub-backed blocks, flows, and interfaces".to_string(),
            instance_mode: InstanceMode::Singleton,
            allowed_modes: Vec::new(),
            admin_ui: None,
            runtime: wafer_run::types::BlockRuntime::Native,
            requires: Vec::new(),
        }
    }

    async fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
        let path = msg.path().to_string();
        let action = msg.action().to_string();
        match (action.as_str(), path.as_str()) {
            ("retrieve", "/registry") | ("retrieve", "/registry/") => Self::handle_browse(msg),
            ("retrieve", "/registry/search") => Self::handle_search(msg, ctx).await,
            ("retrieve", p) if p.starts_with("/registry/packages/") => {
                let rest = &p["/registry/packages/".len()..];
                if let Some(pos) = rest.find("/download/") {
                    let name = &rest[..pos];
                    let version = &rest[pos + "/download/".len()..];
                    Self::handle_download(msg, ctx, name, version).await
                } else if rest.ends_with("/versions") {
                    let name = &rest[..rest.len() - "/versions".len()];
                    Self::handle_get_versions(msg, ctx, name).await
                } else {
                    Self::handle_get_package(msg, ctx, rest).await
                }
            }
            _ => err_not_found(msg, &format!("Registry endpoint not found: {}", path)),
        }
    }

    async fn lifecycle(&self, ctx: &dyn Context, event: LifecycleEvent) -> std::result::Result<(), WaferError> {
        if let LifecycleType::Init = event.event_type {
            let _ = db::exec_raw(ctx, "UPDATE packages SET package_type = 'block' WHERE package_type IS NULL", &[]).await;
        }
        Ok(())
    }
}

pub fn register(w: &mut Wafer) {
    w.register_block("@wafer-site/registry", Arc::new(RegistryBlock::new()));
}
