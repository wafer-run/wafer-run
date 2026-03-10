mod block;
pub mod factory;
pub mod service;
#[cfg(feature = "storage-local")]
pub mod local;
#[cfg(feature = "storage-local")]
pub mod local_factory;
#[cfg(feature = "storage-s3")]
pub mod s3;
#[cfg(feature = "storage-s3")]
pub mod s3_factory;

pub use block::StorageBlock;
