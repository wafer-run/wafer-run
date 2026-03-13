use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use futures::FutureExt;

use crate::block::Block;
use crate::config::*;
use crate::context::RuntimeContext;

/// Maximum depth of nested `call_block()` invocations to prevent infinite recursion.
const DEFAULT_MAX_CALL_DEPTH: u32 = 16;
use crate::block::FuncBlock;
use crate::executor::{matches_pattern, extract_path_vars};
use crate::helpers::expand_env_vars;
use crate::observability::{ObservabilityBus, ObservabilityContext};
use crate::types::*;

/// A parsed reference to a remote block on GitHub, e.g.
/// `"github.com/acme/auth-block@v1.0.0"`.
#[cfg(feature = "wasm")]
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteBlockRef {
    pub owner: String,
    pub repo: String,
    pub version: String,
    /// Optional block name within a multi-block repo, e.g. `"auth"` in
    /// `github.com/wafer-run/wafer-run-blocks/auth@v1.0.0`.
    pub block_name: Option<String>,
}

/// Parse a block name into a `RemoteBlockRef` if it matches the
/// `github.com/{owner}/{repo}@{version}` convention.
///
/// Returns `None` for local block names (no `@`, doesn't start with
/// `github.com/`, wrong number of segments, or empty version).
#[cfg(feature = "wasm")]
pub fn parse_versioned_block(name: &str) -> Option<RemoteBlockRef> {
    let at_pos = name.find('@')?;
    let path = &name[..at_pos];
    let version = &name[at_pos + 1..];

    if version.is_empty() || version == "latest" {
        return None;
    }

    let segments: Vec<&str> = path.split('/').collect();
    if segments[0] != "github.com" {
        return None;
    }

    match segments.len() {
        3 => Some(RemoteBlockRef {
            owner: segments[1].to_string(),
            repo: segments[2].to_string(),
            version: version.to_string(),
            block_name: None,
        }),
        4 => Some(RemoteBlockRef {
            owner: segments[1].to_string(),
            repo: segments[2].to_string(),
            version: version.to_string(),
            block_name: Some(segments[3].to_string()),
        }),
        _ => None,
    }
}

/// A parsed reference to a remote block on GitHub without a version, e.g.
/// `"github.com/acme/auth-block"`. The runtime resolves the latest release
/// that has a `.wasm` asset.
#[cfg(feature = "wasm")]
#[derive(Debug, Clone, PartialEq)]
pub struct UnversionedRemoteBlockRef {
    pub owner: String,
    pub repo: String,
    /// Optional block name within a multi-block repo.
    pub block_name: Option<String>,
}

/// Parse a block name into an `UnversionedRemoteBlockRef` if it matches the
/// `github.com/{owner}/{repo}` convention (no `@version` suffix).
///
/// Returns `None` when the name contains `@`, doesn't start with
/// `github.com/`, or has the wrong number of segments.
#[cfg(feature = "wasm")]
pub fn parse_unversioned_block(name: &str) -> Option<UnversionedRemoteBlockRef> {
    // Accept bare `github.com/owner/repo[/block]` or with `@latest` suffix
    let name = name.strip_suffix("@latest").unwrap_or(name);

    if name.contains('@') {
        return None;
    }

    let segments: Vec<&str> = name.split('/').collect();
    if segments[0] != "github.com" {
        return None;
    }

    match segments.len() {
        3 => {
            if segments[1].is_empty() || segments[2].is_empty() {
                return None;
            }
            Some(UnversionedRemoteBlockRef {
                owner: segments[1].to_string(),
                repo: segments[2].to_string(),
                block_name: None,
            })
        }
        4 => {
            if segments[1].is_empty() || segments[2].is_empty() || segments[3].is_empty() {
                return None;
            }
            Some(UnversionedRemoteBlockRef {
                owner: segments[1].to_string(),
                repo: segments[2].to_string(),
                block_name: Some(segments[3].to_string()),
            })
        }
        _ => None,
    }
}

/// Thin, clonable handle that blocks can store to call flows from async tasks.
#[derive(Clone)]
pub struct RuntimeHandle {
    inner: Arc<Wafer>,
}

impl RuntimeHandle {
    /// Execute a flow by ID.
    pub async fn execute(&self, flow_id: &str, msg: &mut Message) -> Result_ {
        self.inner.execute(flow_id, msg).await
    }

    /// Execute a single block by name (bypasses flows).
    pub async fn execute_block(&self, block_name: &str, msg: &mut Message) -> Result_ {
        self.inner.execute_block(block_name, msg).await
    }
}

