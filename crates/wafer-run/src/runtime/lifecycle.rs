use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::types::*;

use super::Wafer;

impl Wafer {
    /// Initialize the runtime without calling `bind()` on blocks.
    pub async fn start_without_bind(&mut self) -> Result<(), String> {
        self.resolve().await?;

        // Rebuild the all_blocks map so contexts can see all resolved blocks
        self.rebuild_all_blocks();

        // Snapshot introspection data for contexts
        self.blocks_snapshot = Arc::new(self.blocks.values().map(|b| b.info()).collect());
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
        for block in self.blocks.values() {
            let info = block.info();
            for grant in &info.grants {
                // Network/Storage typed grants use URLs/paths, not namespaced
                // resources — skip ownership validation for these.
                if matches!(
                    grant.resource_type,
                    Some(wafer_block::types::ResourceType::Network)
                        | Some(wafer_block::types::ResourceType::Storage)
                ) {
                    all_grants.push(grant.clone());
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
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn start(mut self) -> Result<Arc<Self>, String> {
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
