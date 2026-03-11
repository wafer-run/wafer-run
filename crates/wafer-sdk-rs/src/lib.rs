//! WAFER guest SDK for writing blocks compiled to WebAssembly.
//!
//! This crate provides the host import functions needed to implement a WASM
//! block. Types and traits come from `wafer-block` and are re-exported here.
//!
//! # Quick start
//!
//! ```rust,ignore
//! use wafer_sdk::*;
//!
//! struct MyBlock;
//!
//! #[wafer_block(
//!     name = "my-block",
//!     version = "0.1.0",
//!     interface = "transform",
//!     summary = "A demo block"
//! )]
//! impl MyBlock {
//!     fn handle(msg: Message) -> BlockResult {
//!         msg.cont()
//!     }
//! }
//! ```

// Re-export everything from wafer-block (types, traits, helpers, macros).
pub use wafer_block::*;

// Re-export serde_json for use in macros.
#[doc(hidden)]
pub use serde_json as _serde_json;

// ---------------------------------------------------------------------------
// Host imports (thin ABI)
// ---------------------------------------------------------------------------

#[link(wasm_import_module = "wafer")]
extern "C" {
    #[link_name = "is_cancelled"]
    fn host_is_cancelled() -> i32;

    #[link_name = "log"]
    fn host_log(level_ptr: i32, level_len: i32, msg_ptr: i32, msg_len: i32);

    #[link_name = "call_block"]
    fn host_call_block(name_ptr: i32, name_len: i32, msg_ptr: i32, msg_len: i32) -> i32;

    #[link_name = "read_result"]
    fn host_read_result(dest_ptr: i32, dest_len: i32) -> i32;
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

/// Call another block by name, returning the result.
///
/// Uses a two-phase protocol:
/// 1. `call_block` traps to the host which performs the async operation,
///    then resumes WASM execution returning the result byte length.
/// 2. `read_result` copies the serialized result into a guest-allocated buffer.
pub fn call_block(block_name: &str, msg: &Message) -> BlockResult {
    let msg_json = match serde_json::to_vec(msg) {
        Ok(j) => j,
        Err(e) => {
            return BlockResult {
                action: Action::Error,
                response: None,
                error: Some(WaferError::new("internal", format!("serializing message: {e}"))),
                message: None,
            };
        }
    };

    let result_len = unsafe {
        host_call_block(
            block_name.as_ptr() as i32,
            block_name.len() as i32,
            msg_json.as_ptr() as i32,
            msg_json.len() as i32,
        )
    };

    if result_len <= 0 {
        return BlockResult {
            action: Action::Error,
            response: None,
            error: Some(WaferError::new("internal", "call_block returned error")),
            message: None,
        };
    }

    let mut buf = vec![0u8; result_len as usize];
    unsafe { host_read_result(buf.as_mut_ptr() as i32, result_len) };

    match serde_json::from_slice(&buf) {
        Ok(r) => r,
        Err(e) => BlockResult {
            action: Action::Error,
            response: None,
            error: Some(WaferError::new("internal", format!("parsing call_block result: {e}"))),
            message: None,
        },
    }
}

// ---------------------------------------------------------------------------
// Guest allocator export
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn __wafer_alloc(len: i32) -> i32 {
    let layout = std::alloc::Layout::from_size_align(len as usize, 1).unwrap();
    let ptr = unsafe { std::alloc::alloc(layout) };
    ptr as i32
}