/// Wafer is the WAFER runtime. It manages block registration, flow storage,
/// and execution.
pub struct Wafer {
    pub(crate) blocks: HashMap<String, Arc<dyn Block>>,
    pub(crate) flows: HashMap<String, Flow>,
    /// Block configurations loaded from blocks.json (name → config JSON).
    pub(crate) block_configs: HashMap<String, serde_json::Value>,
    /// All registered blocks + aliases, shared with contexts.
    pub(crate) all_blocks: Arc<HashMap<String, Arc<dyn Block>>>,
    pub hooks: ObservabilityBus,
    /// Snapshot of registered block info (populated at start time).
    pub(crate) blocks_snapshot: Arc<Vec<crate::block::BlockInfo>>,
    /// Snapshot of flow info (populated at start time).
    pub(crate) flow_infos_snapshot: Arc<Vec<crate::config::FlowInfo>>,
    /// Snapshot of flow definitions (populated at start time).
    pub(crate) flow_defs_snapshot: Arc<Vec<crate::config::FlowDef>>,
    /// Alias mappings (e.g. `"@db"` → `"@wafer/sqlite"`). Alias names
    /// can be used wherever a block or flow name is expected.
    pub(crate) aliases: HashMap<String, String>,
    /// Config expanders: registered functions that split a composite config
    /// (e.g. `@wafer/http-server`) into configs for individual blocks.
    pub(crate) config_expanders: HashMap<
        String,
        Box<dyn Fn(serde_json::Value) -> Vec<(String, serde_json::Value)> + Send + Sync>,
    >,
    /// Shared WASM engine for all WASM blocks (fuel-metered).
    #[cfg(feature = "wasm")]
    pub(crate) wasm_engine: Option<Arc<wasmtime::Engine>>,
}

impl Wafer {
    /// Create a new Wafer runtime.
    pub fn new() -> Self {
        Self {
            blocks: HashMap::new(),
            flows: HashMap::new(),
            block_configs: HashMap::new(),
            all_blocks: Arc::new(HashMap::new()),
            aliases: HashMap::new(),
            config_expanders: HashMap::new(),
            hooks: ObservabilityBus::new(),
            blocks_snapshot: Arc::new(Vec::new()),
            flow_infos_snapshot: Arc::new(Vec::new()),
            flow_defs_snapshot: Arc::new(Vec::new()),
            #[cfg(feature = "wasm")]
            wasm_engine: None,
        }
    }

    /// Returns all resolved blocks as an Arc for use in contexts.
    fn all_blocks_arc(&self) -> Arc<HashMap<String, Arc<dyn Block>>> {
        self.all_blocks.clone()
    }

    /// Register an alias mapping. When `call_block(alias)` is called,
    /// it resolves to the target block name.
    pub fn add_alias(&mut self, alias: impl Into<String>, target: impl Into<String>) {
        self.aliases.insert(alias.into(), target.into());
    }

    /// Build a RuntimeContext with shared fields pre-filled.
    fn make_context(
        &self,
        flow_id: impl Into<String>,
        node_id: impl Into<String>,
        config: HashMap<String, String>,
        cancelled: Arc<AtomicBool>,
        deadline: Option<Instant>,
    ) -> RuntimeContext {
        RuntimeContext {
            flow_id: flow_id.into(),
            node_id: node_id.into(),
            config,
            cancelled,
            deadline,
            all_blocks: self.all_blocks_arc(),
            call_depth: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            max_call_depth: DEFAULT_MAX_CALL_DEPTH,
            registered_blocks_snapshot: self.blocks_snapshot.clone(),
            flow_infos_snapshot: self.flow_infos_snapshot.clone(),
            flow_defs_snapshot: self.flow_defs_snapshot.clone(),
            aliases: Arc::new(self.aliases.clone()),
            caller_requires: None, // unrestricted by default; overridden per-block in execute_node
        }
    }

    /// Rebuild the all_blocks map from registered blocks + aliases.
    /// Call this after resolve() completes.
    pub fn rebuild_all_blocks(&mut self) {
        let mut map = HashMap::new();
        for (name, block) in &self.blocks {
            map.insert(name.clone(), block.clone());
        }
        // Insert alias entries — alias names point to the same Arc<dyn Block>
        for (alias, target) in &self.aliases {
            if let Some(block) = self.blocks.get(target) {
                map.insert(alias.clone(), block.clone());
            }
        }
        self.all_blocks = Arc::new(map);
    }

    /// Load block configurations from a JSON file.
    ///
    /// The file should be a JSON object mapping block names to config objects.
    /// Environment variables in `${VAR}` format are expanded before parsing.
    ///
    /// Example:
    /// ```json
    /// {
    ///     "@wafer/database": { "type": "sqlite", "path": "data/app.db" },
    ///     "@wafer/crypto": { "jwt_secret": "${JWT_SECRET}" },
    ///     "@wafer/logger": {}
    /// }
    /// ```
    pub fn load_blocks_json(&mut self, path: &str) -> Result<(), String> {
        let data = std::fs::read_to_string(path)
            .map_err(|e| format!("read blocks.json {}: {}", path, e))?;

        let expanded = expand_env_vars(&data);

        let mut map: HashMap<String, serde_json::Value> = serde_json::from_str(&expanded)
            .map_err(|e| format!("parse blocks.json: {}", e))?;

        // Extract alias definitions before processing block configs
        if let Some(aliases_val) = map.remove("aliases") {
            if let Some(aliases_obj) = aliases_val.as_object() {
                for (alias, target) in aliases_obj {
                    if let Some(target_str) = target.as_str() {
                        self.aliases.insert(alias.clone(), target_str.to_string());
                    }
                }
            }
        }

        for (name, config) in map {
            self.block_configs.insert(name, config);
        }

        Ok(())
    }

    /// Add a block configuration programmatically.
    pub fn add_block_config(&mut self, name: impl Into<String>, config: serde_json::Value) {
        self.block_configs.insert(name.into(), config);
    }

