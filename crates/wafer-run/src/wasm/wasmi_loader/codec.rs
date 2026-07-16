//! Core-ABI codec negotiation (PERF-01).
//!
//! The core (non-streaming) guest ABI is versioned per module via the
//! `__wafer_abi_version` export (see `wafer_block::abi`). This module decides
//! which wire codec a module speaks from its static export list and pins the
//! exact version at instantiation time.

use wafer_block::error::RuntimeError;
use wasmi::{Module, Store};

use super::abi::WasmiHostState;

/// Wire codec for the core (non-streaming) guest ABI, negotiated per module
/// via the `__wafer_abi_version` export (see `wafer_block::abi`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum AbiCodec {
    /// Legacy JSON frames — guests built before the version export existed.
    /// Supported indefinitely so already-shipped wasm artifacts keep working.
    V1Json,
    /// MessagePack frames with bin-encoded bodies (ABI v2).
    V2Rmp,
}

/// Decide the codec from the module's static export list — no instantiation
/// needed. A guest exporting [`wafer_block::abi::ABI_VERSION_EXPORT`] speaks
/// the versioned (v2+) ABI; [`verify_abi_version`] then pins the exact value
/// at instantiation time so a future v3 guest fails loud instead of being
/// mis-decoded as v2.
pub(super) fn abi_codec_of(module: &Module) -> AbiCodec {
    let versioned = module
        .exports()
        .any(|e| e.name() == wafer_block::abi::ABI_VERSION_EXPORT && e.ty().func().is_some());
    if versioned {
        AbiCodec::V2Rmp
    } else {
        AbiCodec::V1Json
    }
}

/// Call the guest's `__wafer_abi_version` and reject any version this host
/// does not speak. Only meaningful for [`AbiCodec::V2Rmp`] modules.
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
    if version != wafer_block::abi::ABI_VERSION {
        return Err(RuntimeError::Wasm(format!(
            "guest declares core ABI v{version}; this host supports v1 (implicit) and v{}",
            wafer_block::abi::ABI_VERSION
        )));
    }
    Ok(())
}
