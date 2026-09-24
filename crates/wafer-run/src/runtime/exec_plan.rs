//! Seal-time compiled dispatch data (PERF-03).
//!
//! [`SealedPlan`] is built once at the end of [`Wafer::seal`] and holds
//! everything the hot dispatch paths used to recompute per call:
//!
//! - per-block flattened config maps (`parse_config_map` of the startup
//!   snapshot's `block_configs`, previously re-parsed on every `run_block`),
//! - per-block declared `requires` allowlists and interfaces, plus the
//!   interface specs keyed by name — what `call_block` needs about a callee
//!   ([`DispatchTable`], shared with every context), so a call neither
//!   builds the callee's `BlockInfo` nor scans a spec list,
//! - compiled flows (see [`crate::waferflow::plan`]).
//!
//! Lookups fall back to the pre-plan code paths for anything not present —
//! blocks registered or flows added after `seal()` — so behavior is
//! unchanged; only the recomputation is gone.

use std::{collections::HashMap, sync::Arc};

use wafer_block::{config::parse_config_map, InterfaceSpec};

use super::Wafer;
use crate::waferflow::plan::{compile_flow, CompiledFlow};

/// What dispatch needs about one registered block, read from its
/// `BlockInfo` once at `seal()`.
pub(crate) struct BlockDispatch {
    /// The block's declared interface (`BlockInfo::interface`), which
    /// `call_block` checks the requested action against.
    pub(crate) interface: String,
    /// The block's [`call_allowlist`](wafer_block::BlockInfo::call_allowlist)
    /// (`requires` plus `optional_requires`); `None` when it declared
    /// neither (unrestricted `call_block`).
    pub(crate) requires: Option<Arc<Vec<String>>>,
}

/// The per-call facts about callees, compiled at `seal()` and `Arc`-shared
/// with every [`RuntimeContext`](crate::context::RuntimeContext).
#[derive(Default)]
pub(crate) struct DispatchTable {
    /// Keyed by registration name.
    pub(crate) blocks: HashMap<String, BlockDispatch>,
    /// Every registered interface spec, keyed by interface name.
    pub(crate) interface_specs: HashMap<String, InterfaceSpec>,
}

/// Immutable dispatch data compiled once at `seal()`.
pub(crate) struct SealedPlan {
    /// Parsed block configs, keyed exactly as
    /// [`StartupSnapshot::block_configs`](crate::snapshot::StartupSnapshot::block_configs)
    /// is: by the registered block's name, since `seal()` moves config
    /// registered under an alias to the alias's target.
    pub(crate) block_configs: HashMap<String, Arc<HashMap<String, String>>>,
    /// What dispatch needs about each registered block.
    pub(crate) dispatch: Arc<DispatchTable>,
    /// Compiled flows keyed by flow id.
    pub(crate) flows: HashMap<String, Arc<CompiledFlow>>,
    /// Shared empty config map handed to contexts with no per-call config
    /// (init/lifecycle contexts, blocks without registered config).
    pub(crate) empty_config: Arc<HashMap<String, String>>,
}

impl SealedPlan {
    /// The pre-seal placeholder: everything falls back to uncompiled paths.
    pub(crate) fn empty() -> Self {
        Self {
            block_configs: HashMap::new(),
            dispatch: Arc::default(),
            flows: HashMap::new(),
            empty_config: Arc::new(HashMap::new()),
        }
    }

    /// Parsed config for the block registered as `resolved` (an alias
    /// resolved to its target). Misses share one empty map.
    pub(crate) fn config_for(&self, resolved: &str) -> Arc<HashMap<String, String>> {
        self.block_configs
            .get(resolved)
            .cloned()
            .unwrap_or_else(|| self.empty_config.clone())
    }
}

impl Wafer {
    /// Build the [`SealedPlan`] from the just-finalized startup snapshot and
    /// the registered flows. Called by `finalize_snapshot` after
    /// `rebuild_all_blocks`, so flow compilation sees the complete registry
    /// (including blocks downloaded during remote resolution).
    pub(crate) fn compile_plan(&self) -> SealedPlan {
        SealedPlan {
            block_configs: self
                .snapshot
                .block_configs
                .iter()
                .map(|(name, cfg)| (name.clone(), Arc::new(parse_config_map(cfg))))
                .collect(),
            dispatch: Arc::new(DispatchTable {
                blocks: self
                    .registration
                    .blocks
                    .iter()
                    .map(|(name, block)| {
                        let info = block.info();
                        let dispatch = BlockDispatch {
                            requires: info.call_allowlist().map(Arc::new),
                            interface: info.interface,
                        };
                        (name.clone(), dispatch)
                    })
                    .collect(),
                interface_specs: self.registration.interface_specs.clone(),
            }),
            flows: self
                .flows
                .values()
                .map(|flow| (flow.id.clone(), Arc::new(compile_flow(self, flow))))
                .collect(),
            empty_config: Arc::new(HashMap::new()),
        }
    }
}