    /// Register a config expander that splits a composite config into
    /// individual block configs. Called during `resolve()` before configs
    /// are distributed to blocks.
    pub fn add_config_expander(
        &mut self,
        name: impl Into<String>,
        expander: impl Fn(serde_json::Value) -> Vec<(String, serde_json::Value)> + Send + Sync + 'static,
    ) {
        self.config_expanders.insert(name.into(), Box::new(expander));
    }

    /// HasBlock returns true if a block with the given type name is registered.
    pub fn has_block(&self, type_name: &str) -> bool {
        self.blocks.contains_key(type_name)
    }

    /// RegisterBlock registers a block instance under the given type name.
    /// The instance is also pre-resolved so it is available via `call_block()`
    /// even when it is not referenced as a flow node.
    ///
    /// The block's `lifecycle(Init)` will be called during `start()` with
    /// config data from `add_block_config()` (if any) or empty data.
    pub fn register_block(&mut self, type_name: impl Into<String>, block: Arc<dyn Block>) {
        let name = type_name.into();
        self.blocks.insert(name, block);
    }

    /// RegisterBlockFunc registers a synchronous inline handler function as a block.
    /// The block is also pre-resolved so it is available via `call_block()`.
    ///
    /// For handlers that need to perform async work, use `register_block_func_async`.
    pub fn register_block_func(
        &mut self,
        type_name: impl Into<String>,
        handler: impl Fn(&dyn crate::context::Context, &mut Message) -> Result_ + Send + Sync + 'static,
    ) {
        use crate::block::BlockInfo;
        let name = type_name.into();
        let block: Arc<dyn Block> = Arc::new(FuncBlock {
            info: BlockInfo {
                name: name.clone(),
                version: "0.0.0".to_string(),
                interface: "inline".to_string(),
                summary: "Inline function block".to_string(),
                instance_mode: InstanceMode::PerNode,
                allowed_modes: Vec::new(),
                admin_ui: None,
                runtime: BlockRuntime::default(),
                requires: Vec::new(),
            },
            handler: Box::new(handler),
        });
        self.register_block(name, block);
    }

    /// RegisterBlockFuncAsync registers an async inline handler function as a block.
    /// The block is also pre-resolved so it is available via `call_block()`.
    ///
    /// The handler receives a context and mutable message reference, and returns
    /// a future that resolves to a `Result_`.
    pub fn register_block_func_async<F, Fut>(
        &mut self,
        type_name: impl Into<String>,
        handler: F,
    ) where
        F: for<'a> Fn(&'a dyn crate::context::Context, &'a mut Message) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result_> + Send + 'static,
    {
        use crate::block::{AsyncFuncBlock, BlockInfo};
        let name = type_name.into();
        let block: Arc<dyn Block> = Arc::new(AsyncFuncBlock {
            info: BlockInfo {
                name: name.clone(),
                version: "0.0.0".to_string(),
                interface: "inline-async".to_string(),
                summary: "Inline async function block".to_string(),
                instance_mode: InstanceMode::PerNode,
                allowed_modes: Vec::new(),
                admin_ui: None,
                runtime: BlockRuntime::default(),
                requires: Vec::new(),
            },
            handler: Box::new(move |ctx, msg| Box::pin(handler(ctx, msg))),
        });
        self.register_block(name, block);
    }

    /// AddFlow adds a programmatically-built flow to the runtime.
    pub fn add_flow(&mut self, flow: Flow) {
        self.flows.insert(flow.id.clone(), flow);
    }

    /// AddFlowDef adds a flow from a JSON definition.
    pub fn add_flow_def(&mut self, def: &FlowDef) {
        let flow = flow_def_to_flow(def);
        self.add_flow(flow);
    }

    /// Gather `"uses"` contributions from all block configs and deep-merge them
    /// into the target infrastructure block configs.
    ///
    /// For each block config that has a `"uses"` key (a JSON object mapping
    /// target block names to contribution objects), the contribution is
    /// deep-merged into the target block's config. Deep-merge rules:
    /// - For JSON objects, keys are combined recursively.
    /// - For non-object values, the target block's own value wins (dependents
    ///   can contribute new keys but not override the infra block's own config).
    /// - If the target block has no config entry yet, one is created.
    /// Run config expanders: remove composite configs from `block_configs`,
    /// invoke their expander, and deep-merge the results into the target blocks.
    fn expand_composite_configs(&mut self) {
        let keys: Vec<String> = self
            .block_configs
            .keys()
            .filter(|k| self.config_expanders.contains_key(k.as_str()))
            .cloned()
            .collect();

        for key in keys {
            if let Some(config) = self.block_configs.remove(&key) {
                if let Some(expander) = self.config_expanders.get(&key) {
                    for (name, val) in expander(config) {
                        let entry = self
                            .block_configs
                            .entry(name)
                            .or_insert_with(|| serde_json::Value::Object(Default::default()));
                        deep_merge(entry, &val);
                    }
                }
            }
        }
    }

    fn gather_uses_configs(&mut self) {
        // Collect all (target, contribution) pairs first to avoid borrow conflicts.
        let mut contributions: Vec<(String, serde_json::Value)> = Vec::new();

        for config in self.block_configs.values() {
            if let Some(uses) = config.get("uses").and_then(|v| v.as_object()) {
                for (target, contrib) in uses {
                    contributions.push((target.clone(), contrib.clone()));
                }
            }
        }

        for (target, contrib) in contributions {
            // Resolve alias: if the target is an alias, merge into the real block
            let resolved_target = self.aliases.get(&target).cloned().unwrap_or(target);
            let entry = self
                .block_configs
                .entry(resolved_target)
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
            deep_merge(entry, &contrib);
        }

        // Strip `uses` keys from all configs so downstream code doesn't see them.
        for config in self.block_configs.values_mut() {
            if let Some(obj) = config.as_object_mut() {
                obj.remove("uses");
            }
        }
    }

