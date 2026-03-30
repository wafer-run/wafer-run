//! Package Registry: in-memory index of wafer-run/registry manifests.
//!
//! On startup, fetches all manifest.json files from the wafer-run/registry
//! GitHub repo and holds them in memory for fast search and browse.
//!
//! GET  /registry                              — browse (HTML UI)
//! GET  /registry/search?q=term&type=block|flow — search packages
//! GET  /registry/packages/{org}/{block}        — package details + versions

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use wafer_run::*;

fn github_token() -> Option<String> {
    std::env::var("GITHUB_TOKEN").ok().filter(|t| !t.is_empty())
}

fn http_client() -> &'static reqwest::Client {
    use std::sync::OnceLock;
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("wafer-registry/0.1")
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

#[derive(serde::Serialize, Clone)]
struct PackageEntry {
    name: String,
    description: String,
    latest: String,
    repo_url: String,
    package_type: String,
    runtime_type: String,
    versions: Vec<VersionInfo>,
}

#[derive(serde::Serialize, Clone)]
struct VersionInfo {
    tag_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    wasm_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    flow_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    crate_name: Option<String>,
    abi: u32,
}

#[derive(serde::Deserialize)]
struct RegistryManifest {
    name: String,
    #[serde(default)]
    summary: String,
    latest: String,
    versions: HashMap<String, ManifestVersion>,
}

#[derive(serde::Deserialize)]
struct ManifestVersion {
    abi: u32,
    #[serde(default)]
    wasm_url: Option<String>,
    #[serde(default)]
    flow_url: Option<String>,
    #[serde(default, rename = "crate")]
    crate_name: Option<String>,
}

// ---------------------------------------------------------------------------
// Block
// ---------------------------------------------------------------------------

pub struct RegistryBlock {
    packages: RwLock<Vec<PackageEntry>>,
}

impl RegistryBlock {
    pub fn new() -> Self {
        Self { packages: RwLock::new(Vec::new()) }
    }

    fn handle_browse(msg: &mut Message) -> Result_ {
        let html = include_str!("../content/registry.html");
        msg.set_meta(
            "resp.header.Content-Security-Policy",
            "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; img-src 'self' data: blob:; font-src 'self' https://fonts.gstatic.com; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'",
        );
        respond(msg, html.as_bytes().to_vec(), "text/html")
    }

    fn handle_search(&self, msg: &mut Message) -> Result_ {
        let query = msg.query("q").to_string().to_lowercase();
        let type_filter = msg.query("type").to_string();

        let packages = self.packages.read().unwrap();
        let filtered: Vec<&PackageEntry> = packages.iter().filter(|pkg| {
            if !query.is_empty()
                && !pkg.name.to_lowercase().contains(&query)
                && !pkg.description.to_lowercase().contains(&query)
            {
                return false;
            }
            if !type_filter.is_empty() {
                let types: Vec<&str> = pkg.package_type.split(',').collect();
                return types.contains(&type_filter.as_str());
            }
            true
        }).collect();

        json_respond(msg, &serde_json::json!({
            "packages": filtered,
            "total": filtered.len(),
        }))
    }

    fn handle_get_package(&self, msg: &mut Message, name: &str) -> Result_ {
        let packages = self.packages.read().unwrap();
        match packages.iter().find(|p| p.name == name) {
            Some(pkg) => json_respond(msg, &serde_json::json!({
                "package": pkg,
                "versions": pkg.versions,
            })),
            None => err_not_found(msg, &format!("Package '{}' not found", name)),
        }
    }

    // -----------------------------------------------------------------------
    // Registry loading
    // -----------------------------------------------------------------------

    async fn load_registry(&self) {
        let tree = match Self::fetch_registry_tree().await {
            Some(t) => t,
            None => return,
        };

        let mut entries = Vec::new();
        for (org, block) in &tree {
            if let Some(manifest) = Self::fetch_manifest(org, block).await {
                entries.push(Self::manifest_to_entry(manifest));
            }
        }

        *self.packages.write().unwrap() = entries;
    }

