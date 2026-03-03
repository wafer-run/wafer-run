use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::block::{Block, BlockInfo};
use crate::common::{ErrorCode, ServiceOp};
use crate::context::Context;
use crate::services::config::ConfigService;
use crate::types::*;
use crate::helpers::{respond_json, respond_empty};

/// ConfigBlock wraps an optional ConfigService and exposes it as a Block.
/// When no ConfigService is provided, falls back to ctx.config_get().
pub struct ConfigBlock {
    service: Option<Arc<dyn ConfigService>>,
}

impl ConfigBlock {
    pub fn new(service: Option<Arc<dyn ConfigService>>) -> Self {
        Self { service }
    }
}

// --- Request types ---

#[derive(Deserialize)]
struct GetRequest {
    key: String,
}

#[derive(Deserialize)]
struct SetRequest {
    key: String,
    value: String,
}

// --- Response types ---

#[derive(Serialize)]
struct GetResponse {
    value: String,
}

impl Block for ConfigBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "wafer/config".to_string(),
            version: "0.1.0".to_string(),
            interface: "wafer.infra.config".to_string(),
            summary: "Configuration key-value access via ConfigService or node config".to_string(),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
        }
    }

    fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
        match msg.kind.as_str() {
            ServiceOp::CONFIG_GET => {
                // Try to parse key from JSON body first, then fall back to meta
                let key = match msg.decode::<GetRequest>() {
                    Ok(req) => req.key,
                    Err(_) => {
                        let meta_key = msg.get_meta("key");
                        if meta_key.is_empty() {
                            return Result_::error(WaferError::new(
                                ErrorCode::INVALID_ARGUMENT,
                                "config.get requires a 'key' in data or meta",
                            ));
                        }
                        meta_key.to_string()
                    }
                };

                // Try ConfigService first, then fall back to ctx.config_get()
                let value = if let Some(ref svc) = self.service {
                    svc.get(&key)
                } else {
                    ctx.config_get(&key).map(|s| s.to_string())
                };

                match value {
                    Some(val) => respond_json(msg, &GetResponse { value: val }),
                    None => Result_::error(WaferError::new(
                        ErrorCode::NOT_FOUND,
                        format!("config key not found: {key}"),
                    )),
                }
            }
            ServiceOp::CONFIG_SET => {
                let req: SetRequest = match msg.decode() {
                    Ok(r) => r,
                    Err(e) => {
                        return Result_::error(WaferError::new(
                            ErrorCode::INVALID_ARGUMENT,
                            format!("invalid config.set request: {e}"),
                        ))
                    }
                };
                match &self.service {
                    Some(svc) => {
                        svc.set(&req.key, &req.value);
                        respond_empty(msg)
                    }
                    None => Result_::error(WaferError::new(
                        ErrorCode::UNAVAILABLE,
                        "config.set requires a ConfigService (no service configured)",
                    )),
                }
            }
            other => Result_::error(WaferError::new(
                ErrorCode::UNIMPLEMENTED,
                format!("unknown config operation: {other}"),
            )),
        }
    }

    fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}
