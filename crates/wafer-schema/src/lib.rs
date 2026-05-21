//! Schema record types describing database tables, columns, indexes, and
//! defaults. Vocab-only crate — no dependencies beyond `std`. Consumed by
//! `wafer-sql-utils` (builders), `wafer-core` (interface contracts), and
//! `wafer-run` (re-exported for convenience).

#![warn(missing_docs)]

/// Schema record types (`Table`, `Column`, `Index`, …) used to describe a database.
pub mod types;

pub use types::*;
