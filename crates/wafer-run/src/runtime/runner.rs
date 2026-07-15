use std::sync::{atomic::AtomicBool, Arc};

use wafer_block::{
    core_types::*,
    streams::{input::InputStream, output::OutputStream},
    Block,
};

use super::Wafer;
use crate::{context::RuntimeContext, observability::ObservabilityBus, platform::Instant};

/// Identity fields for the observability bracket around one block dispatch.
pub(crate) struct DispatchObs<'a> {
    /// Flow in scope, or `""` outside flows.
    pub(crate) flow_id: &'a str,
    /// Node path within the flow tree (the resolved block name outside flows).
    pub(crate) node_path: &'a str,
    /// Block name as written by the caller (flow-step `block`, alias, or
    /// canonical name).
    pub(crate) block_name: &'a str,
}

/// The resolved dispatch target for [`run_resolved`]: the canonical block
/// name plus its once-success init slot.
pub(crate) struct DispatchTarget<'a> {
    /// Canonical (alias-resolved) block name — init identity and error
    /// attribution.
    pub(crate) resolved: &'a str,
    /// The block's once-success init slot.
    pub(crate) slot: &'a Arc<super::slot::BlockSlot>,
}

/// Lazy-init inputs for [`run_resolved`] — the resolved block plus the
/// config source, context and cycle-detection stack the init pipeline needs.
/// Built on demand (via the `make_init` closure) only when the block's init
/// outcome is not already cached, so the steady state pays none of it.
pub(crate) struct DispatchInit<'a> {
    /// The resolved target block.
    pub(crate) block: Arc<dyn Block>,
    /// Per-block env-var config source consulted on first init.
    pub(crate) config_source: Arc<dyn super::config_source::ConfigSource>,
    /// Context passed to `lifecycle(Init)`.
    pub(crate) init_ctx: RuntimeContext,
    /// Init cycle-detection stack for this dispatch.
    pub(crate) stack: &'a super::init_stack::InitStack,
}

/// Shared scaffolding for all three dispatch paths ([`Wafer::run_block`],
/// the flow executor's per-step dispatch, and
/// [`RuntimeContext::dispatch_call`]):
///
/// 1. Lazy init — fast path on `slot`'s cached outcome; on the first
///    dispatch (or while init is in flight) build the init inputs via
///    `make_init` and run the init pipeline, converting failures into a
///    terminal error stream (`Err`).
/// 2. Observability — bracket the dispatch in the opt-in
///    `block_start`/`block_end` hooks via [`ObservabilityBus::block_span`].
///
/// The invocation itself differs per path (plain `handle`, panic-recovery
/// wrapper + stream collection, wasmi attachment seeding), so it is supplied
/// as `invoke`; the observability span covers everything the returned future
/// awaits.
pub(crate) async fn run_resolved<'a, T, Fut>(
    hooks: &ObservabilityBus,
    obs: DispatchObs<'a>,
    target: DispatchTarget<'a>,
    make_init: impl FnOnce() -> DispatchInit<'a>,
    msg: Message,
    input: InputStream,
    invoke: impl FnOnce(Message, InputStream) -> Fut,
) -> Result<T, OutputStream>
where
    Fut: std::future::Future<Output = T>,
{
    // PERF-03: once a block's init outcome is cached, skip constructing the
    // dedicated init context and init-stack frame per dispatch. `try_cached`
    // returns `None` both for "never initialized" and "init in flight"
    // (mutex held) — the slow path re-checks under the slot's lock, and its
    // stack push still detects init cycles (a block mid-init always holds
    // the slot mutex, so a cyclic dispatch can never take the fast path).
    match target.slot.try_cached() {
        Some(Ok(_)) => {}
        Some(Err(e)) => {
            return Err(OutputStream::error(super::init_error_to_wafer_error(
                target.resolved,
                e,
            )));
        }
        None => {
            let init = make_init();
            if let Err(e) = super::run_init_pipeline(
                target.resolved,
                init.block,
                target.slot.clone(),
                init.config_source,
                init.init_ctx,
                init.stack,
            )
            .await
            {
                return Err(OutputStream::error(super::init_error_to_wafer_error(
                    target.resolved,
                    e,
                )));
            }
        }
    }

    let span = hooks.block_span(obs.flow_id, obs.node_path, obs.block_name, &msg);
    let out = invoke(msg, input).await;
    if let Some(span) = span {
        span.end();
    }
    Ok(out)
}

impl Wafer {
    /// Run a flow by ID with the given message.
    pub async fn run(&self, flow_id: &str, msg: Message, input: InputStream) -> OutputStream {
        // Seal-compiled plan (PERF-03). Flows added after `seal()` — or runs
        // on a not-yet-sealed runtime — are not in the plan and are compiled
        // ad hoc for this invocation, which is no more work than the
        // per-step reparsing the executor previously did every run.
        let ad_hoc;
        let compiled: &crate::waferflow::plan::CompiledFlow =
            if let Some(compiled) = self.plan.flows.get(flow_id) {
                compiled
            } else if let Some(flow) = self.flows.get(flow_id) {
                ad_hoc = crate::waferflow::plan::compile_flow(self, flow);
                &ad_hoc
            } else {
                return OutputStream::error(WaferError::new(
                    ErrorCode::NotFound,
                    format!("flow not found: {flow_id}"),
                ));
            };

        // Observability: flow start
        self.hooks.fire_flow_start(flow_id, &msg);
        let start = Instant::now();

        // Set up flow-level timeout via deadline (parsed once at compile).
        let cancelled = Arc::new(AtomicBool::new(false));
        let deadline = compiled.timeout.map(|t| Instant::now() + t);

        let result =
            crate::waferflow::execute_waferflow(compiled, msg, input, self, &cancelled, deadline)
                .await;

        // Observability: flow end
        self.hooks.fire_flow_end(flow_id, start.elapsed());

        result
    }

