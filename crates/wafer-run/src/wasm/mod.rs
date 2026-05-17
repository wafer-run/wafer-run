pub mod capabilities;
/// Host-side wasmi imports satisfied by the runtime (logging, fetch, KV, attachments).
#[cfg(feature = "wasmi")]
pub mod host;
#[cfg(feature = "wasmi")]
pub(crate) mod stream;
/// `wasmi`-backed WASM block loader (gated by the `wasmi` feature).
#[cfg(feature = "wasmi")]
pub mod wasmi_loader;

#[cfg(feature = "wasmi")]
pub use wasmi_loader::WasmiBlock;
