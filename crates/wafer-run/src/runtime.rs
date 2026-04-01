use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::block::Block;
use crate::context::RuntimeContext;
use crate::observability::ObservabilityBus;
use crate::platform::{ConfigExpanderFn, Instant, RegistrarFn};
use crate::types::*;

pub mod lifecycle;
pub mod registry;
pub mod resolver;
pub mod runner;

// Re-export the standalone function so external callers see it at the old path.
pub use runner::run_block_with_recovery;

/// Maximum depth of nested `call_block()` invocations to prevent infinite recursion.
const DEFAULT_MAX_CALL_DEPTH: u32 = 16;

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
pub(crate) struct RegistryManifest {
    #[allow(dead_code)]
    pub(crate) name: String,
    pub(crate) latest: String,
    pub(crate) versions: HashMap<String, VersionEntry>,
}

/// A single version entry in a registry manifest.
#[cfg(feature = "wasm")]
#[derive(serde::Deserialize)]
pub(crate) struct VersionEntry {
    pub(crate) abi: u32,
    pub(crate) wasm_url: Option<String>,
    pub(crate) flow_url: Option<String>,
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
    /// Registered interface specifications.
    pub(crate) interface_specs: HashMap<String, wafer_block::InterfaceSpec>,
    /// Snapshot of interface specs (populated at start time).
    pub(crate) interface_specs_snapshot: Arc<Vec<wafer_block::InterfaceSpec>>,
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
            interface_specs: wafer_block::interfaces::all()
                .into_iter()
                .map(|s| (s.name.clone(), s))
                .collect(),
            interface_specs_snapshot: Arc::new(Vec::new()),
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

    /// Register an interface specification. Overwrites any existing spec
    /// with the same name.
    pub fn register_interface(&mut self, spec: wafer_block::InterfaceSpec) {
        self.interface_specs.insert(spec.name.clone(), spec);
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
            interface_specs_snapshot: self.interface_specs_snapshot.clone(),
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
}

impl Default for Wafer {
    fn default() -> Self {
        Self::new()
    }
}

/// Deep-merge `src` into `dst`. For objects, keys are combined recursively.
/// For non-object values, `dst`'s existing value wins (contributors cannot
/// override the target block's own scalar values).
pub(crate) fn deep_merge(dst: &mut serde_json::Value, src: &serde_json::Value) {
    if let (serde_json::Value::Object(dst_map), serde_json::Value::Object(src_map)) = (dst, src) {
        for (key, src_val) in src_map {
            if let Some(dst_val) = dst_map.get_mut(key) {
                deep_merge(dst_val, src_val);
            } else {
                dst_map.insert(key.clone(), src_val.clone());
            }
        }
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
