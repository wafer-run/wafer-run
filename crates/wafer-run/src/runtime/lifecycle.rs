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

/// Validate a single block's WRAP grant declarations and return the subset
/// that passed validation. Called from `register_block_inner` so grants are
/// available immediately after registration — no init pass required.
///
/// Rules:
/// - Typed grants (Network/Storage/Crypto) may only be declared by the
///   admin block. If `admin_block` is empty (unset), returns
///   `RuntimeError::WrapGrantAdminUnset` — we fail loud rather than
///   silently dropping a security-sensitive grant.
/// - Namespace grants must be owned by the declaring block (per
///   `wafer_block::wrap::resource_owner`). Unnamespaced or owned-by-other
///   grants are logged and dropped (matches prior behaviour).
pub(crate) fn validate_and_collect_grants_for_block(
    block_info: &BlockInfo,
    admin_block: &str,
) -> Result<Vec<wafer_block::types::ResourceGrant>, RuntimeError> {
    let mut out = Vec::new();
    for grant in &block_info.grants {
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
            if admin_block.is_empty() {
                return Err(RuntimeError::WrapGrantAdminUnset {
                    block: block_info.name.clone(),
                    resource_type: format!("{:?}", grant.resource_type),
                });
            }
            if block_info.name == admin_block {
                out.push(grant.clone());
            } else {
                tracing::error!(
                    block = %block_info.name,
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
            Some(owner) if owner == block_info.name => out.push(grant.clone()),
            Some(owner) => tracing::error!(
                block = %block_info.name, resource = %grant.resource, owner = %owner,
                "WRAP: rejecting grant for resource not owned by declaring block"
            ),
            None => tracing::error!(
                block = %block_info.name, resource = %grant.resource,
                "WRAP: rejecting grant with unnamespaced resource"
            ),
        }
    }
    Ok(out)
}

impl Wafer {
    /// Add extra WRAP grants (e.g. loaded from a database) to the runtime.
    /// These are appended to the existing code-declared grants.
    /// Call this before `start()` / `seal()`, or between `seal()` and the
    /// first request.
    pub fn add_wrap_grants(&mut self, grants: Vec<wafer_block::types::ResourceGrant>) {
        let mut all = (*self.wrap_grants).clone();
        all.extend(grants);
        self.wrap_grants = Arc::new(all);
    }

    /// Start the runtime, wrap in `Arc`, and call `bind()` on all blocks.
    ///
    /// Finalizes runtime configuration via [`Wafer::seal`] (composite config
    /// expansion, `uses` contributions, capability resolution, snapshot
    /// finalization), then dispatches `lifecycle(Start)` on every registered
    /// block. Block `Init` is **not** dispatched here — each block is
    /// initialized lazily on first dispatch via [`Wafer::init_block`].
    ///
    /// Use [`Wafer::validate_all_block_configs`] before `start()` for
    /// proactive health checks; broken-config blocks otherwise surface as
    /// 5xx on first invocation.
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
        self.seal().await?;

        for (name, block) in &self.blocks {
            // Each block gets its own context so WRAP sees the correct caller_id
            // when the block accesses its own resources during startup.
            let ctx = self.make_context(
                "startup",
                name.as_str(),
                HashMap::new(),
                Arc::new(AtomicBool::new(false)),
                None,
                crate::runtime::init_stack::InitStack::new(),
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
                crate::runtime::init_stack::InitStack::new(),
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
            crate::runtime::init_stack::InitStack::new(),
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
