pub mod capabilities;
#[cfg(feature = "wasmi")]
pub mod host;
#[cfg(feature = "wasmi")]
pub(crate) mod stream;
#[cfg(feature = "wasmi")]
pub mod wasmi_loader;

#[cfg(feature = "wasmi")]
pub use wasmi_loader::WasmiBlock;
