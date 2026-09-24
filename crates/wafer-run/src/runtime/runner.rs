use std::sync::{atomic::AtomicBool, Arc};

use wafer_block::{
    core_types::*,
    streams::{input::InputStream, output::OutputStream},
    Block,
};

use super::Wafer;
use crate::{
    context::RuntimeContext,
    observability::ObservabilityBus,
    platform::Instant,
    waferflow::{
        executor::{FlowOutcome, TransferChain},
        plan::CompiledFlow,
        ResponderRecord,
    },
};

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

/// Lazy-init inputs for [`run_resolved`]: the resolved block and the context
/// its init is built from. Borrowed, so the steady state (init outcome
/// settled) pays nothing for them.
pub(crate) struct DispatchInit<'a> {
    /// The resolved target block.
    pub(crate) block: &'a Arc<dyn Block>,
    /// The dispatching frame's context: the template
    /// [`RuntimeContext::for_init`] builds the Init context from, and, through
    /// its `init_attempt`, the Init (if any) this dispatch waits on behalf of.
    pub(crate) template: &'a RuntimeContext,
}

/// Shared scaffolding for all three dispatch paths ([`Wafer::run_block`],
/// the flow executor's per-step dispatch, and
/// [`RuntimeContext::dispatch_call`]):
///
/// 1. Lazy init — fast path on `slot`'s settled outcome; on the first
///    dispatch (or while init is in flight or may be retried) run the init
///    pipeline, returning a failure as the typed error (`Err`) the caller
///    turns into its terminal.
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
    init: DispatchInit<'a>,
    msg: Message,
    input: InputStream,
    invoke: impl FnOnce(Message, InputStream) -> Fut,
) -> Result<T, WaferError>
where
    Fut: std::future::Future<Output = T>,
{
    // PERF-03: once a block's init outcome is settled, skip constructing the
    // dedicated init context per dispatch. `try_cached` returns `None` both
    // for "init may run now" and "init in flight" (mutex held) — the slow
    // path re-checks under the slot's lock, and the pipeline refuses a wait
    // that would close an init cycle before taking that lock.
    match target.slot.try_cached() {
        Some(Ok(_)) => {}
        Some(Err(e)) => {
            return Err(super::init_error_to_wafer_error(target.resolved, e));
        }
        None => {
            if let Err(e) =
                super::run_init_pipeline(target.resolved, init.block, target.slot, init.template)
                    .await
            {
                return Err(super::init_error_to_wafer_error(target.resolved, e));
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

/// The error for a flow id that names no flow.
pub(crate) fn flow_not_found(flow_id: &str) -> WaferError {
    WaferError::new(ErrorCode::NotFound, format!("flow not found: {flow_id}"))
}

/// A flow's execution plan: the seal-compiled one, or one compiled for this
/// invocation. See [`Wafer::flow_plan`].
pub(crate) enum FlowPlan<'a> {
    /// Compiled at `seal()`.
    Sealed(&'a CompiledFlow),
    /// Compiled for this invocation.
    AdHoc(Box<CompiledFlow>),
}

impl std::ops::Deref for FlowPlan<'_> {
    type Target = CompiledFlow;

    fn deref(&self) -> &CompiledFlow {
        match self {
            Self::Sealed(plan) => plan,
            Self::AdHoc(plan) => plan,
        }
    }
}

impl Wafer {
    /// Refuse top-level dispatch on a runtime [`seal`](Self::seal) has not
    /// sealed successfully: capabilities, the grant gate, the downloaded
    /// blocks and the snapshot every context reads are all computed there,
    /// so an unsealed or failed-seal runtime would dispatch blocks under
    /// their load-time capabilities and no grant check. `FailedPrecondition`,
    /// naming the seal failure when there was one.
    fn refuse_unless_sealed(&self) -> Option<OutputStream> {
        let reason = match &self.seal_state {
            super::SealState::Sealed => return None,
            super::SealState::Unsealed => {
                "runtime is not sealed: call seal() (or start()) before dispatching".to_string()
            }
            super::SealState::Failed(reason) => {
                format!("runtime failed to seal, so it cannot dispatch: {reason}")
            }
        };
        Some(OutputStream::error(WaferError::new(
            ErrorCode::FailedPrecondition,
            reason,
        )))
    }

    /// Run a flow by ID with the given message. Refused with
    /// `FailedPrecondition` unless [`seal`](Self::seal) succeeded.
    pub async fn run(&self, flow_id: &str, msg: Message, input: InputStream) -> OutputStream {
        if let Some(refused) = self.refuse_unless_sealed() {
            return refused;
        }
        match self.flow_plan(flow_id) {
            Some(plan) => self.run_plan(plan, msg, input).await,
            None => OutputStream::error(flow_not_found(flow_id)),
        }
    }

    /// The execution plan of flow `flow_id`, if it exists. Seal-compiled
    /// (PERF-03); a flow added after `seal()` is not in the plan and is
    /// compiled ad hoc for this invocation, which is no more work than the
    /// per-step reparsing the executor previously did every run.
    pub(crate) fn flow_plan(&self, flow_id: &str) -> Option<FlowPlan<'_>> {
        if let Some(compiled) = self.plan.flows.get(flow_id) {
            Some(FlowPlan::Sealed(compiled))
        } else {
            self.flows.get(flow_id).map(|flow| {
                FlowPlan::AdHoc(Box::new(crate::waferflow::plan::compile_flow(self, flow)))
            })
        }
    }

    /// Execute `plan`, then each flow a `next` transfer hands control to,
    /// as one [`TransferChain`]: one step budget, deadline and cancellation
    /// flag, tightened by every flow entered (see the executor's module
    /// docs). A loop, so a chain of transfers never nests a call.
    ///
    /// Observability: each flow fires `flow_start` when entered and
    /// `flow_end` once the chain has finished, last-entered first, so a
    /// transferring flow's duration encloses its target's.
    pub(crate) async fn run_plan(
        &self,
        plan: FlowPlan<'_>,
        msg: Message,
        input: InputStream,
    ) -> OutputStream {
        let mut chain = TransferChain::new();
        let mut entered: Vec<(FlowPlan<'_>, Instant)> = Vec::new();
        let (mut plan, mut msg, mut input) = (plan, msg, input);
        let mut responder_record = ResponderRecord::new();

        let output = loop {
            self.hooks.fire_flow_start(&plan.id, &msg);
            let start = Instant::now();
            chain.enter(&plan, start);
            let outcome = crate::waferflow::execute_waferflow(
                &plan,
                msg,
                input,
                self,
                &chain,
                responder_record,
            )
            .await;
            entered.push((plan, start));
            match outcome {
                FlowOutcome::Done(output) => break output,
                FlowOutcome::Transfer(transfer) => {
                    plan = transfer.plan;
                    msg = transfer.msg;
                    input = InputStream::from_bytes(transfer.body);
                    responder_record = transfer.responder_record;
                }
            }
        };

        for (plan, start) in entered.iter().rev() {
            self.hooks.fire_flow_end(&plan.id, start.elapsed());
        }
        output
    }

    /// Run a single block by name, bypassing flows. Refused with
    /// `FailedPrecondition` unless [`seal`](Self::seal) succeeded.
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
        if let Some(refused) = self.refuse_unless_sealed() {
            return refused;
        }
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
        // SEC-04: `make_block_context` installs the target's declared
        // `requires` allowlist so `call_block` is gated the same on every
        // invocation path (direct, flow step, nested, lifecycle).
        let ctx = self.make_block_context("", resolved, block_config, cancelled, None);

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
            DispatchInit {
                block: &block,
                template: &ctx,
            },
            msg,
            input,
            |msg, input| block.handle(&ctx, msg, input),
        )
        .await
        .unwrap_or_else(OutputStream::error)
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

/// The message a caught panic carried: its `&str` or `String` payload, else
/// `"unknown panic"`.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
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
            Err(panic_info) => OutputStream::error(WaferError::new(
                ErrorCode::Internal,
                format!("block panicked: {}", panic_message(&*panic_info)),
            )),
        }
    }

    #[cfg(target_arch = "wasm32")]
    {
        block.handle(ctx, msg, input).await
    }
}
