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

pub mod config_source;
pub mod init_stack;
pub mod lifecycle;
pub mod registry;
pub mod resolver;
pub mod runner;
pub mod slot;
pub mod validation;

// Re-export the standalone function so external callers see it at the old path.
pub use runner::run_block_with_recovery;

/// Maximum depth of nested `call_block()` invocations to prevent infinite recursion.
const DEFAULT_MAX_CALL_DEPTH: u32 = 16;

/// ABI version for WASM block compatibility.
pub const ABI_VERSION: u32 = 1;

/// Result of [`Wafer::validate_all_block_configs`]. Used by the `/_health`
/// route to short-circuit boot when a required env var is missing.
#[derive(Debug, Clone)]
pub struct ValidationReport {
    /// Block names whose declared config keys all resolved successfully.
    /// Sorted lexicographically for deterministic output.
    pub ok: Vec<String>,
    /// Blocks with missing required keys or an unreachable config source.
    /// Sorted by `block` for deterministic output.
    pub broken: Vec<BrokenBlock>,
}

/// A single block that failed declared-key validation.
#[derive(Debug, Clone)]
pub struct BrokenBlock {
    pub block: String,
    /// Missing required keys. Currently carries at most one entry — see
    /// [`Wafer::validate_all_block_configs`] for why.
    pub missing_keys: Vec<String>,
}

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
    #[expect(
        dead_code,
        reason = "deserialized for round-trip fidelity; cross-checked elsewhere"
    )]
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
    /// Single immutable bundle of post-startup metadata shared with every
    /// [`RuntimeContext`]. Populated in two phases: `block_configs` is
    /// captured during `resolve()` before the working map is drained;
    /// the rest is filled in at the end of `start_without_bind`.
    pub(crate) snapshot: Arc<crate::snapshot::StartupSnapshot>,
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
    /// Per-block init slots populated by `register_block_inner` and consulted
    /// by [`Wafer::init_block`] for lazy-once-success caching.
    ///
    /// Wrapped in `Arc` (same shape as `aliases`) so [`RuntimeContext`] can
    /// share a cheap clone without copying the map on every dispatch.
    /// Mutations go through `Arc::make_mut` in `register_block_inner`.
    pub(crate) slots: Arc<HashMap<String, Arc<crate::runtime::slot::BlockSlot>>>,
    /// Source of per-block env-var config, consulted on first init.
    pub(crate) config_source: Arc<dyn crate::runtime::config_source::ConfigSource>,
}

impl Wafer {
    /// Construct a new Wafer runtime with default auto-registration:
    /// link-time `linkme` of `register_static_block!`-registered blocks
    /// (Path A) plus `./wafer.lock` cache loading (Path B). For finer
    /// control, use `Wafer::builder()`.
    ///
    /// `config_source` is the per-block env-var config source consulted on
    /// first init. Pass `Arc::new(StaticConfigSource::default())` for tests;
    /// production callers wire in `EnvConfigSource` (solobase-core) or
    /// `D1ConfigSource` (solobase-cloudflare).
    ///
    /// Returns an error if either path fails: a duplicate block name,
    /// a malformed lockfile, or a cache miss for a lockfile entry. A
    /// missing `./wafer.lock` is **not** an error — Path B simply
    /// no-ops in that case.
    pub fn new(
        config_source: Arc<dyn crate::runtime::config_source::ConfigSource>,
    ) -> Result<Self, RuntimeError> {
        Self::builder().config_source(config_source).build()
    }

    /// Builder for fine-grained control: opt-out of either auto-registration
    /// path, or point at a non-default lockfile location.
    pub fn builder() -> crate::WaferBuilder {
        crate::WaferBuilder::default()
    }