    /// Resolve walks all flow trees and resolves block references.
    ///
    /// Before resolving flows, initializes all registered blocks via
    /// `lifecycle(Init)`. Blocks with configs (from `load_blocks_json` or
    /// `add_block_config`) are initialized first (infrastructure), then
    /// remaining blocks are initialized (features that may depend on infra).
    pub async fn resolve(&mut self) -> Result<(), String> {
        // Expand composite configs (e.g. @wafer/http-server → http-listener + router)
        self.expand_composite_configs();
        // Gather uses contributions before initializing blocks
        self.gather_uses_configs();

        let configs: Vec<(String, serde_json::Value)> =
            self.block_configs.drain().collect();

        // Collect names of all pre-registered blocks for phase 2 ordering.
        let pre_registered: Vec<String> = self.blocks.keys().cloned().collect();

        // Track which blocks were initialized with config data.
        let config_names: std::collections::HashSet<String> =
            configs.iter().map(|(n, _)| n.clone()).collect();

        // Sort configs: @wafer/* infrastructure blocks first, then everything else.
        // Infrastructure blocks (database, config, crypto, etc.) must be initialized
        // before feature blocks that depend on them during lifecycle init.
        let mut infra_configs = Vec::new();
        let mut feature_configs = Vec::new();
        for entry in &configs {
            if entry.0.starts_with("@wafer/") {
                infra_configs.push(entry);
            } else {
                feature_configs.push(entry);
            }
        }

        // Phase 1a: Initialize infrastructure blocks (@wafer/*) with configs.
        self.rebuild_all_blocks();
        for (name, config) in &infra_configs {
            if let Some(block) = self.blocks.get(name.as_str()) {
                let ctx = self.make_context(
                    String::new(),
                    String::new(),
                    HashMap::new(),
                    Arc::new(AtomicBool::new(false)),
                    None,
                );

                block
                    .lifecycle(
                        &ctx,
                        LifecycleEvent {
                            event_type: LifecycleType::Init,
                            data: serde_json::to_vec(config).unwrap_or_default(),
                        },
                    )
                    .await
                    .map_err(|e| format!("init block {:?}: {}", name, e))?;
            } else {
                tracing::warn!(block = %name, "block config present but no block registered — skipping");
            }
        }

        // Phase 1b: Initialize feature blocks with configs.
        // Infrastructure is now ready, so these can use @wafer/database etc.
        self.rebuild_all_blocks();
        for (name, config) in &feature_configs {
            if let Some(block) = self.blocks.get(name.as_str()) {
                let ctx = self.make_context(
                    String::new(),
                    String::new(),
                    HashMap::new(),
                    Arc::new(AtomicBool::new(false)),
                    None,
                );

                block
                    .lifecycle(
                        &ctx,
                        LifecycleEvent {
                            event_type: LifecycleType::Init,
                            data: serde_json::to_vec(config).unwrap_or_default(),
                        },
                    )
                    .await
                    .map_err(|e| format!("init block {:?}: {}", name, e))?;
            } else {
                tracing::warn!(block = %name, "block config present but no block registered — skipping");
            }
        }

        // Rebuild the all_blocks snapshot so lifecycle contexts can find
        // all blocks during phase 2.
        self.rebuild_all_blocks();

        // Phase 2: Initialize remaining pre-registered blocks (no config).
        // These can safely call into infrastructure blocks initialized above.
        for name in &pre_registered {
            if config_names.contains(name) {
                continue; // Already initialized in phase 1
            }
            if let Some(block) = self.blocks.get(name) {
                let ctx = self.make_context(
                    String::new(),
                    String::new(),
                    HashMap::new(),
                    Arc::new(AtomicBool::new(false)),
                    None,
                );
                block
                    .lifecycle(
                        &ctx,
                        LifecycleEvent {
                            event_type: LifecycleType::Init,
                            data: Vec::new(),
                        },
                    )
                    .await
                    .map_err(|e| format!("init block {:?}: {}", name, e))?;
            }
        }

        // Phase 3: Resolve flow nodes.
        let flow_ids: Vec<String> = self.flows.keys().cloned().collect();
        for flow_id in flow_ids {
            let mut flow = self.flows.remove(&flow_id).expect("BUG: flow disappeared during iteration");
            self.resolve_node(&mut flow.root).await?;
            self.flows.insert(flow_id.clone(), flow);
        }
        Ok(())
    }

    fn resolve_node<'a>(
        &'a mut self,
        node: &'a mut Node,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            // Parse config map
            if let Some(ref config) = node.config {
                node.config_map = parse_config_map(config);
            }

