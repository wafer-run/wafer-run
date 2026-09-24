use std::sync::Arc;

use wafer_block::{config::BlockConfig, types::ConfigVar, ErrorCode, LifecycleType, WaferError};

use crate::interfaces::network::{
    handler,
    service::{
        NetworkLimits, NetworkService, CONNECT_TIMEOUT_SECS_KEY, MAX_RESPONSE_BYTES_KEY,
        READ_TIMEOUT_SECS_KEY, REQUEST_TIMEOUT_SECS_KEY, STREAM_TIMEOUT_SECS_KEY,
    },
};

crate::service_block! {
    /// Unified network block. Wraps any `NetworkService` implementation.
    block: pub NetworkBlock,
    name: "wafer-run/network",
    version: "0.0.1",
    interface: "http-client@v1",
    description: "Outbound HTTP requests",
    category: Service,
    fields: { service: Arc<dyn NetworkService> },
    info_extras: |_this, info| info.config_keys(vec![
        ConfigVar::new(
            MAX_RESPONSE_BYTES_KEY,
            "Maximum response body size in bytes accepted by the HTTP \
             network service. Responses exceeding this limit are rejected \
             with an error rather than buffered. Defaults to 50 MiB \
             (52428800). Read at the block's Init: an invalid value fails \
             Init, and a change applies after a restart.",
            "52428800",
        )
        .name("Max Response Body Bytes"),
        ConfigVar::new(
            CONNECT_TIMEOUT_SECS_KEY,
            "Seconds the HTTP network service waits to connect to an \
             upstream server (DNS, TCP and TLS). Defaults to 10. Read at \
             the block's Init: an invalid value fails Init, and a change \
             applies after a restart.",
            "10",
        )
        .name("Connect Timeout (s)"),
        ConfigVar::new(
            READ_TIMEOUT_SECS_KEY,
            "Seconds an upstream connection may go without sending data \
             (waiting for the response, or between body chunks) before \
             the request fails. Applies to streaming and buffered \
             requests. Defaults to 30. Read at the block's Init: an invalid \
             value fails Init, and a change applies after a restart.",
            "30",
        )
        .name("Idle Read Timeout (s)"),
        ConfigVar::new(
            REQUEST_TIMEOUT_SECS_KEY,
            "Total seconds allowed for a buffered request, response body \
             included. Streaming requests use the stream timeout \
             instead. Defaults to 30. Read at the block's Init: an invalid \
             value fails Init, and a change applies after a restart.",
            "30",
        )
        .name("Buffered Request Timeout (s)"),
        ConfigVar::new(
            STREAM_TIMEOUT_SECS_KEY,
            "Total seconds allowed for a streaming request, response body \
             included. Empty (the default): no total, so an upstream that \
             keeps sending a byte within every idle read timeout holds the \
             stream open indefinitely. Read at the block's Init: an invalid \
             value fails Init, and a change applies after a restart.",
            "",
        )
        .name("Streaming Request Timeout (s)")
        // Unset means "no total", a value of its own: without `optional` an
        // empty default makes the key required, and every source that
        // leaves it unset fails this block's Init.
        .optional(),
    ]),
    handle: |this, ctx, msg, body| {
        handler::handle_message(this.service.as_ref(), ctx, &msg, &body).await
    },
    // The limits are the block's declared config, resolved by the runtime
    // through the embedder's ConfigSource into the Init payload; the service
    // applies them to every request after this.
    lifecycle: |this, _ctx, event| {
        if event.event_type == LifecycleType::Init {
            let limits = NetworkLimits::from_config(&BlockConfig::from_event(&event))
                .map_err(|e| {
                    WaferError::new(ErrorCode::FailedPrecondition, format!("wafer-run/network: {e}"))
                })?;
            this.service.configure(limits);
        }
        Ok(())
    },
}
