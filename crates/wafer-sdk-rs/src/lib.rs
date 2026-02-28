//! WAFER guest SDK for writing blocks compiled to WebAssembly components.
//!
//! This crate provides the types, helper functions, and service clients
//! needed to implement a WAFER block. The block communicates with the WAFER
//! host runtime through the WebAssembly Component Model — all imports and
//! exports are generated from WIT definitions by `wit-bindgen`.
//!
//! # Quick start
//!
//! ```rust,ignore
//! use wafer_sdk::*;
//!
//! struct MyBlock;
//!
//! impl Guest for MyBlock {
//!     fn info() -> BlockInfo {
//!         BlockInfo {
//!             name: "my-block".into(),
//!             version: "0.1.0".into(),
//!             interface: "transform".into(),
//!             summary: "A demo block".into(),
//!             instance_mode: InstanceMode::PerNode,
//!             allowed_modes: vec![],
//!         }
//!     }
//!
//!     fn handle(msg: Message) -> BlockResult {
//!         // Process the message...
//!         msg.cont()
//!     }
//!
//!     fn lifecycle(event: LifecycleEvent) -> Result<(), WaferError> {
//!         Ok(())
//!     }
//! }
//!
//! wafer_sdk::register_block!(MyBlock);
//! ```

pub mod helpers;
pub mod services;
pub mod types;

// Generate WIT bindings for guest-side code.
wit_bindgen::generate!({
    path: "../../wit/wit",
    world: "wafer-block",
});

// Re-export the guest trait that block authors implement.
pub use exports::wafer::block_world::block::Guest;

// Re-export the most commonly used types at the crate root.
pub use types::*;
pub use helpers::*;

/// Register a type as the block implementation.
///
/// This macro connects your `Guest` implementation to the generated
/// WASM component exports.
///
/// # Example
///
/// ```rust,ignore
/// struct MyBlock;
/// impl wafer_sdk::Guest for MyBlock { /* ... */ }
/// wafer_sdk::register_block!(MyBlock);
/// ```
#[macro_export]
macro_rules! register_block {
    ($block_ty:ty) => {
        $crate::export!($block_ty with_types_in $crate);
    };
}
