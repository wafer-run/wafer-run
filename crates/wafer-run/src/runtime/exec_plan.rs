//! Seal-time compiled dispatch data (PERF-03).
//!
//! [`SealedPlan`] is built once at the end of [`Wafer::seal`] and holds
//! everything the hot dispatch paths used to recompute per call:
//!
//! - per-block flattened config maps (`parse_config_map` of the startup
//!   snapshot's `block_configs`, previously re-parsed on every `run_block`),
//! - per-block declared `requires` allowlists (previously a linear scan of
//!   `snapshot.blocks` plus a `Vec<String>` clone per dispatch),
//! - compiled flows (see [`crate::waferflow::plan`]).
//!
//! Lookups fall back to the pre-plan code paths for anything not present —
//! blocks registered or flows added after `seal()` — so behavior is
//! unchanged; only the recomputation is gone.

use std::{collections::HashMap, sync::Arc};

use wafer_block::config::parse_config_map;

use super::Wafer;
use crate::waferflow::plan::{compile_flow, CompiledFlow};

/// Immutable dispatch data compiled once at `seal()`.
pub(crate) struct SealedPlan {
    /// Parsed block configs, keyed exactly as
    /// [`StartupSnapshot::block_configs`](crate::snapshot::StartupSnapshot::block_configs)
    /// is (the registration name, which may be an alias or a target).
    pub(crate) block_configs: HashMap<String, Arc<HashMap<String, String>>>,
    /// Declared non-empty `requires` allowlist per registered block name;
    /// `None` when the block declared none (unrestricted `call_block`).
    pub(crate) block_requires: HashMap<String, Option<Arc<Vec<String>>>>,
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
            block_requires: HashMap::new(),
            flows: HashMap::new(),
            empty_config: Arc::new(HashMap::new()),
        }
    }

    /// Parsed config for a dispatch target: alias-resolved name first, then
    /// the raw name (the same two-key lookup `run_block` always performed —
    /// `add_block_config` is keyed by registration name, which may be either
    /// the alias or the target). Misses share one empty map.
    pub(crate) fn config_for(&self, resolved: &str, raw: &str) -> Arc<HashMap<String, String>> {
        self.block_configs
            .get(resolved)
            .or_else(|| self.block_configs.get(raw))
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
            block_requires: self
                .registration
                .blocks
                .keys()
                .map(|name| (name.clone(), self.resolve_block_requires_uncached(name)))
                .collect(),
            flows: self
                .flows
                .values()
                .map(|flow| (flow.id.clone(), Arc::new(compile_flow(self, flow))))
                .collect(),
            empty_config: Arc::new(HashMap::new()),
        }
    }
}
