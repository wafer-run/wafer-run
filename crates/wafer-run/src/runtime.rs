use std::{
    collections::HashMap,
    sync::{atomic::AtomicBool, Arc},
};

use wafer_block::{core_types::*, error::RuntimeError, Block};

use crate::{context::RuntimeContext, observability::ObservabilityBus, platform::Instant};

/// Config-expansion passes run during `seal()` (composite configs,
/// declarative flow configs, `uses` contributions).
pub(crate) mod config_expand;
pub mod config_source;
/// Flow-level execution policy (timeout resolution) for the dispatch path.
pub(crate) mod flow_policy;
/// Init-time call stack used to detect cycles when blocks `Init`-call each other.
pub mod init_stack;
/// Lifecycle orchestrator: drives `setup` → `validate_config` → `start` across all blocks.
pub mod lifecycle;
/// Block-registration core (registry maps + WRAP state), grouped out of `Wafer`.
pub(crate) mod registration;
/// Block registry — name-to-instance map populated via `Wafer::register_block`.
pub mod registry;
/// Remote-block machinery: reference parsing, registry manifest fetch, and
/// `.wasm` / `.flow.json` download (wasm feature).
#[cfg(feature = "wasm")]
pub(crate) mod remote;
/// Per-block runner with cancellation, timeout and observability hook wiring.
pub mod runner;
/// `seal()` — the once-per-boot finalization pipeline.
pub(crate) mod seal;
pub mod slot;
/// Post-registration validation of declared interfaces and block configs.
pub mod validation;
/// WASM runtime state (engine + asset loader), grouped out of `Wafer`.
pub(crate) mod wasm_state;

// Re-export the standalone function so external callers see it at the old path.
pub use runner::run_block_with_recovery;

/// Maximum depth of nested `call_block()` invocations to prevent infinite recursion.
const DEFAULT_MAX_CALL_DEPTH: u32 = 16;

