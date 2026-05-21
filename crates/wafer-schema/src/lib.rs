//! Schema record types describing database tables, columns, indexes, and
//! defaults. Vocab-only crate — no runtime, no async, no `Wafer` coupling.
//! Direct deps: `serde` (derive) + `serde_json` (manifest default values).
//! Consumed by `wafer-sql-utils` (builders), `wafer-core` (interface
//! contracts), `wafer-block-postgres` (manifest types), and `wafer-run`
//! (re-exported for convenience).

#![warn(missing_docs)]

/// Schema record types (`Table`, `Column`, `Index`, …) used to describe a database.
pub mod types;

/// Manifest record types and helpers (`CollectionDef`, `collections_to_tables`).
pub mod manifest;

pub use types::*;
