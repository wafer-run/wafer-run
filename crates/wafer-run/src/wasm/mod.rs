pub mod capabilities;
#[cfg(feature = "wasmi")]
pub(crate) mod stream;
/// `wasmi`-backed WASM block loader (gated by the `wasmi` feature).
#[cfg(feature = "wasmi")]
pub mod wasmi_loader;

#[cfg(feature = "wasmi")]
pub use wasmi_loader::{WasmiBlock, WASM_POOLING_ENV};