    /// Construct an empty Wafer with no blocks registered. Used by
    /// `WaferBuilder::build()` as the starting point before Path A
    /// (linkme static registration) and Path B (lockfile) populate registrations.
    pub(crate) fn empty() -> Self {
        Self {
            blocks: HashMap::new(),
            flows: HashMap::new(),
            block_configs: HashMap::new(),
            all_blocks: Arc::new(HashMap::new()),
            aliases: Arc::new(HashMap::new()),
            config_expanders: HashMap::new(),
            registrars: HashMap::new(),
            hooks: ObservabilityBus::new(),
            snapshot: crate::snapshot::StartupSnapshot::empty(),
            interface_specs: wafer_block::interfaces::all()
                .into_iter()
                .map(|s| (s.name.clone(), s))
                .collect(),
            warned_unknown_interfaces: Arc::new(std::sync::Mutex::new(Default::default())),
            wrap_grants: Arc::new(Vec::new()),
            wrap_admin_block: Arc::new(String::new()),
            effective_capabilities: Arc::new(std::collections::HashMap::new()),
            asset_loader: Arc::new(crate::asset_loader::NoopAssetLoader),
            #[cfg(feature = "wasmi")]
            wasm_engine: None,
            slots: Arc::new(HashMap::new()),
            config_source: Arc::new(crate::runtime::config_source::StaticConfigSource::default()),
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

    /// Return `BlockInfo` for every registered block. Used by consumers to
    /// generate discovery documents (e.g., OpenAPI, A2A agent.json) without
    /// having to maintain a duplicate registry.
    ///
    /// Sorted by block `name` for deterministic order across processes
    /// (independent of HashMap's SipHash randomisation). The returned list
    /// is a snapshot — later registrations are not reflected.
    pub fn block_infos(&self) -> Vec<crate::block::BlockInfo> {
        lifecycle::sorted_snapshot(self.blocks.values().map(|b| b.info()))
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
    ///
    /// `init_breadcrumbs` is the per-dispatch init cycle-detection stack.
    /// Top-level callers (HTTP listener, lifecycle, flow executor) pass
    /// `InitStack::new()`. Nested callers (the `init_block_with_stack`
    /// pipeline) pass the inherited stack so transitive `init_block`
    /// calls participate in the same frame.
    pub(crate) fn make_context(
        &self,
        flow_id: impl Into<String>,
        node_id: impl Into<String>,
        config: HashMap<String, String>,
        cancelled: Arc<AtomicBool>,
        deadline: Option<Instant>,
        init_breadcrumbs: crate::runtime::init_stack::InitStack,
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
            snapshot: self.snapshot.clone(),
            warned_unknown_interfaces: self.warned_unknown_interfaces.clone(),
            aliases: self.aliases.clone(),
            caller_requires: None, // unrestricted by default
            caller_id: None,       // top-level call, no caller
            wrap_grants: self.wrap_grants.clone(),
            wrap_admin_block: self.wrap_admin_block.clone(),
            current_attachments: Arc::new(std::collections::BTreeMap::new()),
            init_breadcrumbs,
            slots: self.slots.clone(),
            config_source: self.config_source.clone(),
        }
    }

    /// Lazily initialize the named block. Returns the cached `InitializedState`
    /// if init has already succeeded, or the cached `Permanent` error.
    ///
    /// On the first call:
    /// 1. Loads the block's declared env-config via [`ConfigSource::load_for_block`].
    /// 2. Serializes the resulting `HashMap<String,String>` to JSON bytes.
    /// 3. Invokes `block.lifecycle(Init { data })` with a fresh init-stack
    ///    `RuntimeContext` so any nested `init_block` call participates in
    ///    cycle detection.
    ///
    /// Outcome caching follows [`BlockSlot::get_or_init`]: `Ok` and
    /// [`InitError::Permanent`] are cached for the slot's lifetime;
    /// [`InitError::Transient`] and [`InitError::Cycle`] are not.
    pub async fn init_block(
        &self,
        name: &str,
    ) -> Result<crate::runtime::slot::InitializedState, crate::runtime::slot::InitError> {
        self.init_block_with_stack(name, &crate::runtime::init_stack::InitStack::new())
            .await
    }

    /// Same as [`Wafer::init_block`] but uses the caller's init-stack for
    /// cycle detection. Called from the top-level dispatch paths
    /// (`Wafer::run_block`, the flow executor) and from
    /// [`RuntimeContext::dispatch_call`] (block-to-block `call_block`) so
    /// init runs at most once per block per slot, with cycle detection
    /// across nested init.
    pub(crate) async fn init_block_with_stack(
        &self,
        name: &str,
        stack: &crate::runtime::init_stack::InitStack,
    ) -> Result<crate::runtime::slot::InitializedState, crate::runtime::slot::InitError> {
        use crate::runtime::slot::InitError;

        let block = self
            .blocks
            .get(name)
            .ok_or_else(|| InitError::Permanent(format!("block not registered: {name}")))?
            .clone();
        // Defensive slot allocation: `register_block_inner` pairs every
        // registration with a slot, but the (deprecated) resolver paths in
        // `runtime/resolver.rs` insert directly into `self.blocks` without
        // populating `self.slots`. Until Task 1.11 deletes those paths,
        // allocate a fresh slot on miss. Concurrent first-callers for a
        // resolver-inserted block won't share the same slot (minor gap), but
        // that's acceptable as a bridge — resolver-inserted blocks are not
        // hot dispatch targets.
        let slot = self
            .slots
            .get(name)
            .cloned()
            .unwrap_or_else(|| Arc::new(crate::runtime::slot::BlockSlot::new()));

        // Build the lifecycle(Init) context. The stack we were just handed is
        // inherited into the context so any `init_block` call made transitively
        // by this block participates in the same cycle-detection frame.
        let init_ctx = self.make_context(
            "init",
            name,
            std::collections::HashMap::new(),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            None,
            stack.clone(),
        );

        run_init_pipeline(
            name,
            block,
            slot,
            self.config_source.clone(),
            init_ctx,
            stack,
        )
        .await
    }

    /// Walk every registered block's [`BlockInfo::config_keys`] and ask the
    /// [`ConfigSource`](crate::runtime::config_source::ConfigSource) to load
    /// values. Reports which blocks have missing required keys or
    /// unreachable sources.
    ///
    /// Does **not** invoke any block's `lifecycle` or `handle`. Intended for
    /// the `/_health` route in wafer-site (PR 3) to short-circuit boot when
    /// a required env var is missing.
    ///
    /// # Limitation: single missing key per block
    ///
    /// [`ConfigError::MissingRequired`](crate::runtime::config_source::ConfigError::MissingRequired)
    /// carries only the first missing key — `load_for_block` short-circuits
    /// on the first miss. Each [`BrokenBlock`] therefore reports exactly one
    /// missing key, even if the block declares several required keys that
    /// are all absent. A richer report would require widening `ConfigError`
    /// to carry `Vec<String>`; that's intentionally out of scope for this
    /// change.
    pub async fn validate_all_block_configs(&self) -> ValidationReport {
        use crate::runtime::config_source::ConfigError;

        let mut report = ValidationReport {
            ok: Vec::new(),
            broken: Vec::new(),
        };
        for (name, block) in self.blocks.iter() {
            let info = block.info();
            match self
                .config_source
                .load_for_block(name, &info.config_keys)
                .await
            {
                Ok(_) => report.ok.push(name.clone()),
                Err(ConfigError::MissingRequired { block, key }) => {
                    report.broken.push(BrokenBlock {
                        block,
                        missing_keys: vec![key],
                    });
                }
                Err(ConfigError::Transient { block, .. }) => {
                    report.broken.push(BrokenBlock {
                        block,
                        missing_keys: vec!["<transient: source unreachable>".to_string()],
                    });
                }
            }
        }
        report.ok.sort();
        report.broken.sort_by(|a, b| a.block.cmp(&b.block));
        report
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

/// Convert a slot-level [`InitError`] into a [`WaferError`] for surfacing on
/// dispatch paths (`Wafer::run_block`, flow executor) where the public surface
/// is `OutputStream::error(...)`.
///
/// - `Permanent` / `Cycle` → `FAILED_PRECONDITION` (caller cannot recover by retrying).
/// - `Transient` → `UNAVAILABLE` (caller may retry).
pub(crate) fn init_error_to_wafer_error(
    block: &str,
    e: crate::runtime::slot::InitError,
) -> wafer_block::core_types::WaferError {
    use wafer_block::core_types::{ErrorCode, WaferError};

    use crate::runtime::slot::InitError;
    match e {
        InitError::Permanent(s) => WaferError::new(
            ErrorCode::FailedPrecondition,
            format!("block `{block}` init failed permanently: {s}"),
        ),
        InitError::Transient(s) => WaferError::new(
            ErrorCode::Unavailable,
            format!("block `{block}` init transient failure: {s}"),
        ),
        InitError::Cycle { path } => WaferError::new(
            ErrorCode::FailedPrecondition,
            format!("block `{block}` init cycle detected: {}", path.join(" -> ")),
        ),
    }
}

/// Shared body of the lazy-init pipeline. Used by both
/// [`Wafer::init_block_with_stack`] (top-level + transitive init from inside
/// `lifecycle(Init)`) and [`RuntimeContext::dispatch_call`] (init the callee
/// before block-to-block dispatch).
///
/// Pushes `name` onto the dispatch-scoped init stack, then delegates to the
/// slot's `get_or_init`. The push happens before locking the slot so a parent
/// frame already holding this block on the stack short-circuits with
/// `InitError::Cycle` before re-entering init. The guard pops on drop and
/// must outlive `get_or_init` so transitive `init_block` calls made from
/// inside `lifecycle(Init)` see this name on the stack.
pub(crate) async fn run_init_pipeline(
    name: &str,
    block: Arc<dyn Block>,
    slot: Arc<crate::runtime::slot::BlockSlot>,
    config_source: Arc<dyn crate::runtime::config_source::ConfigSource>,
    init_ctx: RuntimeContext,
    stack: &crate::runtime::init_stack::InitStack,
) -> Result<crate::runtime::slot::InitializedState, crate::runtime::slot::InitError> {
    use crate::runtime::{config_source::ConfigError, slot::InitError};

    let _guard = stack
        .push(name)
        .await
        .map_err(|path| InitError::Cycle { path })?;

    let block_name = name.to_string();
    let block_for_init = block.clone();
    let cfg_src = config_source;

    slot.get_or_init(|| async move {
        let info = block_for_init.info();
        let env_cfg = cfg_src
            .load_for_block(&block_name, &info.config_keys)
            .await
            .map_err(|e| match e {
                ConfigError::MissingRequired { block, key } => InitError::Permanent(format!(
                    "block `{block}` missing required config key `{key}`"
                )),
                ConfigError::Transient { source, .. } => {
                    InitError::Transient(format!("config fetch failed: {source}"))
                }
            })?;

        // Serialize as a JSON object of String→String so blocks can parse
        // the lifecycle(Init) event via the existing `BlockConfig::from_event`
        // pathway (which does serde_json::from_slice on event.data).
        let cfg_map = env_cfg.into_inner();
        let cfg_json: serde_json::Value = cfg_map
            .into_iter()
            .map(|(k, v)| (k, serde_json::Value::String(v)))
            .collect::<serde_json::Map<_, _>>()
            .into();
        let data = serde_json::to_vec(&cfg_json)
            .map_err(|e| InitError::Permanent(format!("serialize block config: {e}")))?;

        block_for_init
            .lifecycle(
                &init_ctx,
                wafer_block::core_types::LifecycleEvent {
                    event_type: wafer_block::core_types::LifecycleType::Init,
                    data,
                },
            )
            .await
            .map_err(|e| InitError::Permanent(format!("lifecycle init failed: {e}")))?;

        Ok(crate::runtime::slot::InitializedState::new())
    })
    .await
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
        // Pair every registration with a fresh init slot so `Wafer::init_block`
        // can lazily run lifecycle(Init) once per block. Mutate through
        // `Arc::make_mut` so live `RuntimeContext` clones sharing the previous
        // Arc keep their snapshot (registration after startup is rare; the
        // copy only happens on those occasional registrations).
        Arc::make_mut(&mut self.slots).insert(
            name.to_string(),
            Arc::new(crate::runtime::slot::BlockSlot::new()),
        );
        Ok(())
    }

    /// Path A: register every `#[wafer_block]`-annotated native block
    /// collected via `linkme` at link time. Called by
    /// `WaferBuilder::build()` when static registration is enabled (default).
    ///
    /// `linkme` preserves entries in the ELF section even for standalone crates
    /// that have no other code-level references from the consumer binary —
    /// unlike `inventory`, which was silently DCE'd by the linker for such
    /// crates.
    ///
    /// On collision (e.g. a block name registered twice across crates),
    /// surfaces `RuntimeError::Inventory { name, source }` wrapping the
    /// underlying `DuplicateBlock` so the offender is named.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn load_inventory_blocks(&mut self) -> Result<(), RuntimeError> {
        for entry in crate::static_registration::STATIC_BLOCK_REGISTRATIONS.iter() {
            let block = (entry.factory)();
            self.register_block_inner(entry.name, block)
                .map_err(|e| RuntimeError::Inventory {
                    name: entry.name.to_string(),
                    source: Box::new(e),
                })?;
            tracing::debug!(
                name = %entry.name,
                source = "linkme",
                "auto-registered block"
            );
        }
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