    /// Run a single block by name, bypassing flows.
    ///
    /// # Security
    ///
    /// This method bypasses WRAP access control. It is the trusted entry point
    /// for processing external HTTP requests — the HTTP adapter calls this to
    /// dispatch to the first block in the chain.
    ///
    /// `RuntimeHandle` (which exposes this method) must NEVER be passed to
    /// WASM blocks or untrusted code. Native blocks receive it via `bind()`
    /// during lifecycle, which is acceptable because native blocks are trusted
    /// (they run in the same process).
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
        input: InputStream,
    ) -> OutputStream {
        // Resolve alias + look up the target block in one step.
        let Some((resolved, block)) = self.registration.lookup_with_alias(block_name) else {
            return OutputStream::error(WaferError::new(
                ErrorCode::NotFound,
                format!("block not found: {block_name}"),
            ));
        };

        let cancelled = Arc::new(AtomicBool::new(false));
        // Seal-compiled block config (PERF-03): the flattened
        // `HashMap<String, String>` is parsed once at `seal()` and shared by
        // `Arc` — previously re-parsed from the JSON snapshot on every call.
        // The alias-resolved-then-raw key order matches `lookup_with_alias`:
        // `add_block_config` is keyed by registration name, which may be
        // either the alias or the target.
        let block_config = self.plan.config_for(resolved, block_name);

        // `node_id` is what the runtime uses to attribute WRAP access on
        // anything this block does on its own behalf (config/db/etc reads).
        // Using a literal `"root"` sentinel here meant every top-level
        // request appeared to come from a non-block caller — false denials.
        // Use the resolved block name instead. `flow_id` is empty (no flow
        // in scope at the top level).
        //
        // Top-level dispatch starts a fresh init-stack; any transitive
        // `init_block` calls inherit it through `RuntimeContext`.
        let init_stack = crate::runtime::init_stack::InitStack::new();
        // SEC-04: `make_block_context` installs the target's declared
        // `requires` allowlist so `call_block` is gated the same on every
        // invocation path (direct, flow step, nested, lifecycle).
        let ctx = self.make_block_context(
            "",
            resolved,
            block_config,
            cancelled,
            None,
            init_stack.clone(),
        );

        // Lazy init + observability bracket via the shared dispatch scaffold.
        let slot = self.slot_for(resolved);
        run_resolved(
            &self.hooks,
            DispatchObs {
                flow_id: "",
                node_path: resolved,
                block_name,
            },
            DispatchTarget {
                resolved,
                slot: &slot,
            },
            || self.dispatch_init(resolved, &block, &init_stack),
            msg,
            input,
            |msg, input| block.handle(&ctx, msg, input),
        )
        .await
        .unwrap_or_else(|init_failure| init_failure)
    }

    /// The once-success init slot paired with a registered block.
    ///
    /// Every registered block has a paired slot (`register_block_inner` /
    /// `register_remote_block`); a missing entry is a runtime invariant
    /// violation, so panic loudly rather than silently constructing a fresh
    /// slot (which would let concurrent callers each run `lifecycle(Init)`).
    pub(crate) fn slot_for(&self, resolved: &str) -> Arc<super::slot::BlockSlot> {
        self.registration
            .slots
            .get(resolved)
            .cloned()
            .expect("slot must exist for any registered block")
    }

    /// Build the lazy-init inputs for [`run_resolved`] from runtime state:
    /// the config source and a dedicated `lifecycle(Init)` context that
    /// inherits `stack` so transitive `init_block` calls participate in the
    /// same cycle-detection frame. Only called (via the `make_init` closure)
    /// when the block's init outcome is not already cached.
    pub(crate) fn dispatch_init<'a>(
        &self,
        resolved: &str,
        block: &Arc<dyn Block>,
        stack: &'a super::init_stack::InitStack,
    ) -> DispatchInit<'a> {
        DispatchInit {
            block: block.clone(),
            config_source: self.config.source.clone(),
            init_ctx: self.make_context(
                "init",
                resolved,
                self.plan.empty_config.clone(),
                Arc::new(AtomicBool::new(false)),
                None,
                stack.clone(),
            ),
            stack,
        }
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

/// Execute a block with optional panic recovery.
/// On native: uses catch_unwind to isolate panics.
/// On wasm32: panics abort (handled by Workers runtime).
pub async fn run_block_with_recovery(
    block: &dyn Block,
    ctx: &dyn crate::context::Context,
    msg: Message,
    input: InputStream,
) -> OutputStream {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use futures::FutureExt;
        let result = std::panic::AssertUnwindSafe(block.handle(ctx, msg, input))
            .catch_unwind()
            .await;
        match result {
            Ok(out) => out,
            Err(panic_info) => {
                let panic_msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_info.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic".to_string()
                };
                OutputStream::error(WaferError::new(
                    ErrorCode::Internal,
                    format!("block panicked: {panic_msg}"),
                ))
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    {
        block.handle(ctx, msg, input).await
    }
}
