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
        handler::handle_message(this.service.as_ref(), ctx.caller_id(), &msg, &body)
    },
}
