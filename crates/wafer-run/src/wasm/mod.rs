pub mod capabilities;
#[cfg(feature = "wasm")]
pub mod host;
#[cfg(feature = "wasm")]
pub mod wasmi_loader;

#[cfg(feature = "wasm")]
pub use wasmi_loader::WasmiBlock;
