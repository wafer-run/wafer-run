use std::{
    collections::HashMap,
    sync::{atomic::AtomicBool, Arc},
};

use crate::{
    block::Block,
    context::RuntimeContext,
    error::RuntimeError,
    observability::ObservabilityBus,
    platform::{ConfigExpanderFn, Instant, RegistrarFn},
    types::*,
};

pub mod lifecycle;
pub mod registry;
pub mod resolver;
pub mod runner;
pub mod validation;

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
    pub async fn run(
        &self,
        flow_id: &str,
        msg: Message,
        input: wafer_block::InputStream,
    ) -> wafer_block::OutputStream {
        self.inner.run(flow_id, msg, input).await
    }

    /// Run a single block by name (bypasses flows).
    ///
    /// # Validation
    ///
    /// Top-level dispatch does **not** run the interface-action validator.
    /// That validator only runs on `RuntimeContext::call_block`, which is
    /// the path used when one block calls another. Callers invoking
    /// `run_block` are trusted (e.g., HTTP listeners) and are responsible
    /// for supplying actions the target block can handle.
    pub async fn run_block(
        &self,
        block_name: &str,
        msg: Message,
        input: wafer_block::InputStream,
    ) -> wafer_block::OutputStream {
        self.inner.run_block(block_name, msg, input).await
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
    pub(crate) aliases: Arc<HashMap<String, String>>,
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
    /// Block names that have already produced an "unknown interface" warning.
    /// Process-local; used by the call_block interface-action validator to
    /// emit the warning at most once per block.
    pub(crate) warned_unknown_interfaces: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// WRAP: all validated resource grants collected from blocks at startup.
    pub(crate) wrap_grants: Arc<Vec<wafer_block::types::ResourceGrant>>,
    /// WRAP: the block ID that has admin privileges (exact match).
    pub(crate) wrap_admin_block: Arc<String>,
    /// Effective capabilities per block after declared ∩ config ∩ host
    /// intersection. Computed at `resolve()` time. WASM blocks enforce
    /// against this; native blocks store for inspector visibility only.
    pub(crate) effective_capabilities:
        Arc<std::collections::HashMap<String, wafer_block::BlockCapabilities>>,
    /// Host-injected async loader for external wasm/js assets referenced by
    /// `BlockInfo::external_assets`. Defaults to `NoopAssetLoader`. Hosts
    /// that need lazy asset loading (e.g. solobase-web fetching
    /// ffmpeg-core.wasm from jsdelivr) call `set_asset_loader` during
    /// startup.
    pub(crate) asset_loader: Arc<dyn crate::asset_loader::LoadAssetCallback>,
    /// Shared WASM engine for all WASM blocks (fuel-metered).
    #[cfg(feature = "wasmi")]
    pub(crate) wasm_engine: Option<Arc<wasmi::Engine>>,
}

