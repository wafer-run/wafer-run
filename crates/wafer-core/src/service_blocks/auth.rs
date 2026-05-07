//! Unified auth block. Mirrors `service_blocks/crypto.rs` shape.
//!
//! Wraps any `AuthService` implementation behind the `Block` trait; the
//! shared handler in `interfaces::auth::handler` routes `auth.*` messages.

use std::sync::Arc;

use wafer_block::{
    block::Block,
    context::Context,
    streams::{input::InputStream, output::OutputStream},
    types::BlockInfo,
    BlockRegistry, RuntimeError, *,
};

use crate::interfaces::auth::{handler, service::AuthService};

/// Unified auth block. Wraps any `AuthService` implementation.
pub struct AuthBlock {
    service: Arc<dyn AuthService>,
}

impl AuthBlock {
    pub fn new(service: Arc<dyn AuthService>) -> Self {
        Self { service }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for AuthBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "suppers-ai/auth",
            "0.0.1",
            "auth@v1",
            "Identity, sessions, PATs, orgs — see auth-block-design spec",
        )
        .category(BlockCategory::Service)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        let body = input.collect_to_bytes().await;
        handler::handle_message(self.service.as_ref(), &msg, &body).await
    }

    async fn lifecycle(
        &self,
        ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        use crate::interfaces::auth::service::AuthError;
        if matches!(event.event_type, LifecycleType::Init) {
            self.service.init(ctx).await.map_err(|e| match e {
                AuthError::Internal(msg) => WaferError::new(ErrorCode::INTERNAL, msg),
                other => WaferError::new(ErrorCode::INTERNAL, format!("auth init: {other}")),
            })?;
        }
        Ok(())
    }
}

/// Register the unified auth block with the given service.
pub fn register_with(
    w: &mut dyn BlockRegistry,
    service: Arc<dyn AuthService>,
) -> Result<(), RuntimeError> {
    w.register_block("suppers-ai/auth", Arc::new(AuthBlock::new(service)))
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use super::*;
    use crate::interfaces::auth::service::{
        AuthError, AuthService, Role, TokenScope, UserId, UserProfile,
    };

    /// Stub service that counts `init()` calls.
    struct InitCounterService {
        inits: Arc<AtomicUsize>,
    }

    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    impl AuthService for InitCounterService {
        async fn init(&self, _ctx: &dyn Context) -> Result<(), AuthError> {
            self.inits.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn require_user(&self, _msg: &Message) -> Result<UserId, AuthError> {
            Err(AuthError::Unauthorized)
        }
        async fn require_token(
            &self,
            _msg: &Message,
            _scope: TokenScope,
        ) -> Result<UserId, AuthError> {
            Err(AuthError::Unauthorized)
        }
        async fn require_role(&self, _msg: &Message, _role: Role) -> Result<UserId, AuthError> {
            Err(AuthError::Unauthorized)
        }
        async fn verify_org_admin(
            &self,
            _user: UserId,
            _provider: &str,
            _org_ref: &str,
        ) -> Result<bool, AuthError> {
            Ok(false)
        }
        async fn user_profile(&self, _user: UserId) -> Result<UserProfile, AuthError> {
            Err(AuthError::NotFound)
        }
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
}
