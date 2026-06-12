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

/// Return value of [`validate_and_collect_grants_for_block`].
///
/// Splits accepted and rejected grants so callers can both merge accepted
/// grants into `Wafer::wrap_grants` and push rejected grants into
/// `Wafer::grant_validation_errors` for `start()` to surface as
/// `RuntimeError::GrantsRejected`.
pub(crate) struct GrantValidationOutcome {
    /// Grants that passed validation and should be merged into `wrap_grants`.
    pub(crate) accepted: Vec<wafer_block::types::ResourceGrant>,
    /// Grants that were rejected — typed grant from non-admin block.
    /// Each entry is a structured rejection that `Wafer::start()` aggregates
    /// into `RuntimeError::GrantsRejected`.
    pub(crate) rejected: Vec<crate::error::GrantValidationError>,
}

/// Validate a single block's WRAP grant declarations and return the subset
/// that passed validation. Called from `register_block_inner` so grants are
/// available immediately after registration — no init pass required, and
/// from `Wafer::set_admin_block` to (re-)collect typed grants from blocks
/// that were registered before the admin block was known.
///
/// Rules:
/// - Typed Network/Crypto grants may only be declared by the admin block
///   (their resources — URLs and operation names — aren't namespaced).
///   If `admin_block` is empty (unset) at registration time, the typed
///   grant is deferred (logged and dropped). `set_admin_block` re-scans
///   all already-registered blocks and re-runs this validation, so
///   deferred typed grants from the admin block are picked up at that
///   point. This accommodates the common pattern of constructing a
///   `Wafer` (which auto-registers linkme-collected blocks during
///   `WaferBuilder::build`) and only then calling `set_admin_block`.
/// - Storage grants (Wave 26 / c18) and Db / untyped grants are
///   namespace-based: a grant must target a resource owned by the
///   declaring block, per
///   [`wafer_block::wrap::typed_resource_owner`]. Unnamespaced or
///   owned-by-other grants are pushed into `rejected` so `seal()`
///   surfaces them via `RuntimeError::GrantsRejected`.
pub(crate) fn validate_and_collect_grants_for_block(
    block_info: &BlockInfo,
    admin_block: &str,
) -> GrantValidationOutcome {
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();
    for grant in &block_info.grants {
        // Network and Crypto resources aren't namespace-bound (URLs,
        // operation names), so they remain admin-only — without this,
        // any block could grant `*` Network / Crypto access and bypass
        // default-deny. Storage was also admin-only before Wave 26;
        // with c18 it became namespace-bound (`{org}/{block}/...`) and
        // routes through the shared ownership check below via
        // `typed_resource_owner`.
        if matches!(
            grant.resource_type,
            Some(wafer_block::types::ResourceType::Network)
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
                accepted.push(grant.clone());
            } else {
                tracing::error!(
                    block = %block_info.name,
                    resource = %grant.resource,
                    resource_type = ?grant.resource_type,
                    admin = %admin_block,
                    "WRAP: rejecting Network/Storage grant from non-admin block — only the admin block may declare typed Network/Storage grants",
                );
                rejected.push(crate::error::GrantValidationError {
                    block: block_info.name.to_string(),
                    grant: grant.clone(),
                    reason: format!(
                        "typed {:?} grants may only be declared by the admin block",
                        grant.resource_type.clone().unwrap(),
                    ),
                });
            }
            continue;
        }

        // SECURITY: namespace-based grants — blocks can only grant
        // access to resources they own. Dispatches to the right parser
        // based on resource type (Storage uses `{org}/{block}/...`,
        // Db / untyped use `{org}__{block}__...`).
        let grant_owner = if grant.resource.ends_with('*') {
            let base = grant.resource.trim_end_matches('*');
            wafer_block::wrap::typed_resource_owner(
                &format!("{base}x"),
                grant.resource_type.as_ref(),
            )
        } else {
            wafer_block::wrap::typed_resource_owner(&grant.resource, grant.resource_type.as_ref())
        };
        match grant_owner {
            Some(owner) if owner == block_info.name => accepted.push(grant.clone()),
            Some(owner) => {
                tracing::error!(
                    block = %block_info.name, resource = %grant.resource, owner = %owner,
                    "WRAP: rejecting grant for resource not owned by declaring block"
                );
                rejected.push(crate::error::GrantValidationError {
                    block: block_info.name.to_string(),
                    grant: grant.clone(),
                    reason: format!(
                        "resource `{}` is owned by `{owner}`, not by declaring block",
                        grant.resource,
                    ),
                });
            }
            None => {
                tracing::error!(
                    block = %block_info.name, resource = %grant.resource,
                    "WRAP: rejecting grant with unnamespaced resource"
                );
                // Shape the hint to the resource type. Storage grants
                // expect `{org}/{block}/...`; Db / untyped expect
                // `{org}__{block}__...`.
                let expected_shape = match grant.resource_type {
                    Some(wafer_block::types::ResourceType::Storage) => {
                        "`{org}/{block}/*` (Storage paths)"
                    }
                    _ => "`{org}__{block}__*` (Db tables)",
                };
                rejected.push(crate::error::GrantValidationError {
                    block: block_info.name.to_string(),
                    grant: grant.clone(),
                    reason: format!(
                        "resource `{}` is unnamespaced — namespace-based grants must target {expected_shape}",
                        grant.resource,
                    ),
                });
            }
        }
    }
    GrantValidationOutcome { accepted, rejected }
}

