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
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
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
