//! Shared types, traits, and macros for WAFER block authors.
//!
//! This crate provides the core types used by both the WAFER runtime (`wafer-run`)
//! and the WASM guest SDK (`wafer-sdk`). Block authors depend on this crate for
//! type definitions, and optionally use the `#[wafer_block]` proc macro for
//! reduced boilerplate.

pub mod error_codes;
pub mod helpers;
pub mod meta;
pub mod types;

// Re-export everything at the crate root for convenience.
pub use error_codes::ErrorCode;
pub use helpers::*;
pub use meta::*;
pub use types::*;

// Re-export the proc macro.
pub use wafer_block_macro::wafer_block;

// Re-export serde_json for use in the register_block! macro.
#[doc(hidden)]
pub use serde_json as _serde_json;

/// The trait that WASM block authors implement.
///
/// This is the guest-side interface. Native blocks implement the `Block` trait
/// from `wafer-run` instead.
pub trait WaferBlock {
    fn info() -> BlockInfo;
    fn handle(msg: Message) -> BlockResult;
    fn lifecycle(event: LifecycleEvent) -> Result<(), WaferError>;
}

/// Register a type as the WASM block implementation.
///
/// This macro generates the thin ABI export functions that the host runtime
/// calls: `__wafer_info`, `__wafer_handle`, and `__wafer_lifecycle`.
///
/// # Example
///
/// ```rust,ignore
/// struct MyBlock;
/// impl wafer_block::WaferBlock for MyBlock { /* ... */ }
/// wafer_block::register_block!(MyBlock);
/// ```
#[macro_export]
macro_rules! register_block {
    ($block_ty:ty) => {
        #[no_mangle]
        pub extern "C" fn __wafer_info() -> i64 {
            let info = <$block_ty as $crate::WaferBlock>::info();
            let json = $crate::_serde_json::to_vec(&info).unwrap_or_default();
            let ptr = json.as_ptr() as i64;
            let len = json.len() as i64;
            std::mem::forget(json);
            (ptr << 32) | len
        }

        #[no_mangle]
        pub extern "C" fn __wafer_handle(ptr: i32, len: i32) -> i64 {
            let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
            let msg: $crate::Message = match $crate::_serde_json::from_slice(bytes) {
                Ok(m) => m,
                Err(e) => {
                    let err = $crate::BlockResult {
                        action: $crate::Action::Error,
                        response: None,
                        error: Some($crate::WaferError {
                            code: "internal".into(),
                            message: format!("deserializing message: {e}"),
                            meta: Default::default(),
                        }),
                        message: None,
                    };
                    let json = $crate::_serde_json::to_vec(&err).unwrap_or_default();
                    let ptr = json.as_ptr() as i64;
                    let len = json.len() as i64;
                    std::mem::forget(json);
                    return (ptr << 32) | len;
                }
            };

            let result = <$block_ty as $crate::WaferBlock>::handle(msg);
            let json = $crate::_serde_json::to_vec(&result).unwrap_or_default();
            let ptr = json.as_ptr() as i64;
            let len = json.len() as i64;
            std::mem::forget(json);
            (ptr << 32) | len
        }

        #[no_mangle]
        pub extern "C" fn __wafer_lifecycle(ptr: i32, len: i32) -> i64 {
            let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
            let event: $crate::LifecycleEvent = match $crate::_serde_json::from_slice(bytes) {
                Ok(e) => e,
                Err(e) => {
                    let err = $crate::WaferError {
                        code: "internal".into(),
                        message: format!("deserializing lifecycle event: {e}"),
                        meta: Default::default(),
                    };
                    let json = $crate::_serde_json::to_vec(&err).unwrap_or_default();
                    let ptr = json.as_ptr() as i64;
                    let len = json.len() as i64;
                    std::mem::forget(json);
                    return (ptr << 32) | len;
                }
            };

            match <$block_ty as $crate::WaferBlock>::lifecycle(event) {
                Ok(()) => 0i64,
                Err(err) => {
                    let json = $crate::_serde_json::to_vec(&err).unwrap_or_default();
                    let ptr = json.as_ptr() as i64;
                    let len = json.len() as i64;
                    std::mem::forget(json);
                    (ptr << 32) | len
                }
            }
        }
    };
}