impl Wafer {
    /// Add extra WRAP grants (e.g. loaded from a database) to the runtime.
    /// These are appended to the existing code-declared grants and tracked
    /// separately so a later `set_admin_block` rescan does not drop them.
    /// Call this before `start()` / `seal()`, or between `seal()` and the
    /// first request.
    ///
    /// **Security:** Grants added here BYPASS the admin-only typed-grant
    /// validator that gates `BlockInfo::grants` declarations. This is the
    /// intended application-side escape hatch for grants that don't live
    /// in any block's static declaration (e.g. operator-configured grants
    /// loaded from a DB at boot). The caller is responsible for vetting
    /// any typed Network/Storage/Crypto grants added through this method —
    /// no admin-block check is applied.
    pub fn add_wrap_grants(&mut self, grants: Vec<wafer_block::types::ResourceGrant>) {
        self.registration.add_wrap_grants(grants);
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
        let block_names: Vec<String> = self.registration.blocks.keys().cloned().collect();
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
    pub async fn start(self) -> Result<Arc<Self>, RuntimeError> {
        self.start_with_priority(&[]).await
    }

    /// Like [`Wafer::start`], but eagerly initializes the named
    /// `priority_blocks` in order BEFORE `init_all_blocks` runs. Useful when
    /// one block's `Init` step creates infrastructure (tables, secrets,
    /// snapshots) that subsequent block inits depend on — without this hook
    /// `init_all_blocks` iterates `HashMap::keys()` in unspecified order,
    /// which means dependent blocks can lose the race and permanent-fail on
    /// missing state.
    ///
    /// Slot caching ([`crate::runtime::slot::BlockSlot::get_or_init`]) makes
    /// the second `init_block` call inside `init_all_blocks` a no-op for any
    /// `priority_blocks` entry already initialized here. Priority-block init
    /// failures are logged-and-tolerated, matching the resilience contract
    /// of `init_all_blocks`.
    ///
    /// Unknown names in `priority_blocks` are silently skipped — no point in
    /// failing the whole runtime over a typo, and the rest of the start path
    /// will surface the resolution error on first call to the missing block.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn start_with_priority(
        mut self,
        priority_blocks: &[&str],
    ) -> Result<Arc<Self>, RuntimeError> {
        // CONTRACT: This event is consumed by `wafer dev` (in
        // `wafer-cli/src/commands/dev/summary.rs`) to detect the start of a
        // runtime spawn. The combination of target = "wafer.runtime",
        // event = "starting", and the `blocks` field name is part of the
        // public boot-event contract. Renaming any of those breaks the dev
        // loop's boot summary; coordinate with wafer-cli when changing.
        tracing::info!(
            target: "wafer.runtime",
            event = "starting",
            blocks = self.registration.blocks.len(),
            "wafer runtime starting"
        );
        self.seal().await?;
        for name in priority_blocks {
            if !self.registration.blocks.contains_key(*name) {
                continue;
            }
            if let Err(e) = self.init_block(name).await {
                tracing::warn!(
                    block = %name,
                    error = %e,
                    "priority block init failed; non-priority blocks will continue",
                );
            }
        }
        self.init_all_blocks().await;

        for (name, block) in &self.registration.blocks {
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
        let trait_handle: Arc<dyn wafer_block::Runtime> = Arc::new(handle);
        for block in arc_self.registration.blocks.values() {
            block.bind(Box::new(trait_handle.clone()));
        }

        Ok(arc_self)
    }

    /// Shut down all resolved block instances (works through `Arc`).
    ///
    /// Each block gets its own context (`node_id` = block name) so WRAP sees
    /// the correct caller when a block accesses its own resources during
    /// `lifecycle(Stop)` — a single shared sentinel context would attribute
    /// every access to a literal name no grants match and falsely deny.
    pub async fn shutdown(&self) {
        for (name, block) in &self.registration.blocks {
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
