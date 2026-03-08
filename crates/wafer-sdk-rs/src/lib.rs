//! WAFER guest SDK for writing blocks compiled to WebAssembly.
//!
//! This crate provides the types, helper functions, and service clients
//! needed to implement a WAFER block. The block communicates with the WAFER
//! host runtime through a thin ABI — all data is serialized as JSON bytes
//! passed through WASM linear memory.
//!
//! # Quick start
//!
//! ```rust,ignore
//! use wafer_sdk::*;
//!
//! struct MyBlock;
//!
//! impl WaferBlock for MyBlock {
//!     fn info() -> BlockInfo {
//!         BlockInfo {
//!             name: "my-block".into(),
//!             version: "0.1.0".into(),
//!             interface: "transform".into(),
//!             summary: "A demo block".into(),
//!             instance_mode: InstanceMode::PerNode,
//!             allowed_modes: vec![],
//!             admin_ui: None,
//!             runtime: BlockRuntime::Wasm,
//!             requires: vec![],
//!         }
//!     }
//!
//!     fn handle(msg: Message) -> BlockResult {
//!         msg.cont()
//!     }
//!
//!     fn lifecycle(_event: LifecycleEvent) -> Result<(), WaferError> {
//!         Ok(())
//!     }
//! }
//!
//! wafer_sdk::register_block!(MyBlock);
//! ```

pub mod helpers;
pub mod types;

// Re-export the most commonly used types at the crate root.
pub use types::*;
pub use helpers::*;

/// The trait that block authors implement.
pub trait WaferBlock {
    fn info() -> BlockInfo;
    fn handle(msg: Message) -> BlockResult;
    fn lifecycle(event: LifecycleEvent) -> Result<(), WaferError>;
}

// ---------------------------------------------------------------------------
// Host imports (thin ABI)
// ---------------------------------------------------------------------------

extern "C" {
    #[link_name = "is_cancelled"]
    fn host_is_cancelled() -> i32;

    #[link_name = "log"]
    fn host_log(level_ptr: i32, level_len: i32, msg_ptr: i32, msg_len: i32);

    #[link_name = "call_block"]
    fn host_call_block(
        name_ptr: i32,
        name_len: i32,
        msg_ptr: i32,
        msg_len: i32,
    ) -> i64;
}

/// Check if the current execution has been cancelled.
pub fn is_cancelled() -> bool {
    unsafe { host_is_cancelled() != 0 }
}

/// Log a message at the given level.
pub fn log(level: &str, msg: &str) {
    unsafe {
        host_log(
            level.as_ptr() as i32,
            level.len() as i32,
            msg.as_ptr() as i32,
            msg.len() as i32,
        );
    }
}

/// Call another block by name with a message, returning the result.
pub fn call_block(block_name: &str, msg: &Message) -> BlockResult {
    let msg_json = match serde_json::to_vec(msg) {
        Ok(j) => j,
        Err(e) => {
            return BlockResult {
                action: Action::Error,
                response: None,
                error: Some(WaferError {
                    code: "internal".into(),
                    message: format!("serializing message: {e}"),
                    meta: Default::default(),
                }),
                message: None,
            };
        }
    };

    let packed = unsafe {
        host_call_block(
            block_name.as_ptr() as i32,
            block_name.len() as i32,
            msg_json.as_ptr() as i32,
            msg_json.len() as i32,
        )
    };

    if packed == 0 {
        return BlockResult {
            action: Action::Error,
            response: None,
            error: Some(WaferError {
                code: "internal".into(),
                message: "call_block returned null".into(),
                meta: Default::default(),
            }),
            message: None,
        };
    }

    let ptr = (packed >> 32) as u32;
    let len = (packed & 0xFFFF_FFFF) as u32;

    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    match serde_json::from_slice(bytes) {
        Ok(r) => r,
        Err(e) => BlockResult {
            action: Action::Error,
            response: None,
            error: Some(WaferError {
                code: "internal".into(),
                message: format!("parsing call_block result: {e}"),
                meta: Default::default(),
            }),
            message: None,
        },
    }
}

// ---------------------------------------------------------------------------
// Guest allocator export
// ---------------------------------------------------------------------------

/// Guest allocator — the host calls this to allocate memory for writing data
/// into the guest's linear memory.
#[no_mangle]
pub extern "C" fn __wafer_alloc(len: i32) -> i32 {
    let layout = std::alloc::Layout::from_size_align(len as usize, 1).unwrap();
    let ptr = unsafe { std::alloc::alloc(layout) };
    ptr as i32
}

// ---------------------------------------------------------------------------
// Macro to register a block
// ---------------------------------------------------------------------------

/// Register a type as the block implementation.
///
/// This macro generates the thin ABI export functions that the host runtime
/// calls: `__wafer_info`, `__wafer_handle`, and `__wafer_lifecycle`.
///
/// # Example
///
/// ```rust,ignore
/// struct MyBlock;
/// impl wafer_sdk::WaferBlock for MyBlock { /* ... */ }
/// wafer_sdk::register_block!(MyBlock);
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

// Re-export serde_json for use in the macro
#[doc(hidden)]
pub use serde_json as _serde_json;
