use std::collections::{HashMap, HashSet};
use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::block::Block;
use crate::config::*;
use crate::context::RuntimeContext;
use crate::executor::{matches_pattern, extract_path_vars};
use crate::helpers::expand_env_vars;
use crate::observability::{ObservabilityBus, ObservabilityContext};
use crate::registry::{Registry, StructBlockFactory};
use crate::types::*;

/// A parsed reference to a remote block on GitHub, e.g.
/// `"github.com/acme/auth-block@v1.0.0"`.
#[cfg(feature = "wasm")]
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteBlockRef {
    pub owner: String,
    pub repo: String,
    pub version: String,
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
    if segments.len() != 3 || segments[0] != "github.com" {
        return None;
    }

    Some(RemoteBlockRef {
        owner: segments[1].to_string(),
        repo: segments[2].to_string(),
        version: version.to_string(),
    })
}

/// A parsed reference to a remote block on GitHub without a version, e.g.
/// `"github.com/acme/auth-block"`. The runtime resolves the latest release
/// that has a `.wasm` asset.
#[cfg(feature = "wasm")]
#[derive(Debug, Clone, PartialEq)]
pub struct UnversionedRemoteBlockRef {
    pub owner: String,
    pub repo: String,
}

/// Parse a block name into an `UnversionedRemoteBlockRef` if it matches the
/// `github.com/{owner}/{repo}` convention (no `@version` suffix).
///
/// Returns `None` when the name contains `@`, doesn't start with
/// `github.com/`, or has the wrong number of segments.
#[cfg(feature = "wasm")]
pub fn parse_unversioned_block(name: &str) -> Option<UnversionedRemoteBlockRef> {
    // Accept bare `github.com/owner/repo` or `github.com/owner/repo@latest`
    let name = name.strip_suffix("@latest").unwrap_or(name);

    if name.contains('@') {
        return None;
    }

    let segments: Vec<&str> = name.split('/').collect();
    if segments.len() != 3 || segments[0] != "github.com" {
        return None;
    }

    if segments[1].is_empty() || segments[2].is_empty() {
        return None;
    }

    Some(UnversionedRemoteBlockRef {
        owner: segments[1].to_string(),
        repo: segments[2].to_string(),
    })
}

/// Wafer is the WAFER runtime. It manages block registration, flow storage,
/// and execution.
pub struct Wafer {
    pub(crate) registry: Registry,
    pub(crate) flows: HashMap<String, Flow>,
    pub(crate) resolved: HashMap<String, Arc<dyn Block>>,
    /// Block configurations loaded from blocks.json (name → config JSON).
    pub(crate) block_configs: HashMap<String, serde_json::Value>,
    /// All registered blocks (infrastructure + application), shared with contexts.
    pub(crate) all_blocks: Arc<HashMap<String, Arc<dyn Block>>>,
    pub hooks: ObservabilityBus,
    /// Snapshot of registered block info (populated at start time).
    pub(crate) blocks_snapshot: Arc<Vec<crate::block::BlockInfo>>,
    /// Snapshot of flow info (populated at start time).
    pub(crate) flow_infos_snapshot: Arc<Vec<crate::config::FlowInfo>>,
    /// Snapshot of flow definitions (populated at start time).
    pub(crate) flow_defs_snapshot: Arc<Vec<crate::config::FlowDef>>,
    /// Shared WASM engine for all WASM blocks (enables epoch-based interruption).
    #[cfg(feature = "wasm")]
    pub(crate) wasm_engine: Option<Arc<wasmtime::Engine>>,
    /// Stop flag for the epoch ticker thread.
    #[cfg(feature = "wasm")]
    epoch_ticker_stop: Arc<AtomicBool>,
    /// Handle for the epoch ticker thread so it can be joined on shutdown.
    #[cfg(feature = "wasm")]
    epoch_ticker_handle: Option<std::thread::JoinHandle<()>>,
}

impl Wafer {
    /// Create a new Wafer runtime.
    ///
    /// Block factories are no longer auto-registered here. Call
    /// `wafer_core::register_all(&mut wafer)` to register infrastructure
    /// and application blocks.
    pub fn new() -> Self {
        let registry = Registry::new();

        Self {
            registry,
            flows: HashMap::new(),
            resolved: HashMap::new(),
            block_configs: HashMap::new(),
            all_blocks: Arc::new(HashMap::new()),
            hooks: ObservabilityBus::new(),
            blocks_snapshot: Arc::new(Vec::new()),
            flow_infos_snapshot: Arc::new(Vec::new()),
            flow_defs_snapshot: Arc::new(Vec::new()),
            #[cfg(feature = "wasm")]
            wasm_engine: None,
            #[cfg(feature = "wasm")]
            epoch_ticker_stop: Arc::new(AtomicBool::new(false)),
            #[cfg(feature = "wasm")]
            epoch_ticker_handle: None,
        }
    }

