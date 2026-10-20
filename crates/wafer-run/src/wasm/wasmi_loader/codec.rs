//! Wasmi glue for codec negotiation (PERF-01).
//!
//! The core (non-streaming) guest ABI is versioned per module via the
//! `__wafer_abi_version` export. The codec itself and the version rule live
//! in [`wafer_block::abi::CoreCodec`], shared with the `wafer` CLI; this
//! module only reads the module's exports and calls the guest's version and
//! host-codec exports on a wasmi instance.

use wafer_block::{abi::CoreCodec, error::RuntimeError};
use wasmi::{Module, Store};

use super::abi::WasmiHostState;

/// Decide the core codec from the module's static export list — no
/// instantiation needed. [`verify_abi_version`] then pins the exact value at
/// instantiation time so a future v3 guest fails loud instead of being
/// mis-decoded as v2.
pub(super) fn abi_codec_of(module: &Module) -> CoreCodec {
    CoreCodec::from_version_export(
        module
            .exports()
            .any(|e| e.name() == wafer_block::abi::ABI_VERSION_EXPORT && e.ty().func().is_some()),
    )
}

/// Call the guest's `__wafer_abi_version` and reject any version this host
/// does not speak. Only meaningful for [`CoreCodec::V2Rmp`] modules.
pub(super) fn verify_abi_version(
    store: &mut Store<WasmiHostState>,
    instance: wasmi::Instance,
) -> Result<(), RuntimeError> {
    let version_fn = instance
        .get_typed_func::<(), i32>(&*store, wafer_block::abi::ABI_VERSION_EXPORT)
        .map_err(|e| {
            RuntimeError::Wasm(format!(
                "getting {}: {e}",
                wafer_block::abi::ABI_VERSION_EXPORT
            ))
        })?;
    let version = version_fn.call(&mut *store, ()).map_err(|e| {
        RuntimeError::Wasm(format!(
            "calling {}: {e}",
            wafer_block::abi::ABI_VERSION_EXPORT
        ))
    })?;
    CoreCodec::check_declared(version).map_err(|e| RuntimeError::Wasm(e.to_string()))
}

/// Codec of host-call payloads, per instance. See `wafer_block::abi::HOST_CODEC_EXPORT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HostCodec {
    Rmp,
    Json,
}

/// Call the guest's `__wafer_host_codec` export if present.
pub(super) fn negotiate_host_codec(
    store: &mut Store<WasmiHostState>,
    instance: wasmi::Instance,
) -> Result<HostCodec, RuntimeError> {
    let Ok(f) = instance.get_typed_func::<(), i32>(&*store, wafer_block::abi::HOST_CODEC_EXPORT)
    else {
        return Ok(HostCodec::Rmp);
    };
    match f.call(&mut *store, ()) {
        Ok(v) if v == wafer_block::abi::HOST_CODEC_JSON => Ok(HostCodec::Json),
        Ok(v) if v == wafer_block::abi::HOST_CODEC_RMP => Ok(HostCodec::Rmp),
        Ok(v) => Err(RuntimeError::Wasm(format!(
            "__wafer_host_codec returned unsupported value {v}"
        ))),
        Err(e) => Err(RuntimeError::Wasm(format!(
            "calling __wafer_host_codec: {e}"
        ))),
    }
}