            if !node.block.is_empty() {
                // Resolve alias to actual block name
                if let Some(target) = self.aliases.get(&node.block) {
                    node.block = target.clone();
                }

                if let Some(block) = self.blocks.get(&node.block) {
                    node.resolved_block = Some(block.clone());
                } else {
                    // Block not in resolved — try remote WASM download
                    #[cfg(feature = "wasm")]
                    {
                        let block = if let Some(remote_ref) = parse_versioned_block(&node.block) {
                            self.download_remote_block(&remote_ref).await?
                        } else if let Some(remote_ref) = parse_unversioned_block(&node.block) {
                            self.resolve_latest_wasm_release(&remote_ref).await?
                        } else {
                            return Err(format!("block type not found: {}", node.block));
                        };

                        let ctx = self.make_context(
                            String::new(),
                            String::new(),
                            node.config_map.clone(),
                            Arc::new(AtomicBool::new(false)),
                            None,
                        );

                        block
                            .lifecycle(
                                &ctx,
                                LifecycleEvent {
                                    event_type: LifecycleType::Init,
                                    data: node
                                        .config
                                        .as_ref()
                                        .map(|c| serde_json::to_vec(c).unwrap_or_default())
                                        .unwrap_or_default(),
                                },
                            )
                            .await
                            .map_err(|e| format!("init remote block {:?}: {}", node.block, e))?;

                        self.blocks.insert(node.block.clone(), block.clone());
                        node.resolved_block = Some(block);
                    }

                    #[cfg(not(feature = "wasm"))]
                    return Err(format!("block type not found: {}", node.block));
                }
            }

