use std::sync::Arc;

use crate::context::Context;
use crate::wasm::capabilities::BlockCapabilities;
use super::bindings;

/// HostState stores the wafer Context and capabilities for host function calls.
pub struct HostState {
    pub context: Option<Arc<dyn Context>>,
    pub capabilities: BlockCapabilities,
}

// ---------------------------------------------------------------------------
// Runtime host implementation (call-block, log, is-cancelled)
// ---------------------------------------------------------------------------

impl bindings::runtime::Host for HostState {
    fn is_cancelled(&mut self) -> bool {
        match &self.context {
            Some(ctx) => ctx.is_cancelled(),
            None => false,
        }
    }

    fn call_block(
        &mut self,
        block_name: String,
        msg: bindings::types::Message,
    ) -> bindings::types::BlockResult {
        let ctx = match &self.context {
            Some(ctx) => ctx.clone(),
            None => {
                return bindings::types::BlockResult {
                    action: bindings::types::Action::Error,
                    response: None,
                    error: Some(bindings::types::WaferError {
                        code: bindings::types::ErrorCode::Internal,
                        message: "no context available".to_string(),
                        meta: vec![],
                    }),
                    message: None,
                };
            }
        };

        // Convert WIT message to internal Message
        let mut internal_msg = crate::types::Message {
            kind: msg.kind,
            data: msg.data,
            meta: msg.meta.into_iter().map(|e| (e.key, e.value)).collect(),
        };

        // Call through the context's call_block
        let result = ctx.call_block(&block_name, &mut internal_msg);

        // Convert internal Result_ back to WIT BlockResult
        let action = match result.action {
            crate::types::Action::Continue => bindings::types::Action::Continue,
            crate::types::Action::Respond => bindings::types::Action::Respond,
            crate::types::Action::Drop => bindings::types::Action::Drop,
            crate::types::Action::Error => bindings::types::Action::Error,
        };

        let response = result.response.map(|r| bindings::types::Response {
            data: r.data,
            meta: r.meta.into_iter()
                .map(|(k, v)| bindings::types::MetaEntry { key: k, value: v })
                .collect(),
        });

        let error = result.error.map(|e| {
            let code = match e.code.as_str() {
                "ok" => bindings::types::ErrorCode::Ok,
                "cancelled" => bindings::types::ErrorCode::Cancelled,
                "invalid_argument" => bindings::types::ErrorCode::InvalidArgument,
                "not_found" => bindings::types::ErrorCode::NotFound,
                "already_exists" => bindings::types::ErrorCode::AlreadyExists,
                "permission_denied" => bindings::types::ErrorCode::PermissionDenied,
                "unauthenticated" => bindings::types::ErrorCode::Unauthenticated,
                "resource_exhausted" => bindings::types::ErrorCode::ResourceExhausted,
                "unimplemented" => bindings::types::ErrorCode::Unimplemented,
                "unavailable" => bindings::types::ErrorCode::Unavailable,
                "internal" => bindings::types::ErrorCode::Internal,
                _ => bindings::types::ErrorCode::Unknown,
            };
            bindings::types::WaferError {
                code,
                message: e.message,
                meta: e.meta.into_iter()
                    .map(|(k, v)| bindings::types::MetaEntry { key: k, value: v })
                    .collect(),
            }
        });

        let message = result.message.map(|m| bindings::types::Message {
            kind: m.kind,
            data: m.data,
            meta: m.meta.into_iter()
                .map(|(k, v)| bindings::types::MetaEntry { key: k, value: v })
                .collect(),
        });

        bindings::types::BlockResult {
            action,
            response,
            error,
            message,
        }
    }

    fn log(&mut self, level: String, msg: String) {
        match level.as_str() {
            "debug" => tracing::debug!("{}", msg),
            "info" => tracing::info!("{}", msg),
            "warn" => tracing::warn!("{}", msg),
            "error" => tracing::error!("{}", msg),
            _ => tracing::info!("{}", msg),
        }
    }
}

// ---------------------------------------------------------------------------
// Types host implementation (needed because types is imported too)
// ---------------------------------------------------------------------------

impl bindings::types::Host for HostState {}
