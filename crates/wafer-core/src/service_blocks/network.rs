use std::sync::Arc;

use wafer_block::types::ConfigVar;

use crate::interfaces::network::{handler, service::NetworkService};

crate::service_block! {
    /// Unified network block. Wraps any `NetworkService` implementation.
    block: pub NetworkBlock,
    name: "wafer-run/network",
    version: "0.0.1",
    interface: "http-client@v1",
    description: "Outbound HTTP requests",
    category: Service,
    fields: { service: Arc<dyn NetworkService> },
    info_extras: |_this, info| info.config_keys(vec![ConfigVar::new(
        "WAFER_RUN__NETWORK__MAX_RESPONSE_BYTES",
        "Maximum response body size in bytes accepted by the HTTP \
         network service. Responses exceeding this limit are rejected \
         with an error rather than buffered. Defaults to 50 MiB \
         (52428800) when unset.",
        "52428800",
    )
    .name("Max Response Body Bytes")]),
    handle: |this, _ctx, msg, body| {
        handler::handle_message(this.service.as_ref(), &msg, &body).await
    },
}