            for child in &mut node.next {
                self.resolve_node(child).await?;
            }
            Ok(())
        })
    }

    /// Download a remote block from GitHub Releases and load it as a sandboxed
    /// WASM block. The `.wasm` asset is expected at:
    /// - Single-block repo: `…/{version}/{repo}.wasm`
    /// - Multi-block repo:  `…/{version}/{block_name}.wasm`
    #[cfg(feature = "wasm")]
    async fn download_remote_block(&mut self, r: &RemoteBlockRef) -> Result<Arc<dyn Block>, String> {
        use crate::wasm::WASMBlock;
        use crate::wasm::capabilities::BlockCapabilities;

        let asset_name = r.block_name.as_deref().unwrap_or(&r.repo);
        let url = format!(
            "https://github.com/{}/{}/releases/download/{}/{}.wasm",
            r.owner, r.repo, r.version, asset_name
        );

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("failed to create HTTP client: {}", e))?;
        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("failed to download {}: {}", url, e))?;

        let status = resp.status().as_u16();
        if status != 200 {
            return Err(format!("failed to download {}: HTTP {}", url, status));
        }

        let body = resp
            .bytes()
            .await
            .map_err(|e| format!("failed to read body from {}: {}", url, e))?;

        if body.is_empty() {
            return Err(format!("failed to download {}: empty response body", url));
        }

        let engine = self.wasm_engine()?.clone();
        let block = WASMBlock::load_with_engine(&engine, &body, BlockCapabilities::none())
            .map_err(|e| format!("failed to load remote block {}: {}", url, e))?;

        Ok(Arc::new(block))
    }

    /// Resolve the latest GitHub Release that has a `.wasm` asset and load it
    /// as a sandboxed WASM block.
    #[cfg(feature = "wasm")]
    async fn resolve_latest_wasm_release(
        &mut self,
        r: &UnversionedRemoteBlockRef,
    ) -> Result<Arc<dyn Block>, String> {
        use crate::wasm::WASMBlock;
        use crate::wasm::capabilities::BlockCapabilities;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("failed to create HTTP client: {}", e))?;

        // 1. Fetch recent releases from the GitHub API
        let api_url = format!(
            "https://api.github.com/repos/{}/{}/releases?per_page=10",
            r.owner, r.repo
        );

        let api_resp = client
            .get(&api_url)
            .header("User-Agent", "wafer-run/0.1.0")
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| format!("failed to fetch releases for {}/{}: {}", r.owner, r.repo, e))?;

        let api_status = api_resp.status().as_u16();
        if api_status != 200 {
            return Err(format!(
                "failed to fetch releases for {}/{}: HTTP {}",
                r.owner, r.repo, api_status
            ));
        }

        let api_body = api_resp
            .bytes()
            .await
            .map_err(|e| format!("failed to read releases response: {}", e))?;

        // 2. Parse the JSON response
        #[derive(serde::Deserialize)]
        struct GhAsset {
            name: String,
            browser_download_url: String,
        }

        #[derive(serde::Deserialize)]
        struct GhRelease {
            assets: Vec<GhAsset>,
        }

        let releases: Vec<GhRelease> = serde_json::from_slice(&api_body)
            .map_err(|e| format!("failed to parse releases JSON for {}/{}: {}", r.owner, r.repo, e))?;

        // 3. Find first release with a .wasm asset
        //    For multi-block repos, match exactly "{block_name}.wasm".
        //    For single-block repos, match any ".wasm" file.
        let mut wasm_url: Option<String> = None;
        for release in &releases {
            for asset in &release.assets {
                let matches = match &r.block_name {
                    Some(bn) => asset.name == format!("{}.wasm", bn),
                    None => asset.name.ends_with(".wasm"),
                };
                if matches {
                    wasm_url = Some(asset.browser_download_url.clone());
                    break;
                }
            }
            if wasm_url.is_some() {
                break;
            }
        }

        let wasm_url = wasm_url.ok_or_else(|| {
            match &r.block_name {
                Some(bn) => format!(
                    "no release with a {}.wasm asset found for {}/{}",
                    bn, r.owner, r.repo
                ),
                None => format!(
                    "no release with a .wasm asset found for {}/{}",
                    r.owner, r.repo
                ),
            }
        })?;

        // 4. Download the .wasm asset
        let dl_resp = client
            .get(&wasm_url)
            .send()
            .await
            .map_err(|e| format!("failed to download {}: {}", wasm_url, e))?;

        let dl_status = dl_resp.status().as_u16();
        if dl_status != 200 {
            return Err(format!(
                "failed to download {}: HTTP {}",
                wasm_url, dl_status
            ));
        }

        let dl_body = dl_resp
            .bytes()
            .await
            .map_err(|e| format!("failed to read body from {}: {}", wasm_url, e))?;

        if dl_body.is_empty() {
            return Err(format!(
                "failed to download {}: empty response body",
                wasm_url
            ));
        }

        // 5. Load via WASM engine
        let engine = self.wasm_engine()?.clone();
        let block =
            WASMBlock::load_with_engine(&engine, &dl_body, BlockCapabilities::none())
                .map_err(|e| {
                    format!(
                        "failed to load remote block {}/{}: {}",
                        r.owner, r.repo, e
                    )
                })?;

        Ok(Arc::new(block))
    }

    /// Get or create the shared WASM engine with fuel metering.
    #[cfg(feature = "wasm")]
    pub fn wasm_engine(&mut self) -> Result<&wasmtime::Engine, String> {
        if self.wasm_engine.is_none() {
            let mut config = wasmtime::Config::default();
            config.consume_fuel(true);
            config.wasm_component_model(true);
            config.async_support(true);
            let engine = wasmtime::Engine::new(&config)
                .map_err(|e| format!("failed to create wasmtime engine: {}", e))?;
            self.wasm_engine = Some(Arc::new(engine));
        }
        Ok(self.wasm_engine.as_ref().unwrap())
    }

    /// Initialize the runtime without calling `bind()` on blocks.
    ///
    /// Use this when you don't need the HTTP listener (e.g. wafer-run-node,
    /// wafer-ffi, or integration tests that manage their own serving).
    pub async fn start_without_bind(&mut self) -> Result<(), String> {
        self.resolve().await?;

        // Rebuild the all_blocks map so contexts can see all resolved blocks
        self.rebuild_all_blocks();

        // Snapshot introspection data for contexts
        self.blocks_snapshot = Arc::new(self.blocks.values().map(|b| b.info()).collect());
        self.flow_infos_snapshot = Arc::new(self.flows_info());
        self.flow_defs_snapshot = Arc::new(self.flow_defs());

        Ok(())
    }

    /// Start the runtime, wrap in `Arc`, and call `bind()` on all blocks.
    ///
    /// This is the primary entry point for applications that want blocks
    /// (like `@wafer/http`) to spawn their own async tasks.
    pub async fn start(mut self) -> Result<Arc<Self>, String> {
        // 1. All mutable work
        self.start_without_bind().await?;

        // 2. Call lifecycle(Start) on all blocks
        let ctx = self.make_context(
            "startup",
            "startup",
            HashMap::new(),
            Arc::new(AtomicBool::new(false)),
            None,
        );
        for block in self.blocks.values() {
            let _ = block.lifecycle(
                &ctx,
                LifecycleEvent {
                    event_type: LifecycleType::Start,
                    data: Vec::new(),
                },
            ).await;
        }

        // 3. Wrap in Arc
        let arc_self = Arc::new(self);

        // 4. Call bind(RuntimeHandle) on all blocks
        let handle = RuntimeHandle {
            inner: arc_self.clone(),
        };
        for (_, block) in &arc_self.blocks {
            block.bind(handle.clone());
        }

        // 5. Return Arc
        Ok(arc_self)
    }

    /// Shut down all resolved block instances (works through `Arc`).
    pub async fn shutdown(&self) {
        let ctx = self.make_context(
            "shutdown",
            "shutdown",
            HashMap::new(),
            Arc::new(AtomicBool::new(false)),
            None,
        );
        for block in self.blocks.values() {
            let _ = block.lifecycle(
                &ctx,
                LifecycleEvent {
                    event_type: LifecycleType::Stop,
                    data: Vec::new(),
                },
            ).await;
        }
    }

    /// Stop shuts down all resolved block instances (requires `&mut self`).
    ///
    /// Prefer `shutdown()` when the runtime is behind an `Arc`.
    pub async fn stop(&mut self) {
        let ctx = self.make_context(
            "shutdown",
            "shutdown",
            HashMap::new(),
            Arc::new(AtomicBool::new(false)),
            None,
        );
        for block in self.blocks.values() {
            let _ = block.lifecycle(
                &ctx,
                LifecycleEvent {
                    event_type: LifecycleType::Stop,
                    data: Vec::new(),
                },
            ).await;
        }
    }

    /// Execute runs a flow by ID with the given message.
    pub async fn execute(&self, flow_id: &str, msg: &mut Message) -> Result_ {
        let flow = match self.flows.get(flow_id) {
            Some(c) => c,
            None => {
                return Result_ {
                    action: Action::Error,
                    error: Some(WaferError::new(
                        "flow_not_found",
                        format!("flow not found: {}", flow_id),
                    )),
                    response: None,
                    message: None,
                };
            }
        };

        // Observability: flow start
        self.hooks.fire_flow_start(flow_id, msg);
        let start = Instant::now();

        // Set up flow-level timeout via deadline
        let cancelled = Arc::new(AtomicBool::new(false));
        let timeout = flow.config.timeout;
        let deadline = if !timeout.is_zero() {
            Some(Instant::now() + timeout)
        } else {
            None
        };

        let mut visited_flows = HashSet::new();
        visited_flows.insert(flow_id.to_string());

        let result = self.execute_node(&flow.root, msg, flow_id, &flow.config.on_error, &cancelled, deadline, &mut visited_flows, "root").await;

        // Check timeout
        let result = if deadline.is_some() && cancelled.load(Ordering::Relaxed) && result.action != Action::Error {
            Result_ {
                action: Action::Error,
                error: Some(WaferError::new(
                    "deadline_exceeded",
                    format!("flow {:?} timed out after {:?}", flow_id, timeout),
                )),
                response: None,
                message: result.message,
            }
        } else {
            result
        };

        // Observability: flow end
        self.hooks.fire_flow_end(flow_id, &result, start.elapsed());

        result
    }

    /// Execute a single block by name, bypassing flows.
    pub async fn execute_block(&self, block_name: &str, msg: &mut Message) -> Result_ {
        // Resolve alias
        let resolved = self.aliases.get(block_name)
            .map(|s| s.as_str())
            .unwrap_or(block_name);

        let block = match self.all_blocks.get(resolved).or_else(|| self.all_blocks.get(block_name)) {
            Some(b) => b.clone(),
            None => {
                return Result_ {
                    action: Action::Error,
                    error: Some(WaferError::new(
                        "block_not_found",
                        format!("block not found: {}", block_name),
                    )),
                    response: None,
                    message: None,
                };
            }
        };

        let cancelled = Arc::new(AtomicBool::new(false));
        let caller_requires = {
            let info = block.info();
            if info.requires.is_empty() { None } else { Some(info.requires) }
        };
        let mut ctx = self.make_context(block_name, "root", HashMap::new(), cancelled, None);
        ctx.caller_requires = caller_requires;

        // Observability
        let obs_ctx = ObservabilityContext {
            flow_id: String::new(),
            node_path: "root".to_string(),
            block_name: block_name.to_string(),
            trace_id: msg.get_meta("trace_id").to_string(),
            message: Some(msg.clone()),
        };
        self.hooks.fire_block_start(&obs_ctx);
        let start = Instant::now();

        let result = std::panic::AssertUnwindSafe(block.handle(&ctx, msg))
            .catch_unwind()
            .await;

        let result = match result {
            Ok(r) => r,
            Err(panic_info) => {
                let panic_msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_info.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic".to_string()
                };
                Result_ {
                    action: Action::Error,
                    error: Some(WaferError::new("panic", format!("block panicked: {}", panic_msg))),
                    response: None,
                    message: Some(msg.clone()),
                }
            }
        };

        self.hooks.fire_block_end(&obs_ctx, &result, start.elapsed());

        result
    }

    fn execute_node<'a>(
        &'a self,
        node: &'a Node,
        msg: &'a mut Message,
        flow_id: &'a str,
        on_error: &'a str,
        cancelled: &'a Arc<AtomicBool>,
        deadline: Option<Instant>,
        visited_flows: &'a mut HashSet<String>,
        node_path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result_> + Send + 'a>> {
        Box::pin(async move {
            // Handle flow references
            if !node.flow.is_empty() {
                return self.execute_flow_ref(node, msg, on_error, cancelled, deadline, visited_flows).await;
            }

            let block = match &node.resolved_block {
                Some(b) => b.clone(),
                None => {
                    return Result_ {
                        action: Action::Error,
                        error: Some(WaferError::new(
                            "unresolved",
                            format!("block not resolved: {}", node.block),
                        )),
                        response: None,
                        message: None,
                    };
                }
            };

            // Build context for this node, with requires enforcement
            let caller_requires = {
                let info = block.info();
                if info.requires.is_empty() {
                    None // unrestricted
                } else {
                    Some(info.requires)
                }
            };
            let mut ctx = self.make_context(
                flow_id,
                node_path,
                node.config_map.clone(),
                cancelled.clone(),
                deadline,
            );
            ctx.caller_requires = caller_requires;

            // Observability: block start
            let obs_ctx = ObservabilityContext {
                flow_id: flow_id.to_string(),
                node_path: node_path.to_string(),
                block_name: node.block.clone(),
                trace_id: msg.get_meta("trace_id").to_string(),
                message: Some(msg.clone()),
            };
            self.hooks.fire_block_start(&obs_ctx);
            let start = Instant::now();

            // Execute block with panic recovery
            let result = std::panic::AssertUnwindSafe(block.handle(&ctx, msg))
                .catch_unwind()
                .await;

            let result = match result {
                Ok(r) => r,
                Err(panic_info) => {
                    let panic_msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                        s.to_string()
                    } else if let Some(s) = panic_info.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "unknown panic".to_string()
                    };
                    Result_ {
                        action: Action::Error,
                        error: Some(WaferError::new("panic", format!("block panicked: {}", panic_msg))),
                        response: None,
                        message: Some(msg.clone()),
                    }
                }
            };

            // Observability: block end
            self.hooks.fire_block_end(&obs_ctx, &result, start.elapsed());

            // Process result
            match result.action {
                Action::Respond | Action::Drop => return result,
                Action::Error => {
                    if on_error == "stop" {
                        return result;
                    }
                    // on_error=continue: fall through to children
                }
                Action::Continue => {}
            }

            // Update message from result if available
            if let Some(ref result_msg) = result.message {
                *msg = result_msg.clone();
            }

            if node.next.is_empty() {
                if result.action == Action::Error {
                    // on_error=continue with no more nodes: swallow error
                    return Result_::continue_with(msg.clone());
                }
                return result;
            }

            self.execute_first_match(&node.next, msg, flow_id, on_error, cancelled, deadline, visited_flows, node_path).await
        })
    }

    fn execute_flow_ref<'a>(
        &'a self,
        node: &'a Node,
        msg: &'a mut Message,
        on_error: &'a str,
        cancelled: &'a Arc<AtomicBool>,
        deadline: Option<Instant>,
        visited_flows: &'a mut HashSet<String>,
    ) -> Pin<Box<dyn Future<Output = Result_> + Send + 'a>> {
        Box::pin(async move {
            // Resolve flow alias
            let flow_name = self.aliases.get(&node.flow)
                .map(|s| s.as_str())
                .unwrap_or(&node.flow);

            // Circular flow reference detection
            if visited_flows.contains(flow_name) {
                return Result_ {
                    action: Action::Error,
                    error: Some(WaferError::new(
                        "circular_flow",
                        format!("circular flow reference detected: {}", flow_name),
                    )),
                    response: None,
                    message: None,
                };
            }

            let target = match self.flows.get(flow_name) {
                Some(c) => c,
                None => {
                    return Result_ {
                        action: Action::Error,
                        error: Some(WaferError::new(
                            "not_found",
                            format!("referenced flow not found: {}", flow_name),
                        )),
                        response: None,
                        message: None,
                    };
                }
            };

            let flow_name_owned = flow_name.to_string();
            visited_flows.insert(flow_name_owned.clone());
            let result = self.execute_node(&target.root, msg, &target.id, &target.config.on_error, cancelled, deadline, visited_flows, "root").await;
            visited_flows.remove(&flow_name_owned);

            if result.action == Action::Continue && !node.next.is_empty() {
                return self.execute_first_match(
                    &node.next,
                    msg,
                    &target.id,
                    on_error,
                    cancelled,
                    deadline,
                    visited_flows,
                    &format!("ref:{}", node.flow),
                ).await;
            }

            result
        })
    }

    fn execute_first_match<'a>(
        &'a self,
        nodes: &'a [Box<Node>],
        msg: &'a mut Message,
        flow_id: &'a str,
        on_error: &'a str,
        cancelled: &'a Arc<AtomicBool>,
        deadline: Option<Instant>,
        visited_flows: &'a mut HashSet<String>,
        parent_path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result_> + Send + 'a>> {
        Box::pin(async move {
            for (i, child) in nodes.iter().enumerate() {
                if !matches_pattern(&child.match_pattern, &msg.kind) {
                    continue;
                }
                // Extract path variables from HTTP route patterns
                if !child.match_pattern.is_empty() {
                    if let Some(idx) = child.match_pattern.find(":/") {
                        let pattern_path = &child.match_pattern[idx + 1..];
                        if let Some(msg_idx) = msg.kind.find(":/") {
                            let msg_path = msg.kind[msg_idx + 1..].to_string();
                            extract_path_vars(pattern_path, &msg_path, msg);
                        }
                    }
                }
                let child_path = format!("{}.{}", parent_path, i);
                return self.execute_node(child, msg, flow_id, on_error, cancelled, deadline, visited_flows, &child_path).await;
            }
            Result_::continue_with(msg.clone())
        })
    }

    /// GetFlow returns a flow by ID.
    pub fn get_flow(&self, id: &str) -> Option<&Flow> {
        self.flows.get(id)
    }

    /// Flows returns info about all loaded flows.
    pub fn flows_info(&self) -> Vec<FlowInfo> {
        self.flows
            .values()
            .map(|c| FlowInfo {
                id: c.id.clone(),
                summary: c.summary.clone(),
                on_error: c.config.on_error.clone(),
                timeout: if c.config.timeout.is_zero() {
                    String::new()
                } else {
                    format!("{}s", c.config.timeout.as_secs())
                },
            })
            .collect()
    }

    /// FlowDefs serializes all runtime flows back to FlowDef format.
    pub fn flow_defs(&self) -> Vec<FlowDef> {
        self.flows.values().map(flow_to_flow_def).collect()
    }
}

impl Default for Wafer {
    fn default() -> Self {
        Self::new()
    }
}

/// Deep-merge `src` into `dst`. For objects, keys are combined recursively.
/// For non-object values, `dst`'s existing value wins (contributors cannot
/// override the target block's own scalar values).
fn deep_merge(dst: &mut serde_json::Value, src: &serde_json::Value) {
    match (dst, src) {
        (serde_json::Value::Object(dst_map), serde_json::Value::Object(src_map)) => {
            for (key, src_val) in src_map {
                if let Some(dst_val) = dst_map.get_mut(key) {
                    deep_merge(dst_val, src_val);
                } else {
                    dst_map.insert(key.clone(), src_val.clone());
                }
            }
        }
        // Non-object: dst wins, do nothing
        _ => {}
    }
}
