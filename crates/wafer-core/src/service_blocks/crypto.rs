use std::sync::Arc;

use crate::interfaces::crypto::{handler, service::CryptoService};

crate::service_block! {
    /// Unified crypto block. Wraps any `CryptoService` implementation.
    block: pub CryptoBlock,
    name: "wafer-run/crypto",
    version: "0.0.1",
    interface: "crypto@v1",
    description: "Cryptographic operations (hashing, JWT, random bytes)",
    category: Service,
    fields: { service: Arc<dyn CryptoService> },
    handle: |this, ctx, msg, body| {
        // Native: Argon2-heavy ops offload to the blocking pool (PERF-02);
        // wasm32 has no blocking pool and keeps the pure sync path.
        #[cfg(not(target_arch = "wasm32"))]
        {
            handler::handle_message_native(&this.service, ctx, ctx.caller_id(), &msg, &body).await
        }
        #[cfg(target_arch = "wasm32")]
        {
            handler::handle_message(this.service.as_ref(), ctx, ctx.caller_id(), &msg, &body)
        }
    },
}
