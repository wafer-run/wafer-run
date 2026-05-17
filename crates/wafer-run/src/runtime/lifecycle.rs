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
/// available immediately after registration — no init pass required, and
/// from `Wafer::set_admin_block` to (re-)collect typed grants from blocks
/// that were registered before the admin block was known.
///
/// Rules:
/// - Typed grants (Network/Storage/Crypto) may only be declared by the
///   admin block. If `admin_block` is empty (unset) at registration time,
///   the typed grant is deferred (logged and dropped). `set_admin_block`
///   re-scans all already-registered blocks and re-runs this validation,
///   so deferred typed grants from the admin block are picked up at that
///   point. This accommodates the common pattern of constructing a
///   `Wafer` (which auto-registers linkme-collected blocks during
///   `WaferBuilder::build`) and only then calling `set_admin_block`.
/// - Namespace grants must be owned by the declaring block (per
///   `wafer_block::wrap::resource_owner`). Unnamespaced or owned-by-other
///   grants are logged and dropped.
pub(crate) fn validate_and_collect_grants_for_block(
    block_info: &BlockInfo,
    admin_block: &str,
) -> Vec<wafer_block::types::ResourceGrant> {
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
                // Admin block not yet set: defer this typed grant.
                // `set_admin_block` re-runs this collector for every
                // registered block once admin is known.
                tracing::debug!(
                    block = %block_info.name,
                    resource = %grant.resource,
                    resource_type = ?grant.resource_type,
                    "WRAP: typed grant deferred — admin block not yet set; will be re-collected by set_admin_block",
                );
                continue;
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
    out
}

impl Wafer {
    /// Add extra WRAP grants (e.g. loaded from a database) to the runtime.
    /// These are appended to the existing code-declared grants and tracked
    /// separately so a later `set_admin_block` rescan does not drop them.
    /// Call this before `start()` / `seal()`, or between `seal()` and the
    /// first request.
    pub fn add_wrap_grants(&mut self, grants: Vec<wafer_block::types::ResourceGrant>) {
        self.wrap_grants_external.extend(grants.iter().cloned());
        let mut all = (*self.wrap_grants).clone();
        all.extend(grants);
        self.wrap_grants = Arc::new(all);
    }

    /// Eagerly run `lifecycle(Init)` on every registered block. Lazy init
    /// semantics for [`Wafer::run_block`] / `call_block` are preserved —
    /// this method just pre-runs Init for every block so on-boot failures
    /// surface before the first request.
    ///
    /// Used by:
    /// - [`Wafer::start`] (native), which needs every block initialized
    ///   before `bind()` because some blocks (e.g. `wafer-run/http-listener`)
    ///   read their config in Init and consume it in `bind()`.
    /// - Cloudflare Workers' boot path (`solobase-cloudflare`), which calls
    ///   [`Wafer::seal`] but not `start()`/`bind()`. Without an eager pass,
    ///   blocks that need Init-time side effects (e.g. admin block running
    ///   its own migrations) never fire until a request happens to touch
    ///   them transitively, leaving fresh deploys in a broken state.
    ///
    /// Init failures are logged and tolerated — transient failures can be
    /// retried by a later dispatch, and tolerating permanent ones keeps boot
    /// resilient when one misconfigured block would otherwise wedge the
    /// whole runtime. Callers that need ordering guarantees should call
    /// [`Wafer::init_block`] for the required-first blocks before invoking
    /// this method (slot caching makes the second call a no-op for any
    /// block already initialised).
    pub async fn init_all_blocks(&self) {
        let block_names: Vec<String> = self.blocks.keys().cloned().collect();
        for name in &block_names {
            let stack = crate::runtime::init_stack::InitStack::new();
            if let Err(e) = self.init_block_with_stack(name, &stack).await {
                tracing::error!(
                    block = %name,
                    error = %e,
                    "block init lifecycle failed during init_all_blocks",
                );
            }
        }
    }

    /// Start the runtime, wrap in `Arc`, and call `bind()` on all blocks.
    ///
    /// Finalizes runtime configuration via [`Wafer::seal`] (composite config
    /// expansion, `uses` contributions, capability resolution, snapshot
    /// finalization), then eagerly dispatches `lifecycle(Init)` on every
    /// registered block before dispatching `lifecycle(Start)` and calling
    /// `bind()`. This is the eager native entry-point: blocks like
    /// `wafer-run/http-listener` read configuration in `Init` and use it in
    /// `bind()` (e.g. the TCP listen address), so `start()` must guarantee
    /// every block is fully initialized before `bind()` runs.
    ///
    /// Lazy init semantics still apply to the dispatch paths
    /// ([`Wafer::run_block`], `call_block`) and to consumers that call
    /// [`Wafer::seal`] directly (e.g. Cloudflare Workers, which never call
    /// `start()` / `bind()`).
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
        self.init_all_blocks().await;

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
