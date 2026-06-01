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
/// Init-time call stack used to detect cycles when blocks `Init`-call each other.
pub mod init_stack;
/// Lifecycle orchestrator: drives `setup` → `validate_config` → `start` across all blocks.
pub mod lifecycle;
/// Block registry — name-to-instance map populated via `Wafer::register_block`.
pub mod registry;
/// Block-name resolver: aliases → registered native → URL → registry manifest.
pub mod resolver;
/// Per-block runner with cancellation, timeout and observability hook wiring.
pub mod runner;
pub mod slot;
/// Post-registration validation of declared interfaces and block configs.
pub mod validation;
/// WASM runtime state (engine + asset loader), grouped out of `Wafer`.
pub(crate) mod wasm_state;

// Re-export the standalone function so external callers see it at the old path.
pub use runner::run_block_with_recovery;

/// Maximum depth of nested `call_block()` invocations to prevent infinite recursion.
const DEFAULT_MAX_CALL_DEPTH: u32 = 16;

/// ABI version for WASM block compatibility.
pub const ABI_VERSION: u32 = 1;

// Re-export so consumers continue to write `wafer_run::ValidationReport` /
// `wafer_run::BrokenBlock`. The canonical definitions now live in
// `wafer-block` (alongside the `Context` trait whose
// `validate_all_block_configs` method returns them).
pub use wafer_block::{BrokenBlock, ValidationReport};

