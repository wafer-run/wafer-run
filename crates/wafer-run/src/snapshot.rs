//! Startup snapshot — a single immutable bundle of runtime metadata
//! captured at `lifecycle::start` and shared with every [`RuntimeContext`]
//! created thereafter.
//!
//! **Status: not yet integrated.** This module defines the target struct
//! shape. Wiring it through `Wafer` and `RuntimeContext` (replacing the
//! five separate snapshot `Arc<…>` fields each holds) is a substantial
//! refactor — ~30 internal call sites in `wafer-run` plus the
//! `Context::registered_blocks` / `::flow_defs` / etc. trait method
//! bodies. Tracked as a follow-up to the 2026-05-14 wafer-run refactor
//! spec (Pass 5).
//!
//! When integrated, cloning a context will drop from five `Arc::clone`s
//! to one, and any future snapshot field will only need to be added in
//! one place instead of the current six (Wafer's struct, Wafer's
//! lifecycle assignment, Wafer's make_context call, RuntimeContext's
//! struct, RuntimeContext::clone_arc, and the trait accessor).

use std::{collections::HashMap, sync::Arc};

use wafer_block::InterfaceSpec;

use crate::block::BlockInfo;

/// Immutable bundle of post-startup metadata. Construct via
/// [`StartupSnapshot::build`] (called from `lifecycle::start`).
#[derive(Default)]
pub struct StartupSnapshot {
    pub blocks: Vec<BlockInfo>,
    pub flow_infos: Vec<wafer_flow::FlowInfo>,
    pub flow_defs: Vec<wafer_flow::WaferFlow>,
    /// Expanded per-block configs (after registrar-driven expansion).
    pub block_configs: HashMap<String, serde_json::Value>,
    pub interface_specs: Vec<InterfaceSpec>,
}

impl StartupSnapshot {
    /// Allocate an empty snapshot. Used to seed the field before
    /// `lifecycle::start` runs; the empty value is never observed by
    /// blocks because contexts are only created after startup.
    pub fn empty() -> Arc<Self> {
        Arc::new(Self::default())
    }
}
