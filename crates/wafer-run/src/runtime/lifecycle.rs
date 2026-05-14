use std::{
    collections::HashMap,
    sync::{atomic::AtomicBool, Arc},
};

use super::Wafer;
use crate::{block::BlockInfo, error::RuntimeError, types::*};

/// Collect `BlockInfo`s into a Vec sorted by their stable `name`, so consumers
/// (admin pages, snapshot consumers) see deterministic order regardless of the
/// underlying HashMap's SipHash randomisation.
pub(crate) fn sorted_snapshot(iter: impl IntoIterator<Item = BlockInfo>) -> Vec<BlockInfo> {
    let mut v: Vec<_> = iter.into_iter().collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

impl Wafer {
    /// Initialize the runtime without calling `bind()` on blocks.
    pub async fn start_without_bind(&mut self) -> Result<(), RuntimeError> {
        self.resolve().await?;

        // Rebuild the all_blocks map so contexts can see all resolved blocks
        self.rebuild_all_blocks();

        // Snapshot introspection data for contexts.
        // Sort by stable block name so consumers see deterministic order
        // independent of HashMap's SipHash randomisation.
        self.blocks_snapshot = Arc::new(sorted_snapshot(self.blocks.values().map(|b| b.info())));
        self.flow_infos_snapshot = Arc::new(self.flows_info());
        self.flow_defs_snapshot = Arc::new(self.flow_defs());
        self.interface_specs_snapshot = Arc::new(self.interface_specs.values().cloned().collect());

        Ok(())
    }

    /// Collect and validate WRAP grants from all registered blocks.
    /// Called during resolve() after blocks are registered, so that Init
    /// lifecycle events can access cross-block resources via grants.
    ///
    /// Preserves any grants previously added via `add_wrap_grants()` (e.g.
    /// grants loaded from a database before `start()`).
    pub(crate) fn collect_wrap_grants(&mut self) {
        // Preserve DB-loaded grants added before start()
        let mut all_grants = (*self.wrap_grants).clone();
        let admin_block: &str = &self.wrap_admin_block;
        for block in self.blocks.values() {
            let info = block.info();
            for grant in &info.grants {
                // Network/Storage/Crypto typed grants use URLs / file-paths /
                // operation-names, not namespaced resources — skip ownership
                // validation, but require the declaring block to be the admin
                // block. Without this, any block could grant `*` Network /
                // Storage / Crypto access to all blocks and bypass default-deny
                // on those resource types.
                if matches!(
                    grant.resource_type,
                    Some(wafer_block::types::ResourceType::Network)
                        | Some(wafer_block::types::ResourceType::Storage)
                        | Some(wafer_block::types::ResourceType::Crypto)
                ) {
                    if info.name == admin_block {
                        all_grants.push(grant.clone());
                    } else {
                        tracing::error!(
                            block = %info.name,
                            resource = %grant.resource,
                            resource_type = ?grant.resource_type,
                            admin = %admin_block,
                            "WRAP: rejecting Network/Storage grant from non-admin block — only the admin block may declare typed Network/Storage grants",
                        );
                    }
                    continue;
                }

                // SECURITY: namespace-based grants — blocks can only grant
                // access to resources they own.
                let grant_owner = if grant.resource.ends_with('*') {
                    let base = grant.resource.trim_end_matches('*');
                    wafer_block::wrap::resource_owner(&format!("{base}x"))
                } else {
                    wafer_block::wrap::resource_owner(&grant.resource)
                };
                match grant_owner {
                    Some(owner) if owner == info.name => all_grants.push(grant.clone()),
                    Some(owner) => tracing::error!(
                        block = %info.name, resource = %grant.resource, owner = %owner,
                        "WRAP: rejecting grant for resource not owned by declaring block"
                    ),
                    None => tracing::error!(
                        block = %info.name, resource = %grant.resource,
                        "WRAP: rejecting grant with unnamespaced resource"
                    ),
                }
            }
        }
        self.wrap_grants = Arc::new(all_grants);
    }

    /// Add extra WRAP grants (e.g. loaded from a database) to the runtime.
    /// These are appended to the existing code-declared grants.
    /// Call this before `start()` / `start_without_bind()`, or between
    /// `start_without_bind()` and the first request.
    pub fn add_wrap_grants(&mut self, grants: Vec<wafer_block::types::ResourceGrant>) {
        let mut all = (*self.wrap_grants).clone();
        all.extend(grants);
        self.wrap_grants = Arc::new(all);
    }

    /// Start the runtime, wrap in `Arc`, and call `bind()` on all blocks.
    ///
    /// # Validation
    ///
    /// Before any block lifecycle event is dispatched, runs:
    /// - **Config presence**: every registered block's declared `config_keys`
    ///   that have no default and no `auto_generate` must be provided, or
    ///   `start()` returns `RuntimeError::Config` with all missing
    ///   `(block, key)` pairs aggregated into a single message. Lifecycle
    ///   events are not dispatched on failure.
    ///
    /// See `crates/wafer-run/src/runtime/validation.rs`.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn start(mut self) -> Result<Arc<Self>, RuntimeError> {
        // CONTRACT: This event is consumed by `wafer dev` (in
        // `wafer-cli/src/commands/dev/summary.rs`) to detect the start of a
        // runtime spawn. The combination of target = "wafer.runtime",
        // event = "starting", and the `blocks` field name is part of the
        // public boot-event contract. Renaming any of those breaks the dev
        // loop's boot summary; coordinate with wafer-cli when changing.
        tracing::info!(
            target: "wafer.runtime",
            event = "starting",
            blocks = self.blocks.len(),
            "wafer runtime starting"
        );
        self.start_without_bind().await?;

        for (name, block) in &self.blocks {
            // Each block gets its own context so WRAP sees the correct caller_id
            // when the block accesses its own resources during startup.
            let ctx = self.make_context(
                "startup",
                name.as_str(),
                HashMap::new(),
                Arc::new(AtomicBool::new(false)),
                None,
            );
            if let Err(e) = block
                .lifecycle(
                    &ctx,
                    LifecycleEvent {
                        event_type: LifecycleType::Start,
                        data: Vec::new(),
                    },
                )
                .await
            {
                tracing::error!(block = %name, error = %e, "block start lifecycle failed");
            }
        }

        let arc_self = Arc::new(self);

        let handle = super::RuntimeHandle {
            inner: arc_self.clone(),
        };
        for block in arc_self.blocks.values() {
            block.bind(Box::new(handle.clone()));
        }

        Ok(arc_self)
    }

    /// Shut down all resolved block instances (works through `Arc`).
    pub async fn shutdown(&self) {
        for (name, block) in &self.blocks {
            let ctx = self.make_context(
                "shutdown",
                name.as_str(),
                HashMap::new(),
                Arc::new(AtomicBool::new(false)),
                None,
            );
            if let Err(e) = block
                .lifecycle(
                    &ctx,
                    LifecycleEvent {
                        event_type: LifecycleType::Stop,
                        data: Vec::new(),
                    },
                )
                .await
            {
                tracing::error!(block = %name, error = %e, "block stop lifecycle failed");
            }
        }
    }

    /// Stop shuts down all resolved block instances (requires `&mut self`).
    pub async fn stop(&mut self) {
        let ctx = self.make_context(
            "shutdown",
            "shutdown",
            HashMap::new(),
            Arc::new(AtomicBool::new(false)),
            None,
        );
        for (name, block) in &self.blocks {
            if let Err(e) = block
                .lifecycle(
                    &ctx,
                    LifecycleEvent {
                        event_type: LifecycleType::Stop,
                        data: Vec::new(),
                    },
                )
                .await
            {
                tracing::error!(block = %name, error = %e, "block stop lifecycle failed");
            }
        }
    }
}

#[cfg(test)]
mod sorted_snapshot_tests {
    use super::*;

    #[test]
    fn sorted_snapshot_orders_by_name() {
        let infos = vec![
            BlockInfo::new("zeta", "0.1.0", "test@v1", "z"),
            BlockInfo::new("alpha", "0.1.0", "test@v1", "a"),
            BlockInfo::new("mu", "0.1.0", "test@v1", "m"),
        ];
        let out = sorted_snapshot(infos);
        let names: Vec<&str> = out.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mu", "zeta"]);
    }
}
