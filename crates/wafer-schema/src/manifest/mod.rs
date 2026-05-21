//! Manifest record types serialised in block manifests and helpers that
//! convert them to runtime [`crate::Table`] definitions.
//!
//! Vocab-only: no runtime, no async, no `Wafer` coupling. Consumed by
//! `wafer-block-postgres` and (eventually) `wafer-block-sqlite` during
//! their `lifecycle(Init)` to materialise tables from JSON config.

/// Convert [`CollectionDef`] manifest entries into runtime schema [`crate::Table`] types.
pub mod to_schema;
/// Manifest record types (`CollectionDef`, `FieldDef`, `IndexDef`) serialised in block manifests.
pub mod types;

pub use to_schema::*;
pub use types::*;
