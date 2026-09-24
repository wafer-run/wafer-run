//! Test fixtures and helpers for wafer-run block tests.
//!
//! This crate is only a dev-dependency of production crates. It exposes a
//! `WaferBuilder` helper that assembles a running `Wafer` runtime with
//! common test wiring. A test that needs a database or crypto service
//! registers the real one (`wafer_core::service_blocks`, e.g. over
//! `wafer-block-sqlite`'s in-memory database), so it exercises the
//! production wire codec and handler.

#![warn(missing_docs)]

pub mod builder;
