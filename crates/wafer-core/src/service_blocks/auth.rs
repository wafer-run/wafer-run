//! Unified auth block.
//!
//! Wraps any `AuthService` implementation behind the `Block` trait; the
//! shared handler in `interfaces::auth::handler` routes `auth.*` messages.

use std::sync::Arc;

use wafer_block::{ErrorCode, LifecycleType, WaferError};

use crate::interfaces::auth::{handler, service::AuthService};

crate::service_block! {
    /// Unified auth block. Wraps any `AuthService` implementation.
    block: pub AuthBlock,
    name: "wafer-run/auth",
    version: "0.0.1",
    interface: "auth@v1",
    description: "Identity, sessions, PATs, orgs — see auth-block-design spec",
    category: Service,
    fields: { service: Arc<dyn AuthService> },
    info_extras: |this, info| info.grants(this.service.grants()),
    handle: |this, _ctx, msg, body| {
        handler::handle_message(this.service.as_ref(), &msg, &body).await
    },
    lifecycle: |this, ctx, event| {
        use crate::interfaces::auth::service::AuthError;
        if matches!(event.event_type, LifecycleType::Init) {
            this.service.init(ctx).await.map_err(|e| match e {
                AuthError::Internal(msg) => WaferError::new(ErrorCode::Internal, msg),
                other => WaferError::new(ErrorCode::Internal, format!("auth init: {other}")),
            })?;
        }
        Ok(())
    },
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use wafer_block::{Block, LifecycleEvent, LifecycleType};
    use wafer_block_macro::wafer_async_trait;

    use super::*;
    use crate::interfaces::auth::service::{AuthError, AuthService};

    /// Stub service that counts `init()` calls.
    struct InitCounterService {
        inits: Arc<AtomicUsize>,
    }

    #[wafer_async_trait]
    impl AuthService for InitCounterService {
        async fn init(&self, _ctx: &dyn wafer_block::Context) -> Result<(), AuthError> {
            self.inits.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        // All other AuthService methods rely on the trait's fail-closed defaults.
    }

    #[tokio::test]
    async fn init_lifecycle_event_invokes_service_init() {
        let counter = Arc::new(AtomicUsize::new(0));
        let svc = Arc::new(InitCounterService {
            inits: counter.clone(),
        });
        let block = AuthBlock::new(svc);

        let ctx = crate::test_support::noop_context();
        let event = LifecycleEvent {
            event_type: LifecycleType::Init,
            data: Vec::new(),
        };
        block.lifecycle(&*ctx, event).await.expect("init lifecycle");

        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "service.init should be called once"
        );
    }

    #[tokio::test]
    async fn noop_context_clone_arc_round_trips_through_dyn() {
        // PR 5b sanity: take a `&dyn Context` from `noop_context()`, upgrade
        // to an `Arc<dyn Context>` via `clone_arc`, and verify trait methods
        // still dispatch through the cloned handle. Exercises the new
        // object-safe `Context::clone_arc` shape end-to-end.
        let ctx = crate::test_support::noop_context();
        let dyn_ref: &dyn wafer_block::Context = &*ctx;
        let arc = dyn_ref.clone_arc();
        drop(ctx);
        // Trait methods should still work through the cloned Arc.
        assert!(!arc.is_cancelled());
        assert!(arc.config_get("anything").is_none());
        assert!(arc.registered_blocks().is_empty());
    }

    #[tokio::test]
    async fn non_init_lifecycle_does_not_invoke_service_init() {
        let counter = Arc::new(AtomicUsize::new(0));
        let svc = Arc::new(InitCounterService {
            inits: counter.clone(),
        });
        let block = AuthBlock::new(svc);

        let ctx = crate::test_support::noop_context();
        for kind in [LifecycleType::Start, LifecycleType::Stop] {
            let event = LifecycleEvent {
                event_type: kind,
                data: Vec::new(),
            };
            block
                .lifecycle(&*ctx, event)
                .await
                .expect("non-init lifecycle");
        }

        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "service.init should NOT be called for Start/Stop"
        );
    }

    /// Stub service whose `grants()` returns one read-only ResourceGrant.
    struct GrantsService {
        grants: Vec<wafer_block::types::ResourceGrant>,
    }

    #[wafer_async_trait]
    impl AuthService for GrantsService {
        fn grants(&self) -> Vec<wafer_block::types::ResourceGrant> {
            self.grants.clone()
        }
        // All other AuthService methods rely on the trait's fail-closed defaults.
    }

    #[test]
    fn block_info_embeds_service_grants() {
        let grant =
            wafer_block::types::ResourceGrant::read("test/consumer", "wafer_run__auth__sessions");
        let svc = Arc::new(GrantsService {
            grants: vec![grant],
        });
        let block = AuthBlock::new(svc);
        let info = block.info();
        assert_eq!(
            info.grants.len(),
            1,
            "grants should round-trip from service"
        );
        assert_eq!(info.grants[0].grantee, "test/consumer");
        assert_eq!(info.grants[0].resource, "wafer_run__auth__sessions");
        assert!(!info.grants[0].write);
    }

    #[test]
    fn block_info_grants_default_empty() {
        let counter = Arc::new(AtomicUsize::new(0));
        let svc = Arc::new(InitCounterService { inits: counter });
        let block = AuthBlock::new(svc);
        let info = block.info();
        assert!(
            info.grants.is_empty(),
            "default AuthService::grants should be empty"
        );
    }
}
