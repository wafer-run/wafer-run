//! wafer-core — Shared reusable WAFER blocks and flow templates.
//!
//! This crate provides all WAFER blocks and the client API layer.
//! wafer-run is a pure runtime engine; wafer-core owns the block library.
//! Consumers register blocks explicitly — there is no `register_all`.

pub mod blocks;
pub mod clients;
pub mod flows;
pub mod interfaces;
