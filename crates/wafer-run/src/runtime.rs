use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use futures::FutureExt;

use crate::block::Block;
use crate::config::*;
use crate::context::RuntimeContext;
use crate::platform::{ConfigExpanderFn, Instant, RegistrarFn};

/// Maximum depth of nested `call_block()` invocations to prevent infinite recursion.
const DEFAULT_MAX_CALL_DEPTH: u32 = 16;
use crate::block::FuncBlock;
use crate::observability::{ObservabilityBus, ObservabilityContext};
use crate::types::*;

/// ABI version for WASM block compatibility.
pub const ABI_VERSION: u32 = 1;

/// A parsed reference to a remote block, e.g. `"wafer-run/sqlite@0.3.0"`.
#[cfg(feature = "wasm")]
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteBlockRef {
    pub org: String,
    pub block: String,
    pub version: String,
}

/// Parse a block name into a versioned `RemoteBlockRef` if it matches the
/// `{org}/{block}@{version}` convention.
///
/// Returns `None` for local block names (no `/`, no version,
/// wrong number of segments, or empty version).
#[cfg(feature = "wasm")]
pub fn parse_versioned_block(name: &str) -> Option<RemoteBlockRef> {
    let at_pos = name.rfind('@')?;
    let path = &name[..at_pos];
    let version = &name[at_pos + 1..];
    if version.is_empty() || version == "latest" {
        return None;
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() != 2 || segments.iter().any(|s| s.is_empty()) {
        return None;
    }
    Some(RemoteBlockRef {
        org: segments[0].to_string(),
        block: segments[1].to_string(),
        version: version.to_string(),
    })
}

/// Parse a block name into an unversioned `RemoteBlockRef` if it matches the
/// `{org}/{block}` convention. No `@version` suffix.
///
/// Returns `None` when the name has a version, no `/`, or wrong
/// number of segments.
#[cfg(feature = "wasm")]
pub fn parse_unversioned_block(name: &str) -> Option<RemoteBlockRef> {
    // Strip optional @latest suffix
    let name = name.strip_suffix("@latest").unwrap_or(name);
    if name.contains('@') {
        return None;
    }
    let segments: Vec<&str> = name.split('/').collect();
    if segments.len() != 2 || segments.iter().any(|s| s.is_empty()) {
        return None;
    }
    Some(RemoteBlockRef {
        org: segments[0].to_string(),
        block: segments[1].to_string(),
        version: "latest".to_string(),
    })
}

/// Registry manifest format for resolving remote blocks.
#[cfg(feature = "wasm")]
#[derive(serde::Deserialize)]
struct RegistryManifest {
    #[allow(dead_code)]
    name: String,
    latest: String,
    versions: HashMap<String, VersionEntry>,
}

/// A single version entry in a registry manifest.
#[cfg(feature = "wasm")]
#[derive(serde::Deserialize)]
struct VersionEntry {
    abi: u32,
    wasm_url: Option<String>,
    flow_url: Option<String>,
}

/// Thin, clonable handle that blocks can store to call flows from async tasks.
/// Native-only: requires `Block::bind()` which is not available on wasm32.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
pub struct RuntimeHandle {
    inner: Arc<Wafer>,
}

#[cfg(not(target_arch = "wasm32"))]
impl RuntimeHandle {
    /// Run a flow by ID.
    pub async fn run(&self, flow_id: &str, msg: &mut Message) -> Result_ {
        self.inner.run(flow_id, msg).await
    }

    /// Run a single block by name (bypasses flows).
    pub async fn run_block(&self, block_name: &str, msg: &mut Message) -> Result_ {
        self.inner.run_block(block_name, msg).await
    }
}

/// Wafer is the WAFER runtime. It manages block registration, flow storage,
/// and execution.
pub struct Wafer {
    pub(crate) blocks: HashMap<String, Arc<dyn Block>>,
    pub(crate) flows: HashMap<String, wafer_flow::WaferFlow>,
    /// Block configurations loaded from blocks.json (name → config JSON).
    pub(crate) block_configs: HashMap<String, serde_json::Value>,
    /// All registered blocks + aliases, shared with contexts.
    pub(crate) all_blocks: Arc<HashMap<String, Arc<dyn Block>>>,
    pub hooks: ObservabilityBus,
    /// Snapshot of registered block info (populated at start time).
    pub(crate) blocks_snapshot: Arc<Vec<crate::block::BlockInfo>>,
    /// Snapshot of flow info (populated at start time).
    pub(crate) flow_infos_snapshot: Arc<Vec<wafer_flow::FlowInfo>>,
    /// Snapshot of flow definitions (populated at start time).
    pub(crate) flow_defs_snapshot: Arc<Vec<wafer_flow::WaferFlow>>,
    /// Snapshot of expanded block configs (populated at start time, for inspector).
    pub(crate) block_configs_snapshot: Arc<HashMap<String, serde_json::Value>>,
    /// Alias mappings (e.g. `"wafer-run/database"` → `"wafer-run/sqlite"`). Alias names
    /// can be used wherever a block or flow name is expected.
    pub(crate) aliases: HashMap<String, String>,
    /// Config expanders: registered functions that split a composite config
    /// (e.g. `wafer-run/http-server`) into configs for individual blocks.
    pub(crate) config_expanders: HashMap<String, ConfigExpanderFn>,
    /// Named registrars: functions that register blocks/flows by name.
    /// Populated by crate consumers (e.g. wafer-core) so that
    /// `wafer.register("wafer-run/http-server", config)` works.
    pub(crate) registrars: HashMap<String, RegistrarFn>,
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
            registrars: HashMap::new(),
            hooks: ObservabilityBus::new(),
            blocks_snapshot: Arc::new(Vec::new()),
            flow_infos_snapshot: Arc::new(Vec::new()),
            flow_defs_snapshot: Arc::new(Vec::new()),
            block_configs_snapshot: Arc::new(HashMap::new()),
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

    /// Add a named registrar function. Registrars are called by
    /// [`register`](Self::register) to set up blocks, flows, and config
    /// by name.
    ///
    /// Typically called by crate consumers (e.g. wafer-core) to make
    /// their blocks available via `wafer.register("wafer-run/...", config)`.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn add_registrar(
        &mut self,
        name: impl Into<String>,
        f: impl Fn(&mut Wafer, serde_json::Value) + Send + Sync + 'static,
    ) {
        self.registrars.insert(name.into(), Box::new(f));
    }

    #[cfg(target_arch = "wasm32")]
    pub fn add_registrar(
        &mut self,
        name: impl Into<String>,
        f: impl Fn(&mut Wafer, serde_json::Value) + 'static,
    ) {
        self.registrars.insert(name.into(), Box::new(f));
    }

    /// Register a block or flow by name with the given config.
    ///
    /// If a registrar was previously added via [`add_registrar`](Self::add_registrar),
    /// it is called immediately. Otherwise, for names matching the
    /// `{org}/{block}` convention, the config is stored and the
    /// block or flow will be resolved during [`resolve()`](Self::resolve)
    /// (downloading `.flow.json` or `.wasm` via the registry).
    pub fn register(&mut self, name: &str, config: serde_json::Value) {
        if let Some(registrar) = self.registrars.remove(name) {
            registrar(self, config);
            self.registrars.insert(name.to_string(), registrar);
            return;
        }

        // No registrar — store config for deferred resolution during resolve().
        // The name must look like a remote ref (org/block).
        if !name.contains('/') {
            panic!("no registrar found for {:?} and name is not a remote ref", name);
        }
        tracing::debug!(name = %name, "no registrar found, deferring to resolve()");
        self.add_block_config(name, config);
    }

    /// Build a RuntimeContext with shared fields pre-filled.
    pub(crate) fn make_context(
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
            block_configs_snapshot: self.block_configs_snapshot.clone(),
            aliases: Arc::new(self.aliases.clone()),
            caller_requires: None, // unrestricted by default
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
    /// Native-only: requires filesystem access.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load_blocks_json(&mut self, path: &str) -> Result<(), String> {
        let data = std::fs::read_to_string(path)
            .map_err(|e| format!("read blocks.json {}: {}", path, e))?;

        let expanded = crate::helpers::expand_env_vars(&data);

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
    #[cfg(not(target_arch = "wasm32"))]
    pub fn add_config_expander(
        &mut self,
        name: impl Into<String>,
        expander: impl Fn(serde_json::Value) -> Vec<(String, serde_json::Value)> + Send + Sync + 'static,
    ) {
        self.config_expanders.insert(name.into(), Box::new(expander));
    }

    #[cfg(target_arch = "wasm32")]
    pub fn add_config_expander(
        &mut self,
        name: impl Into<String>,
        expander: impl Fn(serde_json::Value) -> Vec<(String, serde_json::Value)> + 'static,
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
    #[cfg(not(target_arch = "wasm32"))]
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
                collections: Vec::new(),
            },
            handler: Box::new(handler),
        });
        self.register_block(name, block);
    }

    /// RegisterBlockFunc (wasm32 variant — no Send + Sync bounds).
    #[cfg(target_arch = "wasm32")]
    pub fn register_block_func(
        &mut self,
        type_name: impl Into<String>,
        handler: impl Fn(&dyn crate::context::Context, &mut Message) -> Result_ + 'static,
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
                collections: Vec::new(),
            },
            handler: Box::new(handler),
        });
        self.register_block(name, block);
    }

    /// RegisterBlockFuncAsync registers an async inline handler function as a block.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn register_block_func_async<F, Fut>(
        &mut self,
        type_name: impl Into<String>,
        handler: F,
    ) where
        F: for<'a> Fn(&'a dyn crate::context::Context, &'a mut Message) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result_> + Send + 'static,
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
                collections: Vec::new(),
            },
            handler: Box::new(move |ctx, msg| Box::pin(handler(ctx, msg))),
        });
        self.register_block(name, block);
    }

    /// RegisterBlockFuncAsync (wasm32 variant — Sync only, no Send).
    #[cfg(target_arch = "wasm32")]
    pub fn register_block_func_async<F, Fut>(
        &mut self,
        type_name: impl Into<String>,
        handler: F,
    ) where
        F: for<'a> Fn(&'a dyn crate::context::Context, &'a mut Message) -> Fut + Sync + 'static,
        Fut: std::future::Future<Output = Result_> + 'static,
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
                collections: Vec::new(),
            },
            handler: Box::new(move |ctx, msg| Box::pin(handler(ctx, msg))),
        });
        self.register_block(name, block);
    }

    /// Shorthand for [`register_block_func`](Self::register_block_func).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn register_func(
        &mut self,
        type_name: impl Into<String>,
        handler: impl Fn(&dyn crate::context::Context, &mut Message) -> Result_ + Send + Sync + 'static,
    ) {
        self.register_block_func(type_name, handler);
    }

    #[cfg(target_arch = "wasm32")]
    pub fn register_func(
        &mut self,
        type_name: impl Into<String>,
        handler: impl Fn(&dyn crate::context::Context, &mut Message) -> Result_ + 'static,
    ) {
        self.register_block_func(type_name, handler);
    }

    /// Shorthand for [`register_block_func_async`](Self::register_block_func_async).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn register_func_async<F, Fut>(
        &mut self,
        type_name: impl Into<String>,
        handler: F,
    ) where
        F: for<'a> Fn(&'a dyn crate::context::Context, &'a mut Message) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result_> + Send + 'static,
    {
        self.register_block_func_async(type_name, handler);
    }

    #[cfg(target_arch = "wasm32")]
    pub fn register_func_async<F, Fut>(
        &mut self,
        type_name: impl Into<String>,
        handler: F,
    ) where
        F: for<'a> Fn(&'a dyn crate::context::Context, &'a mut Message) -> Fut + Sync + 'static,
        Fut: std::future::Future<Output = Result_> + 'static,
    {
        self.register_block_func_async(type_name, handler);
    }

    /// Register a WaferFlow definition.
    pub fn add_flow(&mut self, flow: wafer_flow::WaferFlow) {
        self.flows.insert(flow.id.clone(), flow);
    }

    /// Parse, validate, and register a WaferFlow from a JSON string.
    pub fn add_flow_json(&mut self, json: &str) -> Result<(), String> {
        let flow = wafer_flow::parse(json).map_err(|e| e.to_string())?;
        wafer_flow::validate(&flow).map_err(|errors| {
            errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; ")
        })?;
        self.add_flow(flow);
        Ok(())
    }

    /// Gather `"uses"` contributions from all block configs and deep-merge them
    /// into the target infrastructure block configs.
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

    /// Expand declarative `config_map` and `config_defaults` from WaferFlow definitions.
    fn expand_declarative_flow_configs(&mut self) {
        let eligible: Vec<(
            String,
            HashMap<String, wafer_flow::ConfigMapEntry>,
            HashMap<String, serde_json::Value>,
        )> = self
            .flows
            .values()
            .filter(|f| f.config_map.as_ref().map_or(false, |m| !m.is_empty()))
            .filter(|f| self.block_configs.contains_key(&f.id))
            .map(|f| {
                (
                    f.id.clone(),
                    f.config_map.clone().unwrap_or_default(),
                    f.config_defaults.clone().unwrap_or_default(),
                )
            })
            .collect();

        for (flow_id, config_map, config_defaults) in eligible {
            let flow_config = match self.block_configs.remove(&flow_id) {
                Some(c) => c,
                None => continue,
            };

            // 1. Apply config_defaults to target blocks
            for (target, defaults) in &config_defaults {
                let entry = self
                    .block_configs
                    .entry(target.clone())
                    .or_insert_with(|| serde_json::Value::Object(Default::default()));
                deep_merge(entry, defaults);
            }

            // 2. Route config_map keys to target blocks
            if let Some(obj) = flow_config.as_object() {
                for (user_key, mapping) in &config_map {
                    if let Some(value) = obj.get(user_key) {
                        let entry = self
                            .block_configs
                            .entry(mapping.target.clone())
                            .or_insert_with(|| serde_json::Value::Object(Default::default()));
                        let contribution =
                            serde_json::json!({ mapping.key.clone(): value.clone() });
                        deep_merge(entry, &contribution);
                    }
                }
            }
        }
    }

    fn gather_uses_configs(&mut self) {
        let mut contributions: Vec<(String, serde_json::Value)> = Vec::new();

        for config in self.block_configs.values() {
            if let Some(uses) = config.get("uses").and_then(|v| v.as_object()) {
                for (target, contrib) in uses {
                    contributions.push((target.clone(), contrib.clone()));
                }
            }
        }

        for (target, contrib) in contributions {
            let resolved_target = self.aliases.get(&target).cloned().unwrap_or(target);
            let entry = self
                .block_configs
                .entry(resolved_target)
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
            deep_merge(entry, &contrib);
        }

        for config in self.block_configs.values_mut() {
            if let Some(obj) = config.as_object_mut() {
                obj.remove("uses");
            }
        }
    }

    /// Resolve walks all flows and resolves block references.
    ///
    /// Before resolving flows, initializes all registered blocks via
    /// `lifecycle(Init)`. Blocks with configs (from `load_blocks_json` or
    /// `add_block_config`) are initialized first (infrastructure), then
    /// remaining blocks are initialized (features that may depend on infra).
    pub async fn resolve(&mut self) -> Result<(), String> {
        // Resolve remote entries: download .flow.json / .wasm for deferred registrations
        #[cfg(feature = "wasm")]
        self.resolve_remote_entries().await?;

        // Expand composite configs (e.g. wafer-run/http-server → http-listener + router)
        self.expand_composite_configs();
        // Expand declarative flow config_map / config_defaults
        self.expand_declarative_flow_configs();
        // Gather uses contributions before initializing blocks
        self.gather_uses_configs();

        // Snapshot expanded configs for inspector before draining
        self.block_configs_snapshot = Arc::new(self.block_configs.clone());

        let configs: Vec<(String, serde_json::Value)> =
            self.block_configs.drain().collect();

        // Collect names of all pre-registered blocks for phase 2 ordering.
        let pre_registered: Vec<String> = self.blocks.keys().cloned().collect();

        // Track which blocks were initialized with config data.
        let config_names: std::collections::HashSet<String> =
            configs.iter().map(|(n, _)| n.clone()).collect();

        // Sort configs: wafer-run/* infrastructure blocks first, then everything else.
        let mut infra_configs = Vec::new();
        let mut feature_configs = Vec::new();
        for entry in &configs {
            if entry.0.starts_with("wafer-run/") {
                infra_configs.push(entry);
            } else {
                feature_configs.push(entry);
            }
        }

        // Phase 1a: Initialize infrastructure blocks (wafer-run/*) with configs.
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

        // Phase 3: Verify flow block references exist.
        // Collect all referenced block names first to avoid borrow conflict.
        let referenced_blocks: Vec<String> = self
            .flows
            .values()
            .flat_map(|f| f.steps.iter())
            .map(|step| {
                self.aliases
                    .get(&step.block)
                    .cloned()
                    .unwrap_or_else(|| step.block.clone())
            })
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        for block_name in referenced_blocks {
            if self.blocks.contains_key(&block_name) {
                continue;
            }
            // Try WASM download
            #[cfg(feature = "wasm")]
            {
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .build()
                    .map_err(|e| format!("failed to create HTTP client: {}", e))?;
                match self.resolve_remote_block(&client, &block_name).await? {
                    Some(block) => {
                        tracing::info!(block = %block_name, "downloaded remote block");
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
                            .map_err(|e| format!("init remote block {:?}: {}", block_name, e))?;
                        self.blocks.insert(block_name.clone(), block);
                    }
                    None => {
                        return Err(format!("block type not found: {}", block_name));
                    }
                }
            }
            #[cfg(not(feature = "wasm"))]
            return Err(format!("block type not found: {}", block_name));
        }

        Ok(())
    }

    /// Resolve remote blocks for deferred registrations via the registry.
    #[cfg(feature = "wasm")]
    async fn resolve_remote_entries(&mut self) -> Result<(), String> {
        let candidates: Vec<String> = self
            .block_configs
            .keys()
            .filter(|name| name.contains('/'))
            .filter(|name| !self.flows.contains_key(name.as_str()))
            .filter(|name| !self.blocks.contains_key(name.as_str()))
            .filter(|name| {
                parse_unversioned_block(name).is_some() || parse_versioned_block(name).is_some()
            })
            .cloned()
            .collect();

        if candidates.is_empty() {
            return Ok(());
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("failed to create HTTP client: {}", e))?;

        for name in candidates {
            let remote_ref = parse_versioned_block(&name)
                .or_else(|| parse_unversioned_block(&name));
            let remote_ref = match remote_ref {
                Some(r) => r,
                None => continue,
            };

            let manifest_url = format!(
                "https://raw.githubusercontent.com/wafer-run/registry/main/{}/{}/manifest.json",
                remote_ref.org, remote_ref.block
            );

            let resp = client
                .get(&manifest_url)
                .header("User-Agent", "wafer-run/0.1.0")
                .send()
                .await
                .map_err(|e| format!("failed to fetch registry manifest for {}: {}", name, e))?;

            if resp.status().as_u16() == 404 {
                continue;
            }
            if resp.status().as_u16() != 200 {
                return Err(format!(
                    "failed to fetch registry manifest for {}: HTTP {}",
                    name, resp.status().as_u16()
                ));
            }

            let manifest_bytes = resp.bytes().await
                .map_err(|e| format!("failed to read manifest for {}: {}", name, e))?;
            let manifest: RegistryManifest = serde_json::from_slice(&manifest_bytes)
                .map_err(|e| format!("failed to parse registry manifest for {}: {}", name, e))?;

            let version = if remote_ref.version == "latest" {
                manifest.latest.clone()
            } else {
                remote_ref.version.clone()
            };

            let entry = manifest.versions.get(&version).ok_or_else(|| {
                format!("version {} not found in registry for {}", version, name)
            })?;

            if entry.abi != ABI_VERSION {
                return Err(format!(
                    "block {} version {} requires ABI {} but runtime supports ABI {}",
                    name, version, entry.abi, ABI_VERSION
                ));
            }

            if let Some(flow_url) = &entry.flow_url {
                let flow = self.download_flow_from_url(&client, flow_url, &name).await?;

                // Pre-resolve block dependencies from the flow's blocks list
                let blocks_to_resolve: Vec<String> = flow
                    .blocks
                    .as_ref()
                    .map(|b| b.iter()
                        .filter(|b| !self.blocks.contains_key(b.as_str()))
                        .cloned()
                        .collect())
                    .unwrap_or_default();

                self.add_flow(flow);

                for block_name in &blocks_to_resolve {
                    if self.blocks.contains_key(block_name.as_str()) {
                        continue;
                    }
                    match self.resolve_remote_block(&client, block_name).await {
                        Ok(Some(block)) => {
                            tracing::info!(block = %block_name, "downloaded remote block");
                            self.blocks.insert(block_name.clone(), block);
                        }
                        Ok(None) => {
                            tracing::debug!(
                                block = %block_name,
                                "block not found in registry, will resolve during step resolution"
                            );
                        }
                        Err(e) => {
                            return Err(format!(
                                "failed to download block dependency {:?} for flow {:?}: {}",
                                block_name, name, e
                            ));
                        }
                    }
                }
            } else if let Some(wasm_url) = &entry.wasm_url {
                let block = self.download_wasm_from_url(&client, wasm_url, &name).await?;
                tracing::info!(block = %name, "downloaded remote WASM block from registry");
                self.blocks.insert(name.clone(), block);
            }
        }

        Ok(())
    }

    /// Download a `.flow.json` from a direct URL and parse as WaferFlow.
    #[cfg(feature = "wasm")]
    async fn download_flow_from_url(
        &self,
        client: &reqwest::Client,
        url: &str,
        name: &str,
    ) -> Result<wafer_flow::WaferFlow, String> {
        let resp = client
            .get(url)
            .header("User-Agent", "wafer-run/0.1.0")
            .send()
            .await
            .map_err(|e| format!("failed to download flow for {}: {}", name, e))?;

        if resp.status().as_u16() != 200 {
            return Err(format!(
                "failed to download flow for {}: HTTP {}",
                name, resp.status().as_u16()
            ));
        }

        let body = resp.bytes().await
            .map_err(|e| format!("failed to read flow body for {}: {}", name, e))?;

        let body_str = std::str::from_utf8(&body)
            .map_err(|e| format!("failed to decode flow body for {}: {}", name, e))?;

        let flow = wafer_flow::parse(body_str)
            .map_err(|e| format!("failed to parse flow JSON for {}: {}", name, e))?;

        tracing::info!(flow = %flow.id, url = %url, "downloaded remote flow definition");
        Ok(flow)
    }

    /// Download a `.wasm` block from a direct URL.
    #[cfg(feature = "wasm")]
    async fn download_wasm_from_url(
        &mut self,
        client: &reqwest::Client,
        url: &str,
        name: &str,
    ) -> Result<Arc<dyn Block>, String> {
        use crate::wasm::WASMBlock;
        use crate::wasm::capabilities::BlockCapabilities;

        let resp = client
            .get(url)
            .header("User-Agent", "wafer-run/0.1.0")
            .send()
            .await
            .map_err(|e| format!("failed to download WASM for {}: {}", name, e))?;

        let status = resp.status().as_u16();
        if status != 200 {
            return Err(format!("failed to download WASM for {}: HTTP {}", name, status));
        }

        let body = resp.bytes().await
            .map_err(|e| format!("failed to read WASM body for {}: {}", name, e))?;

        if body.is_empty() {
            return Err(format!("failed to download WASM for {}: empty response body", name));
        }

        let engine = self.wasm_engine()?.clone();
        let block = WASMBlock::load_with_engine(&engine, &body, BlockCapabilities::none())
            .map_err(|e| format!("failed to load remote block {}: {}", name, e))?;

        Ok(Arc::new(block))
    }

    /// Resolve a remote block via the registry. Returns `Ok(None)` if the block
    /// is not found in the registry.
    #[cfg(feature = "wasm")]
    async fn resolve_remote_block(
        &mut self,
        client: &reqwest::Client,
        name: &str,
    ) -> Result<Option<Arc<dyn Block>>, String> {
        let remote_ref = parse_versioned_block(name)
            .or_else(|| parse_unversioned_block(name));
        let remote_ref = match remote_ref {
            Some(r) => r,
            None => return Ok(None),
        };

        let manifest_url = format!(
            "https://raw.githubusercontent.com/wafer-run/registry/main/{}/{}/manifest.json",
            remote_ref.org, remote_ref.block
        );

        let resp = client
            .get(&manifest_url)
            .header("User-Agent", "wafer-run/0.1.0")
            .send()
            .await
            .map_err(|e| format!("failed to fetch registry manifest for {}: {}", name, e))?;

        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        if resp.status().as_u16() != 200 {
            return Err(format!(
                "failed to fetch registry manifest for {}: HTTP {}",
                name, resp.status().as_u16()
            ));
        }

        let manifest_bytes = resp.bytes().await
            .map_err(|e| format!("failed to read manifest for {}: {}", name, e))?;
        let manifest: RegistryManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| format!("failed to parse registry manifest for {}: {}", name, e))?;

        let version = if remote_ref.version == "latest" {
            manifest.latest.clone()
        } else {
            remote_ref.version.clone()
        };

        let entry = manifest.versions.get(&version).ok_or_else(|| {
            format!("version {} not found in registry for {}", version, name)
        })?;

        if entry.abi != ABI_VERSION {
            return Err(format!(
                "block {} version {} requires ABI {} but runtime supports ABI {}",
                name, version, entry.abi, ABI_VERSION
            ));
        }

        if let Some(wasm_url) = &entry.wasm_url {
            let block = self.download_wasm_from_url(client, wasm_url, name).await?;
            Ok(Some(block))
        } else if let Some(flow_url) = &entry.flow_url {
            tracing::debug!(block = %name, flow_url = %flow_url, "block is a flow, not a WASM block");
            Ok(None)
        } else {
            let crate_name = format!("wafer-block-{}", remote_ref.block);
            Err(format!(
                "Block \"{}\" is native-only and must be compiled in.\n\
                 Add it with: cargo add {}",
                name, crate_name
            ))
        }
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
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn start(mut self) -> Result<Arc<Self>, String> {
        self.start_without_bind().await?;

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

        let arc_self = Arc::new(self);

        let handle = RuntimeHandle {
            inner: arc_self.clone(),
        };
        for (_, block) in &arc_self.blocks {
            block.bind(Box::new(handle.clone()));
        }

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

    /// Run a flow by ID with the given message.
    pub async fn run(&self, flow_id: &str, msg: &mut Message) -> Result_ {
        let flow = match self.flows.get(flow_id) {
            Some(f) => f,
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
        let timeout = flow.config.as_ref().and_then(|c| {
            // Prefer string timeout, fall back to timeout_ms
            if let Some(ref t) = c.timeout {
                let d = parse_duration(t);
                if !d.is_zero() { return Some(d); }
            }
            c.timeout_ms.map(std::time::Duration::from_millis)
        });
        let deadline = timeout.and_then(|t| {
            if !t.is_zero() { Some(Instant::now() + t) } else { None }
        });

        let result = crate::waferflow::execute_waferflow(flow, msg, self, &cancelled, deadline).await;

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

    /// Run a single block by name, bypassing flows.
    pub async fn run_block(&self, block_name: &str, msg: &mut Message) -> Result_ {
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

        let result = run_block_with_recovery(&*block, &ctx, msg).await;

        self.hooks.fire_block_end(&obs_ctx, &result, start.elapsed());

        result
    }

    /// Flows returns info about all loaded flows.
    pub fn flows_info(&self) -> Vec<wafer_flow::FlowInfo> {
        self.flows
            .values()
            .map(|f| wafer_flow::FlowInfo {
                id: f.id.clone(),
                name: f.name.clone(),
                description: f.description.clone(),
            })
            .collect()
    }

    /// Return all WaferFlow definitions.
    pub fn flow_defs(&self) -> Vec<wafer_flow::WaferFlow> {
        self.flows.values().cloned().collect()
    }
}

impl Default for Wafer {
    fn default() -> Self {
        Self::new()
    }
}

/// Execute a block with optional panic recovery.
/// On native: uses catch_unwind to isolate panics.
/// On wasm32: panics abort (handled by Workers runtime).
pub async fn run_block_with_recovery(
    block: &dyn Block,
    ctx: &dyn crate::context::Context,
    msg: &mut Message,
) -> Result_ {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let result = std::panic::AssertUnwindSafe(block.handle(ctx, msg))
            .catch_unwind()
            .await;
        match result {
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
        }
    }

    #[cfg(target_arch = "wasm32")]
    {
        block.handle(ctx, msg).await
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

// ---------------------------------------------------------------------------
// BlockRegistry implementation
// ---------------------------------------------------------------------------

impl wafer_block::registry::BlockRegistry for Wafer {
    fn register_block(&mut self, name: &str, block: Arc<dyn Block>) {
        self.blocks.insert(name.to_string(), block);
    }

    fn add_alias(&mut self, alias: &str, target: &str) {
        self.aliases.insert(alias.to_string(), target.to_string());
    }

    fn add_block_config(&mut self, name: &str, config: serde_json::Value) {
        self.block_configs.insert(name.to_string(), config);
    }
}
