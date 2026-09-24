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
    info_extras: |_this, info| info.config_keys(vec![
        ConfigVar::new(
            "WAFER_RUN__NETWORK__MAX_RESPONSE_BYTES",
            "Maximum response body size in bytes accepted by the HTTP \
             network service. Responses exceeding this limit are rejected \
             with an error rather than buffered. Defaults to 50 MiB \
             (52428800) when unset. Parsed once at startup: an invalid \
             value fails service construction (requires restart to apply).",
            "52428800",
        )
        .name("Max Response Body Bytes"),
        ConfigVar::new(
            "WAFER_RUN__NETWORK__CONNECT_TIMEOUT_SECS",
            "Seconds the HTTP network service waits to connect to an \
             upstream server (DNS, TCP and TLS). Defaults to 10. Parsed \
             once at startup: an invalid value fails service construction \
             (requires restart to apply).",
            "10",
        )
        .name("Connect Timeout (s)"),
        ConfigVar::new(
            "WAFER_RUN__NETWORK__READ_TIMEOUT_SECS",
            "Seconds an upstream connection may go without sending data \
             (waiting for the response, or between body chunks) before \
             the request fails. Applies to streaming and buffered \
             requests. Defaults to 30. Parsed once at startup: an invalid \
             value fails service construction (requires restart to apply).",
            "30",
        )
        .name("Idle Read Timeout (s)"),
        ConfigVar::new(
            "WAFER_RUN__NETWORK__REQUEST_TIMEOUT_SECS",
            "Total seconds allowed for a buffered request, response body \
             included. Streaming requests use the stream timeout \
             instead. Defaults to 30. Parsed once at startup: an invalid \
             value fails service construction (requires restart to apply).",
            "30",
        )
        .name("Buffered Request Timeout (s)"),
        ConfigVar::new(
            "WAFER_RUN__NETWORK__STREAM_TIMEOUT_SECS",
            "Total seconds allowed for a streaming request, response body \
             included. Empty (the default): no total, so an upstream that \
             keeps sending a byte within every idle read timeout holds the \
             stream open indefinitely. Parsed once at startup: an invalid \
             value fails service construction (requires restart to apply).",
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
}