    /// List all {org}/{block} pairs from the registry repo tree.
    async fn fetch_registry_tree() -> Option<Vec<(String, String)>> {
        let url = "https://api.github.com/repos/wafer-run/registry/git/trees/main?recursive=1";
        let mut req = http_client().get(url);
        if let Some(token) = github_token() {
            req = req.bearer_auth(token);
        }
        let body: serde_json::Value = req.send().await.ok()?.json().await.ok()?;
        let entries = body.get("tree").and_then(|v| v.as_array())?;

        let mut pairs = Vec::new();
        for entry in entries {
            let path = entry.get("path").and_then(|v| v.as_str()).unwrap_or("");
            if path.ends_with("/manifest.json") {
                let parts: Vec<&str> = path.split('/').collect();
                if parts.len() == 3 {
                    pairs.push((parts[0].to_string(), parts[1].to_string()));
                }
            }
        }
        Some(pairs)
    }

    async fn fetch_manifest(org: &str, block: &str) -> Option<RegistryManifest> {
        let url = format!(
            "https://raw.githubusercontent.com/wafer-run/registry/main/{}/{}/manifest.json",
            org, block
        );
        let mut req = http_client().get(&url);
        if let Some(token) = github_token() {
            req = req.bearer_auth(token);
        }
        req.send().await.ok()?.json().await.ok()
    }

    fn manifest_to_entry(manifest: RegistryManifest) -> PackageEntry {
        let repo_url = manifest.versions.values()
            .find_map(|v| v.wasm_url.as_ref().or(v.flow_url.as_ref()))
            .and_then(|url| {
                let rest = url.strip_prefix("https://github.com/")?;
                let parts: Vec<&str> = rest.splitn(3, '/').collect();
                (parts.len() >= 2).then(|| format!("https://github.com/{}/{}", parts[0], parts[1]))
            })
            .unwrap_or_default();

        let has_wasm = manifest.versions.values().any(|v| v.wasm_url.is_some());
        let has_flow = manifest.versions.values().any(|v| v.flow_url.is_some());
        let has_crate = manifest.versions.values().any(|v| v.crate_name.is_some());

        let mut pkg_types = Vec::new();
        if has_wasm || has_crate { pkg_types.push("block"); }
        if has_flow { pkg_types.push("flow"); }
        let package_type = if pkg_types.is_empty() { "block".to_string() } else { pkg_types.join(",") };

        let runtime_type = match (has_crate, has_wasm) {
            (true, true) => "both",
            (true, false) => "native",
            (false, true) => "wasm",
            (false, false) => "native",
        }.to_string();

        let mut versions: Vec<VersionInfo> = manifest.versions.iter().map(|(ver, entry)| {
            VersionInfo {
                tag_name: format!("v{}", ver),
                wasm_url: entry.wasm_url.clone(),
                flow_url: entry.flow_url.clone(),
                crate_name: entry.crate_name.clone(),
                abi: entry.abi,
            }
        }).collect();
        versions.sort_by(|a, b| b.tag_name.cmp(&a.tag_name));

        PackageEntry {
            name: manifest.name,
            description: manifest.summary,
            latest: manifest.latest,
            repo_url,
            package_type,
            runtime_type,
            versions,
        }
    }
}

#[async_trait::async_trait]
impl Block for RegistryBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "wafer-site/registry".to_string(),
            version: "0.6.0".to_string(),
            interface: "http-handler@v1".to_string(),
            summary: "Package registry backed by wafer-run/registry GitHub repo".to_string(),
            instance_mode: InstanceMode::Singleton,
            allowed_modes: Vec::new(),
            admin_ui: None,
            runtime: wafer_run::types::BlockRuntime::Native,
            requires: Vec::new(),
            collections: Vec::new(),
            config_schema: None,
        }
    }

    async fn handle(&self, _ctx: &dyn Context, msg: &mut Message) -> Result_ {
        let path = msg.path().to_string();
        let action = msg.action().to_string();
        match (action.as_str(), path.as_str()) {
            ("retrieve", "/registry") | ("retrieve", "/registry/") => Self::handle_browse(msg),
            ("retrieve", "/registry/search") => self.handle_search(msg),
            ("retrieve", p) if p.starts_with("/registry/packages/") => {
                let name = &p["/registry/packages/".len()..];
                self.handle_get_package(msg, name)
            }
            _ => err_not_found(msg, &format!("Registry endpoint not found: {}", path)),
        }
    }

    async fn lifecycle(&self, _ctx: &dyn Context, event: LifecycleEvent) -> std::result::Result<(), WaferError> {
        if let LifecycleType::Init = event.event_type {
            self.load_registry().await;
        }
        Ok(())
    }
}

pub fn register(w: &mut Wafer) {
    w.register_block("wafer-site/registry", Arc::new(RegistryBlock::new()));
}