// Re-export so consumers continue to write `wafer_run::ValidationReport` /
// `wafer_run::BrokenBlock`. The canonical definitions now live in
// `wafer-block` (alongside the `Context` trait whose
// `validate_all_block_configs` method returns them).
pub use wafer_block::{BrokenBlock, ValidationReport};

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
    /// Block-registration core: the registry maps (blocks, aliases, slots,
    /// registrars, expanders, interface specs, block configs) + nested WRAP
    /// grant/capability state.
    /// See [`RegistrationCore`](crate::runtime::registration::RegistrationCore).
    pub(crate) registration: crate::runtime::registration::RegistrationCore,
    /// Registered flows (id → flow). Read during execution.
    pub(crate) flows: HashMap<String, wafer_flow::WaferFlow>,
    /// Observability hook bus — exposed so consumers can register flow/block
    /// callbacks. `Arc`-shared with every [`RuntimeContext`] so nested
    /// block-to-block dispatch (`call_block`) fires the same hooks as the
    /// top-level dispatch paths.
    pub hooks: Arc<ObservabilityBus>,
    /// Single immutable bundle of post-startup metadata shared with every
    /// [`RuntimeContext`]. Populated at the end of [`Wafer::seal`].
    pub(crate) snapshot: Arc<crate::snapshot::StartupSnapshot>,
    /// Block names that have already produced an "unknown interface" warning.
    /// Process-local; used by the call_block interface-action validator to
    /// emit the warning at most once per block.
    pub(crate) warned_unknown_interfaces:
        Arc<parking_lot::Mutex<std::collections::HashSet<String>>>,
    /// WASM runtime state: shared fuel-metered engine + host-injected asset
    /// loader. See [`WasmState`](crate::runtime::wasm_state::WasmState).
    pub(crate) wasm: crate::runtime::wasm_state::WasmState,
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
    /// production callers wire in `EnvConfigSource` (the native app) or
    /// `D1ConfigSource` (the Cloudflare Workers app).
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
            registration: crate::runtime::registration::RegistrationCore::new(),
            flows: HashMap::new(),
            hooks: Arc::new(ObservabilityBus::new()),
            snapshot: crate::snapshot::StartupSnapshot::empty(),
            warned_unknown_interfaces: Arc::new(parking_lot::Mutex::new(Default::default())),
            wasm: crate::runtime::wasm_state::WasmState::new(),
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
        self.registration.all_blocks.clone()
    }

    /// Resolve a dispatch target through the alias map, returning the
    /// canonical name and a handle to the block.
    ///
    /// Tries the alias-resolved name first, then the raw name (the same
    /// canonicalize-then-fallback the flow runner and `RuntimeContext`
    /// dispatch use). This is the single accessor for block lookup so callers
    /// don't reach into `registration.all_blocks` directly.
    pub fn lookup_block<'a>(&'a self, name: &'a str) -> Option<(&'a str, Arc<dyn Block>)> {
        self.registration.lookup_with_alias(name)
    }

    /// Register an alias mapping. When `call_block(alias)` is called,
    /// it resolves to the target block name single-hop.
    ///
    /// The alias map is constrained to a forest of depth 1 — chained
    /// aliases are rejected at registration time so lookup is always
    /// O(1) and the canonical-name resolution rule is the same at
    /// `seal()` and at runtime. See [`wafer_block::error::AliasError`] for
    /// rejection reasons.
    pub fn add_alias(
        &mut self,
        alias: impl Into<String>,
        target: impl Into<String>,
    ) -> Result<(), wafer_block::error::AliasError> {
        self.registration.add_alias(alias.into(), target.into())
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
        self.registration.canonicalize(name)
    }

    /// Set the admin block ID for WRAP access control.
    /// Must be set before `start()` / `seal()`.
    ///
    /// Re-scans every already-registered block's typed WRAP grants
    /// (Network/Storage/Crypto) so admin-declared typed grants registered
    /// before this call are collected. This is the recommended path for
    /// embedders that auto-register blocks via `linkme` during
    /// `WaferBuilder::build()` and only know the admin block id after
    /// construction (e.g. the consuming application).
    ///
    /// External grants previously added via [`Wafer::add_wrap_grants`] are
    /// preserved across the rescan.
    pub fn set_admin_block(&mut self, block_id: impl Into<String>) {
        self.registration.set_admin_block(block_id.into());
    }

    /// Get the collected WRAP grants (read-only).
    /// Available after `start()` / `seal()`.
    pub fn wrap_grants(&self) -> &Arc<Vec<wafer_block::types::ResourceGrant>> {
        &self.registration.wrap.grants
    }

    /// Get the admin block ID (read-only).
    pub fn wrap_admin_block(&self) -> &Arc<String> {
        &self.registration.wrap.admin_block
    }

    /// Register a host-side loader for external assets. Called during startup
    /// by hosts that need lazy asset loading. Replaces any previously
    /// registered loader.
    ///
    /// Propagates the new loader to all already-registered WASM blocks so that
    /// `set_asset_loader` and `register_block` can be called in any order.
    pub fn set_asset_loader(&mut self, loader: &Arc<dyn crate::asset_loader::LoadAssetCallback>) {
        self.wasm.asset_loader = loader.clone();
        // Forward to all WasmiBlock instances currently registered.
        #[cfg(feature = "wasmi")]
        for block in self.registration.blocks.values() {
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
    pub fn block_infos(&self) -> Vec<wafer_block::BlockInfo> {
        lifecycle::sorted_snapshot(self.registration.blocks.values().map(|b| b.info()))
    }

    /// Return the registration key of every registered block, sorted for
    /// deterministic order across processes.
    ///
    /// Unlike [`Wafer::block_infos`] — which reports each block's
    /// self-declared `info().name` — these are the names registration
    /// validated and `init_block` / `call_block` resolve, so they are the
    /// correct iteration set for whole-runtime lifecycle drivers (e.g. a
    /// deploy-time init funnel).
    pub fn block_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.registration.blocks.keys().cloned().collect();
        names.sort();
        names
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

    /// Return the per-guest-call [`ResourceLimits`](crate::ResourceLimits)
    /// configured on this runtime via
    /// [`WaferBuilder::fuel_per_call`](crate::WaferBuilder::fuel_per_call) and
    /// [`WaferBuilder::max_wasm_memory_pages`](crate::WaferBuilder::max_wasm_memory_pages).
    /// Defaults to the bounded [`ResourceLimits::default`](crate::ResourceLimits::default)
    /// (100M metered fuel, 256-page / 16 MiB memory).
    ///
    /// Consumers that load WASM blocks directly — rather than through the
    /// runtime's shared engine — read this back to pass to
    /// [`WasmiBlock::load_from_bytes_with_limits`](crate::WasmiBlock::load_from_bytes_with_limits)
    /// so directly-registered blocks honour the same fuel budget and memory
    /// cap.
    pub fn resource_limits(&self) -> crate::ResourceLimits {
        self.wasm.resource_limits()
    }

    /// Return just the per-guest-call [`FuelLimit`](crate::FuelLimit) configured
    /// on this runtime. Convenience accessor over
    /// [`resource_limits`](Self::resource_limits) for callers that only care
    /// about fuel.
    pub fn fuel_limit(&self) -> crate::FuelLimit {
        self.wasm.fuel
    }

    /// Look up the effective (declared ∩ config ∩ host) capabilities for a
    /// registered block. Returns `None` if the block did not declare and no
    /// config/host caps were provided.
    pub fn effective_capabilities(
        &self,
        block_name: &str,
    ) -> Option<&wafer_block::BlockCapabilities> {
        self.registration
            .wrap
            .effective_capabilities
            .get(block_name)
    }

    /// Register an interface specification. Overwrites any existing spec
    /// with the same name.
    pub fn register_interface(&mut self, spec: wafer_block::InterfaceSpec) {
        self.registration.register_interface(spec);
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
            aliases: self.registration.aliases.clone(),
            caller_requires: None, // unrestricted by default
            caller_id: None,       // top-level call, no caller
            wrap_grants: self.registration.wrap.grants.clone(),
            wrap_admin_block: self.registration.wrap.admin_block.clone(),
            current_attachments: Arc::new(std::collections::BTreeMap::new()),
            init_breadcrumbs,
            slots: self.registration.slots.clone(),
            config_source: self.config.source.clone(),
            hooks: self.hooks.clone(),
        }
    }

    /// Build a context for executing `block_name`'s **own** code with its
    /// declared `requires` allowlist installed, so `call_block` is gated
    /// identically on every invocation path — top-level dispatch, flow step,
    /// nested call, and lifecycle (Init/Start/Stop).
    ///
    /// SEC-04: the bare [`Wafer::make_context`] leaves `caller_requires:
    /// None` (unrestricted) and must be reserved for true host/root
    /// operations, not block execution. Building a block's context through
    /// `make_context` alone let its permitted `call_block` set silently widen
    /// to "anything" whenever it ran as a flow step or during a lifecycle
    /// event, defeating the macro's documented security gate.
    pub(crate) fn make_block_context(
        &self,
        flow_id: impl Into<String>,
        block_name: &str,
        config: HashMap<String, String>,
        cancelled: Arc<AtomicBool>,
        deadline: Option<Instant>,
        init_breadcrumbs: crate::runtime::init_stack::InitStack,
    ) -> RuntimeContext {
        let mut ctx = self.make_context(
            flow_id,
            block_name,
            config,
            cancelled,
            deadline,
            init_breadcrumbs,
        );
        ctx.caller_requires = self.resolve_block_requires(block_name);
        ctx
    }

    /// Read a block's declared `requires` allowlist for context construction.
    /// Prefers the immutable startup snapshot (avoids rebuilding `BlockInfo`
    /// via `block.info()` on every dispatch); falls back to `block.info()`
    /// for a block registered after `seal()`. An empty or absent list yields
    /// `None` — an undeclared `requires` leaves the block's `call_block` set
    /// unrestricted, matching [`RuntimeContext::caller_requires`] semantics.
    fn resolve_block_requires(&self, resolved_block_name: &str) -> Option<Vec<String>> {
        self.snapshot
            .blocks
            .iter()
            .find(|b| b.name == resolved_block_name)
            .map(|b| b.requires.clone())
            .or_else(|| {
                self.registration
                    .blocks
                    .get(resolved_block_name)
                    .map(|b| b.info().requires)
            })
            .filter(|r| !r.is_empty())
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
            .registration
            .blocks
            .get(name)
            .ok_or_else(|| InitError::Permanent(format!("block not registered: {name}")))?
            .clone();
        // Every registered block — including remote ones downloaded by
        // `seal()` — pairs registration with a slot via `register_block_inner`
        // or `register_remote_block`. If `self.registration.blocks` contains `name` but
        // `self.registration.slots` does not, that is a runtime invariant violation; the
        // panic message points at the bug rather than masking it with a
        // fresh slot (which would let concurrent first-callers each run
        // `lifecycle(Init)` independently).
        let slot = self
            .registration
            .slots
            .get(name)
            .cloned()
            .expect("slot must exist for any registered block");

        // Build the lifecycle(Init) context. The stack we were just handed is
        // inherited into the context so any `init_block` call made transitively
        // by this block participates in the same cycle-detection frame.
        // SEC-04: `make_block_context` installs the block's `requires` so any
        // `call_block` made during Init is gated by the same allowlist as its
        // request-time calls.
        let init_ctx = self.make_block_context(
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
        crate::runtime::config_source::validate_block_configs(
            self.registration.blocks.iter(),
            &self.config.source,
        )
        .await
    }

    /// Rebuild the all_blocks map from registered blocks + aliases.
    /// Call this after resolve() completes.
    pub fn rebuild_all_blocks(&mut self) {
        self.registration.rebuild_all_blocks();
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

// ---------------------------------------------------------------------------
// BlockRegistry implementation
// ---------------------------------------------------------------------------

/// Convert a block name like `my-org/auth` to its config variable prefix `MY_ORG__AUTH__`.
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
        self.registration
            .register_block_inner(name, block, &self.wasm.asset_loader)
    }

    fn add_alias(
        &mut self,
        alias: &str,
        target: &str,
    ) -> Result<(), wafer_block::error::AliasError> {
        self.registration
            .add_alias(alias.to_string(), target.to_string())
    }

    fn add_block_config(&mut self, name: &str, config: serde_json::Value) {
        self.registration.add_block_config(name, config);
    }
}

impl Wafer {
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
            self.registration
                .register_block_inner(entry.name, block, &self.wasm.asset_loader)
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
    impl wafer_block::Block for NoopBlock {
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
            .registration
            .register_remote_block("some-org/remote", block)
            .expect("register_remote_block succeeds for valid block");

        assert!(
            wafer.registration.blocks.contains_key("some-org/remote"),
            "blocks map must contain remote block"
        );
        assert!(
            wafer.registration.slots.contains_key("some-org/remote"),
            "slots map must contain a slot for every registered block — \
             missing this lets concurrent first-callers each run lifecycle(Init)"
        );
    }

    #[test]
    fn test_block_name_to_var_prefix() {
        assert_eq!(block_name_to_var_prefix("my-org/auth"), "MY_ORG__AUTH__");
        assert_eq!(
            block_name_to_var_prefix("wafer-run/web"),
            "WAFER_RUN__WEB__"
        );
        assert_eq!(
            block_name_to_var_prefix("my-org/products"),
            "MY_ORG__PRODUCTS__"
        );
    }

    #[test]
    fn test_validate_block_name_valid() {
        assert!(validate_block_name("my-org/auth").is_ok());
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
            Err(wafer_block::error::AliasError::Cycle { alias }) => assert_eq!(alias, "x"),
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
            Err(wafer_block::error::AliasError::TargetIsAlias { alias, target }) => {
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
            Err(wafer_block::error::AliasError::AliasIsExistingTarget { alias }) => {
                assert_eq!(alias, "intermediate");
            }
            other => panic!("expected AliasIsExistingTarget error, got {other:?}"),
        }
    }
}
