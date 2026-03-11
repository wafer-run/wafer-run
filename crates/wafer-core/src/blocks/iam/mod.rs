use std::sync::Arc;
use crate::clients::database as db;
use crate::clients::database::{Filter, FilterOp, ListOptions};
use wafer_run::*;

/// IAMBlock checks if the authenticated user has a required role.
/// Configure the required role via node config: {"role": "admin"}.
pub struct IAMBlock;

impl IAMBlock {
    pub fn new() -> Self {
        Self
    }

    /// Check if user has the required role by querying iam_user_roles table.
    async fn has_role_db(ctx: &dyn Context, user_id: &str, role: &str) -> Option<bool> {
        let filters = vec![
            Filter {
                field: "user_id".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::Value::String(user_id.to_string()),
            },
            Filter {
                field: "role".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::Value::String(role.to_string()),
            },
        ];

        let opts = ListOptions {
            filters,
            limit: 1,
            ..Default::default()
        };

        match db::list(ctx, "iam_user_roles", &opts).await {
            Ok(result) => Some(!result.records.is_empty()),
            Err(_) => None,
        }
    }

    /// Check if user has the required role from message meta (fallback).
    fn has_role_meta(msg: &Message, role: &str) -> bool {
        let roles_str = msg.get_meta("auth.user_roles");
        if roles_str.is_empty() {
            return false;
        }
        roles_str.split(',').any(|r| r.trim() == role)
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for IAMBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "@wafer/iam-guard".to_string(),
            version: "0.1.0".to_string(),
            interface: "middleware@v1".to_string(),
            summary: "Role-based access control middleware".to_string(),
            instance_mode: InstanceMode::Singleton,
            allowed_modes: Vec::new(),
            admin_ui: None,
            runtime: wafer_run::types::BlockRuntime::Both,
            requires: vec!["@wafer/database".to_string()],
        }
    }

    async fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
        // Check that user is authenticated
        let user_id = msg.user_id().to_string();
        if user_id.is_empty() {
            return err_unauthorized(msg, "Authentication required");
        }

        // Get required role from config (default: "admin")
        let required_role = ctx
            .config_get("role")
            .unwrap_or("admin")
            .to_string();

        // Try database lookup first, fall back to meta roles
        let has_role = match Self::has_role_db(ctx, &user_id, &required_role).await {
            Some(result) => result,
            None => Self::has_role_meta(msg, &required_role),
        };

        if has_role {
            msg.clone().cont()
        } else {
            err_forbidden(msg, &format!("Requires '{}' role", required_role))
        }
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn register(w: &mut Wafer) {
    w.register_block("@wafer/iam-guard", Arc::new(IAMBlock::new()));
}
