//! Test fixtures and helpers for wafer-run block tests.
//!
//! This crate is only a dev-dependency of production crates. It exposes
//! `FakeDb` and `FakeCrypto` (real `Block` implementations backed by
//! in-memory state) and a `WaferBuilder` helper that assembles a running
//! `Wafer` runtime with common test wiring.

pub mod builder;
pub mod fake_crypto;
pub mod fake_db;