    /// Returns all blocks (resolved + infrastructure) as an Arc for use in contexts.
    fn all_blocks_arc(&self) -> Arc<HashMap<String, Arc<dyn Block>>> {
        self.all_blocks.clone()
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
            max_call_depth: 16,
            registered_blocks_snapshot: self.blocks_snapshot.clone(),
            flow_infos_snapshot: self.flow_infos_snapshot.clone(),
            flow_defs_snapshot: self.flow_defs_snapshot.clone(),
        }
    }

    /// Rebuild the all_blocks map from the resolved blocks.
    /// Call this after resolve() completes and after registering infrastructure blocks.
    pub fn rebuild_all_blocks(&mut self) {
        let mut map = HashMap::new();
        for (name, block) in &self.resolved {
            map.insert(name.clone(), block.clone());
        }
        self.all_blocks = Arc::new(map);
    }

    /// Registry returns the block registry.
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Load block configurations from a JSON file.
    ///
    /// The file should be a JSON object mapping block names to config objects.
    /// Environment variables in `${VAR}` format are expanded before parsing.
    ///
    /// Example:
    /// ```json
    /// {
    ///     "wafer/database": { "type": "sqlite", "path": "data/app.db" },
    ///     "wafer/crypto": { "jwt_secret": "${JWT_SECRET}" },
    ///     "wafer/logger": {}
    /// }
    /// ```
    pub fn load_blocks_json(&mut self, path: &str) -> Result<(), String> {
        let data = std::fs::read_to_string(path)
            .map_err(|e| format!("read blocks.json {}: {}", path, e))?;

        let expanded = expand_env_vars(&data);

        let map: HashMap<String, serde_json::Value> = serde_json::from_str(&expanded)
            .map_err(|e| format!("parse blocks.json: {}", e))?;

        for (name, config) in map {
            self.block_configs.insert(name, config);
        }

        Ok(())
    }

    /// Add a block configuration programmatically.
    pub fn add_block_config(&mut self, name: impl Into<String>, config: serde_json::Value) {
        self.block_configs.insert(name.into(), config);
    }

    /// HasBlock returns true if a block with the given type name is registered.
    pub fn has_block(&self, type_name: &str) -> bool {
        self.registry.has(type_name)
    }

    /// RegisterBlock registers a block instance under the given type name.
    /// The instance is also pre-resolved so it is available via `call_block()`
    /// even when it is not referenced as a flow node.
    pub fn register_block(&mut self, type_name: impl Into<String>, block: Arc<dyn Block>) {
        let name = type_name.into();
        let block_clone = block.clone();
        let name_clone = name.clone();
        if let Err(e) = self.registry.register(
            name_clone,
            Arc::new(StructBlockFactory {
                new_func: move || block_clone.clone(),
            }),
        ) {
            tracing::warn!(block = %name, error = %e, "registry registration failed for block");
        }
        self.resolved.insert(name, block);
    }

    /// RegisterBlockFunc registers an inline handler function as a block.
    pub fn register_block_func(
        &mut self,
        type_name: impl Into<String>,
        handler: impl Fn(&dyn crate::context::Context, &mut Message) -> Result_ + Send + Sync + 'static,
    ) {
        if let Err(e) = self.registry.register_func(type_name, handler) {
            tracing::warn!(error = %e, "registry registration failed for block func");
        }
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
            let entry = self
                .block_configs
                .entry(target)
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
    /// Before resolving flows, creates block instances from `block_configs`
    /// (loaded via `load_blocks_json` or `add_block_config`). Blocks that
    /// are already in `resolved` (from explicit `register_block` calls) are
    /// skipped.
    pub fn resolve(&mut self) -> Result<(), String> {
        // Gather uses contributions before creating blocks
        self.gather_uses_configs();

        // Resolve blocks from block_configs
        let configs: Vec<(String, serde_json::Value)> =
            self.block_configs.drain().collect();

        for (name, config) in configs {
            if self.resolved.contains_key(&name) {
                // Explicit register_block takes precedence
                continue;
            }
            if let Some(factory) = self.registry.get(&name) {
                let block = factory.create(Some(&config));

                // Run lifecycle Init
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
                            data: serde_json::to_vec(&config).unwrap_or_default(),
                        },
                    )
                    .map_err(|e| format!("init block {:?}: {}", name, e))?;

                self.resolved.insert(name.clone(), block);
            } else {
                tracing::warn!(block = %name, "block config present but no factory registered — skipping");
            }
        }

        let flow_ids: Vec<String> = self.flows.keys().cloned().collect();
        for flow_id in flow_ids {
            // Take flow out temporarily
            let mut flow = self.flows.remove(&flow_id).expect("BUG: flow disappeared during iteration");
            self.resolve_node(&mut flow.root)?;
            self.flows.insert(flow_id.clone(), flow);
        }
        Ok(())
    }

    fn resolve_node(&mut self, node: &mut Node) -> Result<(), String> {
        // Parse config map
        if let Some(ref config) = node.config {
            node.config_map = parse_config_map(config);
        }

        if !node.block.is_empty() {
            if let Some(block) = self.resolved.get(&node.block) {
                node.resolved_block = Some(block.clone());
            } else if let Some(factory) = self.registry.get(&node.block) {
                let block = factory.create(node.config.as_ref());

                // Initialize block
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
                    .map_err(|e| format!("init block {:?}: {}", node.block, e))?;

                self.resolved.insert(node.block.clone(), block.clone());
                node.resolved_block = Some(block);
            } else {
                // Try remote block download (wasm feature only)
                #[cfg(feature = "wasm")]
                {
                    let block = if let Some(remote_ref) = parse_versioned_block(&node.block) {
                        self.download_remote_block(&remote_ref)?
                    } else if let Some(remote_ref) = parse_unversioned_block(&node.block) {
                        self.resolve_latest_wasm_release(&remote_ref)?
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
                        .map_err(|e| format!("init remote block {:?}: {}", node.block, e))?;

                    self.resolved.insert(node.block.clone(), block.clone());
                    node.resolved_block = Some(block);
                }

                #[cfg(not(feature = "wasm"))]
                return Err(format!("block type not found: {}", node.block));
            }
        }

        for child in &mut node.next {
            self.resolve_node(child)?;
        }
        Ok(())
    }

    /// Download a remote block from GitHub Releases and load it as a sandboxed
    /// WASM block. The `.wasm` asset is expected at:
    /// `https://github.com/{owner}/{repo}/releases/download/{version}/{repo}.wasm`
    #[cfg(feature = "wasm")]
    fn download_remote_block(&mut self, r: &RemoteBlockRef) -> Result<Arc<dyn Block>, String> {
        use crate::wasm::WASMBlock;
        use crate::wasm::capabilities::BlockCapabilities;

        let url = format!(
            "https://github.com/{}/{}/releases/download/{}/{}.wasm",
            r.owner, r.repo, r.version, r.repo
        );

        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("failed to create HTTP client: {}", e))?;
        let resp = client
            .get(&url)
            .send()
            .map_err(|e| format!("failed to download {}: {}", url, e))?;

        let status = resp.status().as_u16();
        if status != 200 {
            return Err(format!("failed to download {}: HTTP {}", url, status));
        }

        let body = resp
            .bytes()
            .map_err(|e| format!("failed to read body from {}: {}", url, e))?;

        if body.is_empty() {
            return Err(format!("failed to download {}: empty response body", url));
        }

        let engine = self.wasm_engine().clone();
        let block = WASMBlock::load_with_engine(&engine, &body, BlockCapabilities::none())
            .map_err(|e| format!("failed to load remote block {}: {}", url, e))?;

        Ok(Arc::new(block))
    }

    /// Resolve the latest GitHub Release that has a `.wasm` asset and load it
    /// as a sandboxed WASM block.
    #[cfg(feature = "wasm")]
    fn resolve_latest_wasm_release(
        &mut self,
        r: &UnversionedRemoteBlockRef,
    ) -> Result<Arc<dyn Block>, String> {
        use crate::wasm::WASMBlock;
        use crate::wasm::capabilities::BlockCapabilities;

        let client = reqwest::blocking::Client::builder()
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
        let mut wasm_url: Option<String> = None;
        for release in &releases {
            for asset in &release.assets {
                if asset.name.ends_with(".wasm") {
                    wasm_url = Some(asset.browser_download_url.clone());
                    break;
                }
            }
            if wasm_url.is_some() {
                break;
            }
        }

        let wasm_url = wasm_url.ok_or_else(|| {
            format!(
                "no release with a .wasm asset found for {}/{}",
                r.owner, r.repo
            )
        })?;

        // 4. Download the .wasm asset
        let dl_resp = client
            .get(&wasm_url)
            .send()
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
            .map_err(|e| format!("failed to read body from {}: {}", wasm_url, e))?;

        if dl_body.is_empty() {
            return Err(format!(
                "failed to download {}: empty response body",
                wasm_url
            ));
        }

        // 5. Load via WASM engine
        let engine = self.wasm_engine().clone();
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

    /// Get or create the shared WASM engine with hardened configuration.
    #[cfg(feature = "wasm")]
    pub fn wasm_engine(&mut self) -> &wasmtime::Engine {
        if self.wasm_engine.is_none() {
            let mut config = wasmtime::Config::new();
            config.epoch_interruption(true);
            let engine = wasmtime::Engine::new(&config)
                .expect("failed to create hardened WASM engine");
            self.wasm_engine = Some(Arc::new(engine));
        }
        self.wasm_engine.as_ref().expect("BUG: wasm engine not initialized")
    }

    /// Start initializes the runtime.
    pub fn start(&mut self) -> Result<(), String> {
        if self.resolved.is_empty() {
            self.resolve()?;
        }

        // Rebuild the all_blocks map so contexts can see all resolved blocks
        self.rebuild_all_blocks();

        // Snapshot introspection data for contexts
        self.blocks_snapshot = Arc::new(self.registry.list());
        self.flow_infos_snapshot = Arc::new(self.flows_info());
        self.flow_defs_snapshot = Arc::new(self.flow_defs());

        // Spawn epoch ticker for WASM engine interrupt support
        #[cfg(feature = "wasm")]
        if let Some(ref engine) = self.wasm_engine {
            let engine = engine.clone();
            let stop = self.epoch_ticker_stop.clone();
            self.epoch_ticker_handle = Some(std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    engine.increment_epoch();
                }
            }));
        }

        Ok(())
    }

    /// Stop shuts down all resolved block instances.
    pub fn stop(&mut self) {
        #[cfg(feature = "wasm")]
        {
            self.epoch_ticker_stop.store(true, Ordering::Relaxed);
            if let Some(handle) = self.epoch_ticker_handle.take() {
                let _ = handle.join();
            }
        }

        let ctx = self.make_context(
            "shutdown",
            "shutdown",
            HashMap::new(),
            Arc::new(AtomicBool::new(false)),
            None,
        );
        for block in self.resolved.values() {
            let _ = block.lifecycle(
                &ctx,
                LifecycleEvent {
                    event_type: LifecycleType::Stop,
                    data: Vec::new(),
                },
            );
        }
    }

    /// Execute runs a flow by ID with the given message.
    pub fn execute(&self, flow_id: &str, msg: &mut Message) -> Result_ {
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

        let result = self.execute_node(&flow.root, msg, flow_id, &flow.config.on_error, &cancelled, deadline, &mut visited_flows, "root");

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

    fn execute_node(
        &self,
        node: &Node,
        msg: &mut Message,
        flow_id: &str,
        on_error: &str,
        cancelled: &Arc<AtomicBool>,
        deadline: Option<Instant>,
        visited_flows: &mut HashSet<String>,
        node_path: &str,
    ) -> Result_ {
        // Handle flow references
        if !node.flow.is_empty() {
            return self.execute_flow_ref(node, msg, on_error, cancelled, deadline, visited_flows);
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

        // Build context for this node
        let ctx = self.make_context(
            flow_id,
            node_path,
            node.config_map.clone(),
            cancelled.clone(),
            deadline,
        );

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
        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            block.handle(&ctx, msg)
        }));

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

        self.execute_first_match(&node.next, msg, flow_id, on_error, cancelled, deadline, visited_flows, node_path)
    }

    fn execute_flow_ref(
        &self,
        node: &Node,
        msg: &mut Message,
        on_error: &str,
        cancelled: &Arc<AtomicBool>,
        deadline: Option<Instant>,
        visited_flows: &mut HashSet<String>,
    ) -> Result_ {
        // Circular flow reference detection
        if visited_flows.contains(&node.flow) {
            return Result_ {
                action: Action::Error,
                error: Some(WaferError::new(
                    "circular_flow",
                    format!("circular flow reference detected: {}", node.flow),
                )),
                response: None,
                message: None,
            };
        }

        let target = match self.flows.get(&node.flow) {
            Some(c) => c,
            None => {
                return Result_ {
                    action: Action::Error,
                    error: Some(WaferError::new(
                        "not_found",
                        format!("referenced flow not found: {}", node.flow),
                    )),
                    response: None,
                    message: None,
                };
            }
        };

        visited_flows.insert(node.flow.clone());
        let result = self.execute_node(&target.root, msg, &target.id, &target.config.on_error, cancelled, deadline, visited_flows, "root");
        visited_flows.remove(&node.flow);

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
            );
        }

        result
    }

    fn execute_first_match(
        &self,
        nodes: &[Box<Node>],
        msg: &mut Message,
        flow_id: &str,
        on_error: &str,
        cancelled: &Arc<AtomicBool>,
        deadline: Option<Instant>,
        visited_flows: &mut HashSet<String>,
        parent_path: &str,
    ) -> Result_ {
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
            return self.execute_node(child, msg, flow_id, on_error, cancelled, deadline, visited_flows, &child_path);
        }
        Result_::continue_with(msg.clone())
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