/// A parsed reference to a remote block, e.g. `"wafer-run/sqlite@0.3.0"`.
#[cfg(feature = "wasm")]
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteBlockRef {
    /// Org slug (left-hand side of `org/block`).
    pub org: String,
    /// Block name (right-hand side of `org/block`).
    pub block: String,
    /// Semver-style version following `@`.
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

#[cfg(not(target_arch = "wasm32"))]
#[wafer_block::wafer_async_trait]
impl wafer_block::Runtime for RuntimeHandle {
    async fn run(
        &self,
        flow_id: &str,
        msg: Message,
        input: wafer_block::InputStream,
    ) -> wafer_block::OutputStream {
        self.inner.run(flow_id, msg, input).await
    }

    async fn run_block(
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
    /// Observability hook bus — exposed so consumers can register flow/block callbacks.
    pub hooks: ObservabilityBus,
    /// Single immutable bundle of post-startup metadata shared with every
    /// [`RuntimeContext`]. Populated at the end of [`Wafer::seal`].
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
    /// WRAP: merged grant list (code-declared + external).
    /// This is what [`RuntimeContext`] receives and what
    /// [`Wafer::wrap_grants`] returns. Rebuilt by `set_admin_block` (so
    /// previously-deferred typed grants get re-collected) and by
    /// `add_wrap_grants`.
    pub(crate) wrap_grants: Arc<Vec<wafer_block::types::ResourceGrant>>,
    /// WRAP: extra grants supplied via [`Wafer::add_wrap_grants`] (e.g.
    /// loaded from a database). Kept separately from block-declared
    /// grants so `set_admin_block` can rebuild the code-declared portion
    /// from `self.blocks` without losing externally-injected entries.
    pub(crate) wrap_grants_external: Vec<wafer_block::types::ResourceGrant>,
    /// WRAP: the block ID that has admin privileges (exact match).
    pub(crate) wrap_admin_block: Arc<String>,
    /// Effective capabilities per block after declared ∩ config ∩ host
    /// intersection. Computed at `resolve()` time. WASM blocks enforce
    /// against this; native blocks store for inspector visibility only.
    pub(crate) effective_capabilities:
        Arc<std::collections::HashMap<String, wafer_block::BlockCapabilities>>,
    /// WASM runtime state: shared fuel-metered engine + host-injected asset
    /// loader. See [`WasmState`](crate::runtime::wasm_state::WasmState).
    pub(crate) wasm: crate::runtime::wasm_state::WasmState,
    /// Per-block init slots populated by `register_block_inner` (code-registered
    /// blocks) and `register_remote_block` (blocks downloaded during `seal()`).
    /// Consulted by [`Wafer::init_block`] for lazy-once-success caching.
    ///
    /// Wrapped in `Arc` (same shape as `aliases`) so [`RuntimeContext`] can
    /// share a cheap clone without copying the map on every dispatch.
    /// Mutations go through `Arc::make_mut` in those two registration paths.
    pub(crate) slots: Arc<HashMap<String, Arc<crate::runtime::slot::BlockSlot>>>,
    /// Accumulator for grant-validation failures from
    /// `validate_and_collect_grants_for_block`. Drained + checked by
    /// `Wafer::start()`; if non-empty, boot fails with
    /// `RuntimeError::GrantsRejected`.
    pub(crate) grant_validation_errors: Vec<crate::error::GrantValidationError>,
    /// Runtime configuration inputs: the lazy per-block [`ConfigSource`]
    /// consulted on first init, plus the embedder-supplied synchronous config
    /// snapshot layered under per-call config (populated via
    /// [`Wafer::set_config_snapshot`]). Both are cloned as a pair into every
    /// [`RuntimeContext`] [`make_context`](Self::make_context) produces.
    /// See [`ConfigState`](crate::runtime::config_source::ConfigState).
    pub(crate) config: crate::runtime::config_source::ConfigState,
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
            wrap_grants_external: Vec::new(),
            wrap_admin_block: Arc::new(String::new()),
            effective_capabilities: Arc::new(std::collections::HashMap::new()),
            wasm: crate::runtime::wasm_state::WasmState::new(),
            slots: Arc::new(HashMap::new()),
            grant_validation_errors: Vec::new(),
            config: crate::runtime::config_source::ConfigState::default_static(),
        }
    }

    /// Install an embedder-supplied env-style config snapshot. Replaces any
    /// previous snapshot. Wired into every subsequent
    /// [`RuntimeContext`](crate::context::RuntimeContext) via
    /// [`make_context`](Self::make_context); blocks read it via
    /// `ctx.config_get(key)`.
    ///
    /// Per-call config (e.g. `step.config` on a flow step) wins over the
    /// snapshot — see `Context::config_get` for the layering.
    ///
    /// Best called once before `seal()` / `start()`; calling later is allowed
    /// but contexts already produced (e.g. for in-flight `init_block` runs)
    /// hold the previous snapshot's `Arc` until they drop.
    pub fn set_config_snapshot(&mut self, snapshot: HashMap<String, String>) {
        self.config.set_snapshot(snapshot);
    }

    /// Borrow the embedder-supplied config snapshot installed via
    /// [`set_config_snapshot`](Self::set_config_snapshot). Returns an empty
    /// map if no snapshot has been installed.
    pub fn config_snapshot(&self) -> &Arc<HashMap<String, String>> {
        self.config.snapshot()
    }

    /// Returns all resolved blocks as an Arc for use in contexts.
    fn all_blocks_arc(&self) -> Arc<HashMap<String, Arc<dyn Block>>> {
        self.all_blocks.clone()
    }

    /// Register an alias mapping. When `call_block(alias)` is called,
    /// it resolves to the target block name single-hop.
    ///
    /// The alias map is constrained to a forest of depth 1 — chained
    /// aliases are rejected at registration time so lookup is always
    /// O(1) and the canonical-name resolution rule is the same at
    /// `seal()` and at runtime. See [`crate::error::AliasError`] for
    /// rejection reasons.
    pub fn add_alias(
        &mut self,
        alias: impl Into<String>,
        target: impl Into<String>,
    ) -> Result<(), crate::error::AliasError> {
        let alias = alias.into();
        let target = target.into();
        if alias == target {
            return Err(crate::error::AliasError::Cycle { alias });
        }
        if self.aliases.contains_key(&target) {
            return Err(crate::error::AliasError::TargetIsAlias { alias, target });
        }
        if self.aliases.values().any(|t| t == &alias) {
            return Err(crate::error::AliasError::AliasIsExistingTarget { alias });
        }
        Arc::make_mut(&mut self.aliases).insert(alias, target);
        Ok(())
    }

    /// Resolve `name` through the alias map, single-hop. Returns the
    /// canonical block name (the alias target if `name` is an alias)
    /// or `name` itself if `name` is not an alias.
    ///
    /// Single-hop is sufficient because [`Wafer::add_alias`] rejects
    /// chained registrations. When the alias semantics change in the
    /// future (case-folding, version-pinned aliases), this is the
    /// one site to update — instead of the six lookup sites that
    /// previously open-coded the same `aliases.get(...).unwrap_or(name)`
    /// pattern.
    pub fn canonicalize<'a>(&'a self, name: &'a str) -> &'a str {
        self.aliases.get(name).map(|s| s.as_str()).unwrap_or(name)
    }

    /// Set the admin block ID for WRAP access control.
    /// Must be set before `start()` / `seal()`.
    ///
    /// Re-scans every already-registered block's typed WRAP grants
    /// (Network/Storage/Crypto) so admin-declared typed grants registered
    /// before this call are collected. This is the recommended path for
    /// embedders that auto-register blocks via `linkme` during
    /// `WaferBuilder::build()` and only know the admin block id after
    /// construction (e.g. solobase).
    ///
    /// External grants previously added via [`Wafer::add_wrap_grants`] are
    /// preserved across the rescan.
    pub fn set_admin_block(&mut self, block_id: impl Into<String>) {
        self.wrap_admin_block = Arc::new(block_id.into());
        self.rebuild_wrap_grants();
    }

    /// Rebuild `self.wrap_grants` from scratch by walking every registered
    /// block's declared grants (filtered through the per-block validator)
    /// and concatenating the externally-supplied grants
    /// (`self.wrap_grants_external`). Called by `set_admin_block` and
    /// `add_wrap_grants`.
    fn rebuild_wrap_grants(&mut self) {
        self.grant_validation_errors.clear(); // full re-walk; old errors are stale
        let admin_block: String = (*self.wrap_admin_block).clone();
        let mut merged: Vec<wafer_block::types::ResourceGrant> = Vec::new();
        // Walk blocks in deterministic order for snapshot stability.
        let mut names: Vec<&String> = self.blocks.keys().collect();
        names.sort();
        for name in names {
            let block = &self.blocks[name];
            let info = block.info();
            let outcome = crate::runtime::lifecycle::validate_and_collect_grants_for_block(
                &info,
                &admin_block,
            );
            merged.extend(outcome.accepted);
            self.grant_validation_errors.extend(outcome.rejected);
        }
        merged.extend(self.wrap_grants_external.iter().cloned());
        self.wrap_grants = Arc::new(merged);
    }

    /// Get the collected WRAP grants (read-only).
    /// Available after `start()` / `seal()`.
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
        self.wasm.asset_loader = loader.clone();
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
        &self.wasm.asset_loader
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
            config_snapshot: self.config.snapshot.clone(),
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
            config_source: self.config.source.clone(),
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
        // Every registered block — including remote ones downloaded by
        // `seal()` — pairs registration with a slot via `register_block_inner`
        // or `register_remote_block`. If `self.blocks` contains `name` but
        // `self.slots` does not, that is a runtime invariant violation; the
        // panic message points at the bug rather than masking it with a
        // fresh slot (which would let concurrent first-callers each run
        // `lifecycle(Init)` independently).
        let slot = self
            .slots
            .get(name)
            .cloned()
            .expect("slot must exist for any registered block");

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
            self.config.source.clone(),
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
                .config
                .source
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

    let _guard = stack.push(name).map_err(|path| InitError::Cycle { path })?;

    let block_name = name.to_string();
    let block_for_init = block.clone();
    let cfg_src = config_source;
    // Snapshot of caller-registered JSON config (via `Wafer::add_block_config`).
    // Threaded into the init payload alongside env-resolved keys so blocks like
    // `wafer-run/router` (which read `"routes"` from `event.data`) still see
    // their config after lazy init. See the regression test
    // `init_merges_block_config`.
    let block_configs_snapshot = init_ctx.snapshot.block_configs.clone();

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

        // Build the lifecycle(Init).data payload: start from the JSON config
        // the caller registered via `Wafer::add_block_config` (if any), then
        // overlay env-resolved values on top. Env-config keys win — operators
        // can override JSON config via env vars.
        //
        // Blocks parse this via `BlockConfig::from_event`, which does
        // `serde_json::from_slice` on `event.data`. PR #98 originally serialized
        // only the env map, silently dropping `add_block_config` JSON (notably
        // `wafer-run/router`'s `"routes"` array). This merge restores the
        // pre-#98 contract.
        let mut merged: serde_json::Map<String, serde_json::Value> = block_configs_snapshot
            .get(&block_name)
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        for (k, v) in env_cfg.into_inner() {
            merged.insert(k, serde_json::Value::String(v));
        }
        let data = serde_json::to_vec(&serde_json::Value::Object(merged))
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

    fn add_alias(&mut self, alias: &str, target: &str) -> Result<(), crate::error::AliasError> {
        Wafer::add_alias(self, alias, target)
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

        // Validate this block's WRAP grants and append them to the
        // runtime-wide grant list. Grants are static `BlockInfo` metadata —
        // no init pass required. Typed grants (Network/Storage/Crypto)
        // declared by a block registered before `set_admin_block` are
        // deferred: `set_admin_block` re-scans every registered block and
        // re-collects them, so the registration order doesn't matter.
        let admin_block: String = (*self.wrap_admin_block).clone();
        let outcome =
            crate::runtime::lifecycle::validate_and_collect_grants_for_block(&info, &admin_block);
        if !outcome.accepted.is_empty() {
            let mut all = (*self.wrap_grants).clone();
            all.extend(outcome.accepted);
            self.wrap_grants = Arc::new(all);
        }
        self.grant_validation_errors.extend(outcome.rejected);

        // Propagate the current asset loader to the block before inserting.
        // Only WasmiBlock instances override `as_any()`, so native blocks are
        // skipped without any unsafe code.
        #[cfg(feature = "wasmi")]
        if let Some(wasmi_block) = block
            .as_any()
            .and_then(|any| any.downcast_ref::<crate::wasm::WasmiBlock>())
        {
            wasmi_block.set_asset_loader(self.wasm.asset_loader.clone());
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

    /// Insert a block downloaded by `seal()`'s remote-resolution path while
    /// running the same WRAP grant validation + slot allocation that
    /// `register_block_inner` performs for code-registered blocks.
    ///
    /// Remote blocks must not bypass WRAP grant collection or slot allocation
    /// — without a paired slot, concurrent first-callers would each construct
    /// their own `BlockSlot` and run `lifecycle(Init)` twice, breaking the
    /// once-only-success guarantee for stateful inits (migrations, idempotent
    /// setup). See `Wafer::init_block_with_stack` / `RuntimeContext::dispatch_call`.
    ///
    /// Block-name and config-key-prefix validation are intentionally skipped
    /// here: remote blocks come in under names the user already declared in
    /// flow definitions or block_configs, and re-validating now would cause
    /// `seal()` to reject blocks that were already accepted by the user's
    /// configuration. Duplicate-registration is also not checked because
    /// every remote-path call site filters `self.blocks.contains_key(name)`
    /// before invoking this helper.
    pub(crate) fn register_remote_block(
        &mut self,
        name: &str,
        block: Arc<dyn Block>,
    ) -> Result<(), RuntimeError> {
        let info = block.info();
        let admin_block: String = (*self.wrap_admin_block).clone();
        let outcome =
            crate::runtime::lifecycle::validate_and_collect_grants_for_block(&info, &admin_block);
        if !outcome.accepted.is_empty() {
            let mut all = (*self.wrap_grants).clone();
            all.extend(outcome.accepted);
            self.wrap_grants = Arc::new(all);
        }
        self.grant_validation_errors.extend(outcome.rejected);

        self.blocks.insert(name.to_string(), block);
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
        for entry in wafer_block::STATIC_BLOCK_REGISTRATIONS.iter() {
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
    use wafer_block_macro::wafer_async_trait;

    use super::*;

    /// Minimal `Block` for unit tests that need a registered handle.
    struct NoopBlock {
        info: wafer_block::BlockInfo,
    }

    #[wafer_async_trait]
    impl crate::block::Block for NoopBlock {
        fn info(&self) -> wafer_block::BlockInfo {
            self.info.clone()
        }
        async fn handle(
            &self,
            _ctx: &dyn wafer_block::context::Context,
            _msg: wafer_block::Message,
            _input: wafer_block::streams::input::InputStream,
        ) -> wafer_block::streams::output::OutputStream {
            wafer_block::streams::output::OutputStream::respond(vec![])
        }
        async fn lifecycle(
            &self,
            _ctx: &dyn wafer_block::context::Context,
            _event: wafer_block::LifecycleEvent,
        ) -> std::result::Result<(), wafer_block::WaferError> {
            Ok(())
        }
    }

    /// Remote-resolution path (`seal()` downloads referenced blocks not yet
    /// registered) must pair each insertion with a `BlockSlot`. Without
    /// this, concurrent first-callers each construct their own slot and
    /// both run `lifecycle(Init)` — breaking the once-only-success
    /// guarantee for stateful inits (migrations, idempotent setup).
    #[test]
    fn register_remote_block_pairs_blocks_and_slot() {
        let mut wafer = Wafer::builder()
            .disable_inventory()
            .disable_lockfile()
            .build()
            .expect("empty wafer build is infallible");

        let block = Arc::new(NoopBlock {
            info: wafer_block::BlockInfo::new("some-org/remote", "0.1.0", "iface@v1", "test"),
        });
        wafer
            .register_remote_block("some-org/remote", block)
            .expect("register_remote_block succeeds for valid block");

        assert!(
            wafer.blocks.contains_key("some-org/remote"),
            "blocks map must contain remote block"
        );
        assert!(
            wafer.slots.contains_key("some-org/remote"),
            "slots map must contain a slot for every registered block — \
             missing this lets concurrent first-callers each run lifecycle(Init)"
        );
    }

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

    fn test_wafer() -> Wafer {
        Wafer::builder()
            .disable_inventory()
            .disable_lockfile()
            .build()
            .expect("empty wafer build is infallible")
    }

    fn test_ctx(w: &Wafer, overrides: HashMap<String, String>) -> RuntimeContext {
        w.make_context(
            "test-flow",
            "test-node",
            overrides,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            None,
            crate::runtime::init_stack::InitStack::new(),
        )
    }

    #[test]
    fn config_snapshot_is_visible_via_config_get() {
        use wafer_block::context::Context as _;
        let mut w = test_wafer();
        let mut snap = HashMap::new();
        snap.insert("KEY".to_string(), "from-snapshot".to_string());
        w.set_config_snapshot(snap);
        let ctx = test_ctx(&w, HashMap::new());
        assert_eq!(ctx.config_get("KEY"), Some("from-snapshot"));
        assert_eq!(ctx.config_get("UNSET"), None);
    }

    #[test]
    fn per_call_override_wins_over_snapshot() {
        use wafer_block::context::Context as _;
        let mut w = test_wafer();
        let mut snap = HashMap::new();
        snap.insert("KEY".to_string(), "from-snapshot".to_string());
        w.set_config_snapshot(snap);
        let mut ov = HashMap::new();
        ov.insert("KEY".to_string(), "from-override".to_string());
        let ctx = test_ctx(&w, ov);
        assert_eq!(ctx.config_get("KEY"), Some("from-override"));
    }

    #[test]
    fn snapshot_default_is_empty_and_config_get_returns_none() {
        use wafer_block::context::Context as _;
        let w = test_wafer();
        let ctx = test_ctx(&w, HashMap::new());
        assert_eq!(ctx.config_get("ANY"), None);
        assert!(w.config_snapshot().is_empty());
    }

    #[test]
    fn override_only_keys_still_resolve_with_no_snapshot_set() {
        use wafer_block::context::Context as _;
        let w = test_wafer();
        let mut ov = HashMap::new();
        ov.insert("KEY".to_string(), "from-override".to_string());
        let ctx = test_ctx(&w, ov);
        assert_eq!(ctx.config_get("KEY"), Some("from-override"));
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

    #[tokio::test]
    async fn canonicalize_returns_input_for_unaliased_name() {
        let cfg_src: std::sync::Arc<dyn crate::ConfigSource> =
            std::sync::Arc::new(crate::StaticConfigSource::default());
        let wafer = Wafer::new(cfg_src).expect("Wafer::new");
        assert_eq!(wafer.canonicalize("wafer-run/sqlite"), "wafer-run/sqlite");
    }

    #[tokio::test]
    async fn canonicalize_returns_target_for_aliased_name() {
        let cfg_src: std::sync::Arc<dyn crate::ConfigSource> =
            std::sync::Arc::new(crate::StaticConfigSource::default());
        let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
        wafer
            .add_alias("db", "wafer-run/database")
            .expect("add_alias");
        assert_eq!(wafer.canonicalize("db"), "wafer-run/database");
    }

    #[tokio::test]
    async fn add_alias_rejects_cycle_to_self() {
        let cfg_src: std::sync::Arc<dyn crate::ConfigSource> =
            std::sync::Arc::new(crate::StaticConfigSource::default());
        let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
        match wafer.add_alias("x", "x") {
            Err(crate::error::AliasError::Cycle { alias }) => assert_eq!(alias, "x"),
            other => panic!("expected Cycle error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn add_alias_rejects_target_that_is_an_existing_alias_key() {
        let cfg_src: std::sync::Arc<dyn crate::ConfigSource> =
            std::sync::Arc::new(crate::StaticConfigSource::default());
        let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
        wafer
            .add_alias("intermediate", "wafer-run/router")
            .expect("first registration succeeds");
        match wafer.add_alias("my-router", "intermediate") {
            Err(crate::error::AliasError::TargetIsAlias { alias, target }) => {
                assert_eq!(alias, "my-router");
                assert_eq!(target, "intermediate");
            }
            other => panic!("expected TargetIsAlias error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn add_alias_rejects_alias_that_is_an_existing_target() {
        let cfg_src: std::sync::Arc<dyn crate::ConfigSource> =
            std::sync::Arc::new(crate::StaticConfigSource::default());
        let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
        wafer
            .add_alias("my-router", "intermediate")
            .expect("first registration succeeds");
        match wafer.add_alias("intermediate", "wafer-run/router") {
            Err(crate::error::AliasError::AliasIsExistingTarget { alias }) => {
                assert_eq!(alias, "intermediate");
            }
            other => panic!("expected AliasIsExistingTarget error, got {other:?}"),
        }
    }
}
