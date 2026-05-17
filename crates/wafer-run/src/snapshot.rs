//! Startup snapshot — a single immutable bundle of runtime metadata
//! captured at `lifecycle::start` and shared with every [`RuntimeContext`]
//! created thereafter.
//!
//! Cloning a context Arc-clones this one bundle rather than five separate
//! snapshot `Arc<…>` fields. Adding a new piece of post-startup metadata
//! is a one-place change here instead of touching `Wafer`, the lifecycle
//! assignment, `make_context`, `RuntimeContext`, `clone_arc`, and the
//! trait accessor.

use std::{collections::HashMap, sync::Arc};

use wafer_block::InterfaceSpec;

use crate::block::BlockInfo;

/// Immutable bundle of post-startup metadata. Populated at the end of
/// [`crate::runtime::Wafer::seal`].
#[derive(Default, Clone)]
pub struct StartupSnapshot {
    /// Public metadata for every registered block, in registration order.
    pub blocks: Vec<BlockInfo>,
    /// Metadata for every loaded flow (id, description, declared interfaces).
    pub flow_infos: Vec<wafer_flow::FlowInfo>,
    /// Parsed flow definitions, indexable by `FlowInfo.id`.
    pub flow_defs: Vec<wafer_flow::WaferFlow>,
    /// Expanded per-block configs (after registrar-driven expansion).
    pub block_configs: HashMap<String, serde_json::Value>,
    /// Interface contracts declared by registered blocks, deduplicated.
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
