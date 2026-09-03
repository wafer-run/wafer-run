//! Store/instance construction and the per-call context scope.
//!
//! - [`apply_fuel`] sets a store's fuel budget (honoring `Unmetered`).
//! - [`instantiate`] builds a fresh store + instance, running `_start` for
//!   TinyGo modules.
//! - [`ContextScope`] is the RAII guard that installs the borrowed
//!   [`Context`](crate::context::Context) into the store for one invocation.

use wafer_block::{core_types::MetaEntry, error::RuntimeError};
use wasmi::{Engine, Linker, Module, Store};

use super::{
    super::{capabilities::BlockCapabilities, stream::StreamRegistry},
    abi::{ProcExitTrap, WasmiHostState},
    codec::{negotiate_host_codec, HostCodec},
};
use crate::{
    context::Context,
    runtime::wasm_state::{FuelLimit, ResourceLimits},
};

/// Apply the configured [`FuelLimit`] to a freshly-prepared store.
///
/// `Metered(n)` sets the store's fuel to `n`; `Unmetered` is a no-op — wasmi
/// rejects `set_fuel` when the engine has `consume_fuel(false)`, so the loader
/// must skip it. `ctx` labels the error site (initial set vs. post-`_start`
/// refill).
pub(super) fn apply_fuel(
    store: &mut Store<WasmiHostState>,
    fuel: FuelLimit,
    ctx: &str,
) -> Result<(), RuntimeError> {
    if let FuelLimit::Metered(n) = fuel {
        store
            .set_fuel(n)
            .map_err(|e| RuntimeError::Wasm(format!("{ctx}: {e}")))?;
    }
    Ok(())
}

/// Create a fresh store + instance from the pre-built linker and module.
///
/// For TinyGo WASM modules (wasi target) the exported `_start` function must
/// be called after instantiation to initialise the Go runtime (allocator,
/// goroutine scheduler, global vars) and to invoke `main()` (which calls
/// `wafer.Register`). Without it every WAFER export traps with `unreachable`.
///
/// `_start` terminates by calling `proc_exit(0)` — that traps with our
/// [`ProcExitTrap`] marker. We downcast the wasmi error to that typed marker:
/// `proc_exit(0)` is the expected WASI shutdown; a non-zero (or unrelated) trap
/// is surfaced as an error. Rust-compiled blocks have no `_start` export and
/// are unaffected.
pub(super) fn instantiate(
    engine: &Engine,
    linker: &Linker<WasmiHostState>,
    module: &Module,
    caps: &BlockCapabilities,
    limits: ResourceLimits,
) -> Result<(Store<WasmiHostState>, wasmi::Instance), RuntimeError> {
    let host_state = WasmiHostState {
        context: None,
        max_memory_pages: limits.memory_pages,
        max_table_elements: limits.max_table_elements,
        capabilities: caps.clone(),
        inbound_protected_meta: Vec::new(),
        streams: StreamRegistry::with_limits(limits.max_host_bytes, limits.max_live_streams),
        pending_stream_finish: None,
        pending_stream_read: None,
        pending_stream_take_error: None,
        pending_load_asset: None,
        current_attachments: None,
        host_codec: HostCodec::Rmp,
    };
    let mut store = Store::new(engine, host_state);

    // Resource limits — the `WasmiHostState` is the store's `ResourceLimiter`,
    // and its `max_memory_pages` field bounds `memory.grow`.
    store.limiter(|state| state);
    // Fuel metering — `Metered(n)` sets the per-call budget; `Unmetered` skips
    // `set_fuel` entirely (wasmi rejects it when `consume_fuel(false)`).
    apply_fuel(&mut store, limits.fuel, "setting fuel")?;

    let pre = linker
        .instantiate(&mut store, module)
        .map_err(|e| RuntimeError::Wasm(format!("instantiation: {e}")))?;
    let instance = pre
        .start(&mut store)
        .map_err(|e| RuntimeError::Wasm(format!("running start function: {e}")))?;

    // Call `_start` if exported — required for TinyGo WASM modules.
    if let Ok(start_fn) = instance.get_typed_func::<(), ()>(&store, "_start") {
        match start_fn.call(&mut store, ()) {
            Ok(()) => {}
            Err(e) => match e.downcast_ref::<ProcExitTrap>() {
                // proc_exit(0) is the normal WASI shutdown path — expected.
                Some(ProcExitTrap { code: 0 }) => {}
                // A non-zero exit, or any other trap, is a genuine startup failure.
                Some(ProcExitTrap { code }) => {
                    return Err(RuntimeError::Wasm(format!(
                        "WASM _start exited with non-zero code {code}"
                    )));
                }
                None => {
                    return Err(RuntimeError::Wasm(format!("WASM _start failed: {e}")));
                }
            },
        }
        // Re-fill fuel so the subsequent guest call has a full budget.
        apply_fuel(&mut store, limits.fuel, "refilling fuel after _start")?;
    }

    let codec = negotiate_host_codec(&mut store, instance)?;
    store.data_mut().host_codec = codec;
    // `negotiate_host_codec` CALLS a guest export, so it spends fuel from the
    // budget set above. Refill afterwards for the same reason the `_start`
    // branch does: the first real guest call must start from a full budget,
    // not from whatever the negotiation left behind.
    apply_fuel(
        &mut store,
        limits.fuel,
        "refilling fuel after codec negotiation",
    )?;

    Ok((store, instance))
}

/// RAII guard that installs an owned [`Context`] into the wasmi store's
/// `context` slot for the duration of a single guest invocation and clears it
/// on drop — on *every* exit path (`?`, early `return Err`, the unhandled-trap
/// branch, or success).
///
/// The slot holds an owned `Arc<dyn Context>` minted via
/// [`Context::clone_arc`], so host imports may hold it across await points
/// with no lifetime hazard (this replaced the old `ContextGuard`, which
/// `transmute`d a borrowed `&dyn Context` to `'static` and policed the lie
/// with a strong-count assertion at drop). Clearing the slot on drop is now
/// hygiene — a stale context must not leak into the next invocation — not a
/// use-after-free guard.
pub(super) struct ContextScope<'s> {
    store: &'s mut Store<WasmiHostState>,
}

impl<'s> ContextScope<'s> {
    /// Install `ctx` into the store's `context` slot and seed the per-call
    /// `current_attachments`. The slot is cleared when the returned scope is
    /// dropped.
    pub(super) fn new(
        store: &'s mut Store<WasmiHostState>,
        ctx: &dyn Context,
        attachments: Option<std::collections::BTreeMap<String, wafer_block::Attachment>>,
        inbound_protected: Vec<MetaEntry>,
    ) -> Self {
        store.data_mut().context = Some(ctx.clone_arc());
        store.data_mut().current_attachments = attachments;
        // SEC-01: seed the host-owned identity for this frame so nested
        // `call_block`s the guest makes inherit it and cannot forge their own.
        store.data_mut().inbound_protected_meta = inbound_protected;
        Self { store }
    }

    /// Shared access to the underlying store.
    pub(super) fn store(&self) -> &Store<WasmiHostState> {
        self.store
    }

    /// Mutable access to the underlying store.
    pub(super) fn store_mut(&mut self) -> &mut Store<WasmiHostState> {
        self.store
    }
}

impl Drop for ContextScope<'_> {
    fn drop(&mut self) {
        // Clear the slot so a stale context never leaks into a later
        // invocation that forgets to install its own.
        self.store.data_mut().context = None;
    }
}
