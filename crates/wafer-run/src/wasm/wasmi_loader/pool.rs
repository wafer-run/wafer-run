//! Warm instance pooling (PERF-01 Part B).
//!
//! Blocks that declare a state-retaining
//! [`InstanceMode`](wafer_block::InstanceMode) may reuse a warm store +
//! instance across `handle` calls. This module owns the pooling policy
//! constants, the kill-switch env parsing, and the [`PooledInstance`] carrier;
//! the checkout/checkin methods live on [`WasmiBlock`](super::WasmiBlock).

use wafer_block::error::RuntimeError;
use wasmi::Store;

use super::abi::WasmiHostState;

/// Env var for the host-level wasm instance-pooling kill switch.
///
/// - **absent** (or empty, matching the `WAFER_LOCKFILE` convention) —
///   pooling is enabled for blocks that declared a state-retaining
///   [`InstanceMode`](wafer_block::InstanceMode) (`Singleton` / `PerFlow`).
/// - **`on` / `off`** (ASCII case-insensitive) — explicitly enable/disable.
///   `off` is the isolation escape hatch for hosts running third-party wasm
///   that (wrongly) declared a state-retaining mode.
/// - **anything else** — a hard error at `WasmiBlock` load and at
///   [`Wafer::seal`](crate::Wafer::seal). A mistyped value must fail loud,
///   never silently fall back to either behavior.
pub const WASM_POOLING_ENV: &str = "WAFER_RUN_WASM_POOLING";

/// Recycle a pooled instance after this many served calls, bounding unbounded
/// guest-heap growth (the core ABI has no guest-side free for host-written
/// buffers, so every reused call leaks its request/response allocations into
/// the guest heap until the instance is recycled).
pub(super) const MAX_CALLS_PER_INSTANCE: u32 = 256;

/// Maximum number of idle instances retained per block. Beyond this, extra
/// instances are dropped on checkin rather than queued.
pub(super) const MAX_POOLED_INSTANCES: usize = 4;

/// A warm store + instance pair retained across `handle` calls for blocks
/// that opted into reuse via a state-retaining `InstanceMode`.
pub(super) struct PooledInstance {
    pub(super) store: Store<WasmiHostState>,
    pub(super) instance: wasmi::Instance,
    /// Calls this instance has completed; drives the
    /// [`MAX_CALLS_PER_INSTANCE`] recycle bound.
    pub(super) calls_served: u32,
}

/// Parse the raw [`WASM_POOLING_ENV`] value. `None`/empty means "not
/// configured" → pooling permitted for declared blocks.
///
/// Compiled out on `wasm32` targets (no process environment to parse —
/// see [`wasm_pooling_host_override`]).
#[cfg(not(target_arch = "wasm32"))]
fn parse_wasm_pooling(raw: Option<&str>) -> Result<bool, String> {
    let Some(raw) = raw else {
        return Ok(true);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        // Same convention as WAFER_LOCKFILE: an empty env var is "unset".
        return Ok(true);
    }
    if trimmed.eq_ignore_ascii_case("on") {
        return Ok(true);
    }
    if trimmed.eq_ignore_ascii_case("off") {
        return Ok(false);
    }
    Err(format!(
        "invalid {WASM_POOLING_ENV} value {raw:?}: expected \"on\" or \"off\" \
         (absent/empty defaults to \"on\")"
    ))
}

/// Read + validate the host-level pooling kill switch from the process
/// environment. Called at every `WasmiBlock` load and at `Wafer::seal` so an
/// invalid value fails loud at startup on both the direct-embedder path
/// (gizza-style `load_from_bytes`) and the runtime boot path.
///
/// On `wasm32` targets there is no process environment (same reasoning as
/// the `WAFER_LOCKFILE` auto-discovery skip in `builder.rs`), so the switch
/// is always "enabled for declared blocks".
pub(crate) fn wasm_pooling_host_override() -> Result<bool, RuntimeError> {
    #[cfg(target_arch = "wasm32")]
    {
        Ok(true)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let raw = std::env::var(WASM_POOLING_ENV).ok();
        parse_wasm_pooling(raw.as_deref()).map_err(RuntimeError::Config)
    }
}

// ---------------------------------------------------------------------------
// Unit tests for the pooling kill-switch parser
// ---------------------------------------------------------------------------

#[cfg(all(test, not(target_arch = "wasm32")))]
mod pooling_env_tests {
    use super::*;

    /// Config rule: absent (or empty, per the WAFER_LOCKFILE convention)
    /// means "pooling enabled for declared blocks".
    #[test]
    fn absent_or_empty_defaults_to_enabled() {
        assert_eq!(parse_wasm_pooling(None), Ok(true));
        assert_eq!(parse_wasm_pooling(Some("")), Ok(true));
        assert_eq!(parse_wasm_pooling(Some("   ")), Ok(true));
    }

    /// Explicit on/off values are honored, ASCII case-insensitively.
    #[test]
    fn explicit_on_off_values() {
        assert_eq!(parse_wasm_pooling(Some("on")), Ok(true));
        assert_eq!(parse_wasm_pooling(Some("ON")), Ok(true));
        assert_eq!(parse_wasm_pooling(Some("off")), Ok(false));
        assert_eq!(parse_wasm_pooling(Some("Off")), Ok(false));
        assert_eq!(parse_wasm_pooling(Some(" off ")), Ok(false));
    }

    /// Config rule: present-but-invalid must fail loud, never silently fall
    /// back to either behavior. The error names the env var so an operator
    /// can find the mistyped setting.
    #[test]
    fn invalid_values_fail_loud_naming_the_var() {
        for bad in ["0", "1", "true", "false", "cold", "sometimes"] {
            let err = parse_wasm_pooling(Some(bad))
                .expect_err("invalid value must be rejected, not defaulted");
            assert!(
                err.contains(WASM_POOLING_ENV),
                "error must name the env var: {err}"
            );
            assert!(err.contains(bad), "error must echo the bad value: {err}");
        }
    }
}