impl Wafer {
    /// Create a new Wafer runtime.
    pub fn new() -> Self {
        Self {
            blocks: HashMap::new(),
            flows: HashMap::new(),
            block_configs: HashMap::new(),
            all_blocks: Arc::new(HashMap::new()),
            aliases: Arc::new(HashMap::new()),
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
            warned_unknown_interfaces: Arc::new(std::sync::Mutex::new(Default::default())),
            wrap_grants: Arc::new(Vec::new()),
            wrap_admin_block: Arc::new(String::new()),
            effective_capabilities: Arc::new(std::collections::HashMap::new()),
            asset_loader: Arc::new(crate::asset_loader::NoopAssetLoader),
            #[cfg(feature = "wasmi")]
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
        Arc::make_mut(&mut self.aliases).insert(alias.into(), target.into());
    }

    /// Set the admin block ID for WRAP access control.
    /// Must be set before `start()` / `start_without_bind()`.
    pub fn set_admin_block(&mut self, block_id: impl Into<String>) {
        self.wrap_admin_block = Arc::new(block_id.into());
    }

    /// Get the collected WRAP grants (read-only).
    /// Available after `start()` / `start_without_bind()`.
    pub fn wrap_grants(&self) -> &Arc<Vec<wafer_block::types::ResourceGrant>> {
        &self.wrap_grants
    }

    /// Get the admin block ID (read-only).
    pub fn wrap_admin_block(&self) -> &Arc<String> {
        &self.wrap_admin_block
    }

    /// Register a host-side loader for external assets. Called during startup
    /// by hosts that need lazy asset loading. Replaces any previously
    /// registered loader.
    ///
    /// Propagates the new loader to all already-registered WASM blocks so that
    /// `set_asset_loader` and `register_block` can be called in any order.
    pub fn set_asset_loader(&mut self, loader: Arc<dyn crate::asset_loader::LoadAssetCallback>) {
        self.asset_loader = loader.clone();
        // Forward to all WasmiBlock instances currently registered.
        #[cfg(feature = "wasmi")]
        for block in self.blocks.values() {
            if let Some(wasmi_block) = block
                .as_any()
                .and_then(|any| any.downcast_ref::<crate::wasm::WasmiBlock>())
            {
                wasmi_block.set_asset_loader(loader.clone());
            }
        }
    }

    /// Return the currently registered asset loader. Defaults to
    /// `NoopAssetLoader` if `set_asset_loader` was never called.
    ///
    /// Returns a borrow to match the `wrap_grants()` / `wrap_admin_block()`
    /// pattern — callers who need ownership can `.clone()` themselves. This
    /// keeps the wasmi host-import hot path refcount-free.
    pub fn asset_loader(&self) -> &Arc<dyn crate::asset_loader::LoadAssetCallback> {
        &self.asset_loader
    }

    /// Look up the effective (declared ∩ config ∩ host) capabilities for a
    /// registered block. Returns `None` if the block did not declare and no
    /// config/host caps were provided.
    pub fn effective_capabilities(
        &self,
        block_name: &str,
    ) -> Option<&wafer_block::BlockCapabilities> {
        self.effective_capabilities.get(block_name)
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
            config: Arc::new(config),
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
            warned_unknown_interfaces: self.warned_unknown_interfaces.clone(),
            aliases: self.aliases.clone(),
            caller_requires: None, // unrestricted by default
            caller_id: None,       // top-level call, no caller
            wrap_grants: self.wrap_grants.clone(),
            wrap_admin_block: self.wrap_admin_block.clone(),
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
        for (alias, target) in self.aliases.as_ref() {
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

/// Convert a block name like `suppers-ai/auth` to its config variable prefix `SUPPERS_AI__AUTH__`.
///
/// Convention: `-` → `_`, `/` → `__`, uppercase, trailing `__`.
fn block_name_to_var_prefix(name: &str) -> String {
    let mut prefix = name.to_uppercase().replace('/', "__").replace('-', "_");
    prefix.push_str("__");
    prefix
}

/// Validate a block name follows the `{org}/{block}` convention.
///
/// Rules:
/// - Exactly two segments separated by `/`
/// - Each segment: lowercase `[a-z0-9-]`, no `_`, no consecutive `--`,
///   not starting or ending with `-`
/// - Minimum 1 char per segment
pub(crate) fn validate_block_name(name: &str) -> Result<(), RuntimeError> {
    let (org, block) = name
        .split_once('/')
        .ok_or_else(|| RuntimeError::InvalidBlockName {
            name: name.to_string(),
            reason: "must be {org}/{block}".to_string(),
        })?;
    if name.matches('/').count() != 1 {
        return Err(RuntimeError::InvalidBlockName {
            name: name.to_string(),
            reason: "exactly one / required".to_string(),
        });
    }
    for (label, segment) in [("org", org), ("block", block)] {
        if segment.is_empty() {
            return Err(RuntimeError::InvalidBlockName {
                name: name.to_string(),
                reason: format!("empty {label} segment"),
            });
        }
        if segment.contains('_') {
            return Err(RuntimeError::InvalidBlockName {
                name: name.to_string(),
                reason: format!("underscore not allowed in {label} segment, use hyphen"),
            });
        }
        if segment.contains("--") {
            return Err(RuntimeError::InvalidBlockName {
                name: name.to_string(),
                reason: format!("consecutive hyphens not allowed in {label} segment"),
            });
        }
        if segment.starts_with('-') || segment.ends_with('-') {
            return Err(RuntimeError::InvalidBlockName {
                name: name.to_string(),
                reason: format!("{label} segment cannot start or end with hyphen"),
            });
        }
        if !segment
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(RuntimeError::InvalidBlockName {
                name: name.to_string(),
                reason: format!(
                    "only lowercase alphanumeric and hyphens allowed in {label} segment"
                ),
            });
        }
    }
    Ok(())
}

impl wafer_block::registry::BlockRegistry for Wafer {
    fn register_block(&mut self, name: &str, block: Arc<dyn Block>) -> Result<(), RuntimeError> {
        self.register_block_inner(name, block)
    }

    fn add_alias(&mut self, alias: &str, target: &str) {
        Arc::make_mut(&mut self.aliases).insert(alias.to_string(), target.to_string());
    }

    fn add_block_config(&mut self, name: &str, config: serde_json::Value) {
        self.block_configs.insert(name.to_string(), config);
    }
}

impl Wafer {
    /// Shared validation + insertion logic used by both the `BlockRegistry`
    /// trait impl and the inherent `register_block()` method.
    fn register_block_inner(
        &mut self,
        name: &str,
        block: Arc<dyn Block>,
    ) -> Result<(), RuntimeError> {
        if self.blocks.contains_key(name) {
            return Err(RuntimeError::DuplicateBlock {
                name: name.to_string(),
            });
        }

        // Validate block name format
        validate_block_name(name)?;

        // Validate that all config_keys use the block's own prefix.
        // Block "suppers-ai/auth" may only declare keys starting with "SUPPERS_AI__AUTH__".
        let info = block.info();
        if !info.config_keys.is_empty() {
            let expected_prefix = block_name_to_var_prefix(name);
            for var in &info.config_keys {
                if !var.key.starts_with(&expected_prefix) {
                    return Err(RuntimeError::ConfigVarPrefix {
                        name: name.to_string(),
                        var: var.key.clone(),
                        prefix: expected_prefix,
                    });
                }
            }
        }

        // Propagate the current asset loader to the block before inserting.
        // Only WasmiBlock instances override `as_any()`, so native blocks are
        // skipped without any unsafe code.
        #[cfg(feature = "wasmi")]
        if let Some(wasmi_block) = block
            .as_any()
            .and_then(|any| any.downcast_ref::<crate::wasm::WasmiBlock>())
        {
            wasmi_block.set_asset_loader(self.asset_loader.clone());
        }

        self.blocks.insert(name.to_string(), block);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_block_name_to_var_prefix() {
        assert_eq!(
            block_name_to_var_prefix("suppers-ai/auth"),
            "SUPPERS_AI__AUTH__"
        );
        assert_eq!(
            block_name_to_var_prefix("wafer-run/web"),
            "WAFER_RUN__WEB__"
        );
        assert_eq!(
            block_name_to_var_prefix("suppers-ai/products"),
            "SUPPERS_AI__PRODUCTS__"
        );
    }

    #[test]
    fn test_validate_block_name_valid() {
        assert!(validate_block_name("suppers-ai/auth").is_ok());
        assert!(validate_block_name("wafer-run/sqlite").is_ok());
        assert!(validate_block_name("some-long-repo/test-block").is_ok());
        assert!(validate_block_name("a/b").is_ok());
        assert!(validate_block_name("org123/block456").is_ok());
    }

    #[test]
    fn test_validate_block_name_invalid() {
        // No slash
        assert!(validate_block_name("noSlash").is_err());
        // Two slashes
        assert!(validate_block_name("a/b/c").is_err());
        // Empty segment
        assert!(validate_block_name("/block").is_err());
        assert!(validate_block_name("org/").is_err());
        // Underscore
        assert!(validate_block_name("my_org/block").is_err());
        assert!(validate_block_name("org/my_block").is_err());
        // Consecutive hyphens
        assert!(validate_block_name("some--long/test").is_err());
        assert!(validate_block_name("org/test--block").is_err());
        // Leading/trailing hyphen
        assert!(validate_block_name("-org/block").is_err());
        assert!(validate_block_name("org/block-").is_err());
        assert!(validate_block_name("org/-block").is_err());
        // Uppercase
        assert!(validate_block_name("Org/block").is_err());
    }
}
