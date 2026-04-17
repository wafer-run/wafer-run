use std::sync::Arc;

use wafer_block::*;
use wafer_core::clients::{
    database as db,
    database::{Filter, FilterOp, ListOptions},
};

/// IAMBlock checks if the authenticated user has a required role.
/// Configure the required role via node config: {"role": "admin"}.
pub struct IAMBlock;

impl Default for IAMBlock {
    fn default() -> Self {
        Self::new()
    }
}

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
        BlockInfo::new(
            "wafer-run/iam-guard",
            "0.0.1",
            "middleware@v1",
            "Role-based access control middleware",
        )
        .instance_mode(InstanceMode::Singleton)
        .requires(vec!["wafer-run/database".into()])
        .category(BlockCategory::Infrastructure)
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        // Check that user is authenticated
        let user_id = msg.user_id().to_string();
        if user_id.is_empty() {
            return OutputStream::error(WaferError {
                code: ErrorCode::Unauthenticated,
                message: "Authentication required".to_string(),
                meta: vec![],
            });
        }

        // Get required role from config (default: "admin")
        let required_role = ctx.config_get("role").unwrap_or("admin").to_string();

        // Try database lookup first, fall back to meta roles
        let has_role = match Self::has_role_db(ctx, &user_id, &required_role).await {
            Some(result) => result,
            None => Self::has_role_meta(&msg, &required_role),
        };

        if has_role {
            OutputStream::continue_with(msg)
        } else {
            OutputStream::error(WaferError {
                code: ErrorCode::PermissionDenied,
                message: format!("Requires '{required_role}' role"),
                meta: vec![],
            })
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

pub fn register(w: &mut dyn wafer_block::BlockRegistry) -> Result<(), wafer_block::RuntimeError> {
    w.register_block("wafer-run/iam-guard", Arc::new(IAMBlock::new()))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use wafer_block::{
        streams::{input::InputStream, output::TerminalNotResponse},
        Message,
    };
    use wafer_test_support::{
        builder::WaferBuilder,
        fake_db::{FailureMode, FakeDb},
    };

    use super::*;

    async fn build_wafer(
        db: Arc<FakeDb>,
        config: Option<serde_json::Value>,
    ) -> Arc<wafer_run::Wafer> {
        // iam-guard queries the unnamespaced `iam_user_roles` table, which WRAP
        // denies unless the caller is the admin block. `run_block` builds the
        // context with node_id = "root" (the flow_id argument), so the WRAP
        // caller presented during iam-guard's nested DB call is "root".
        let mut b = WaferBuilder::new()
            .with_fake_db(db)
            .with_block("wafer-run/iam-guard", Arc::new(IAMBlock::new()))
            .with_admin_block("root");
        if let Some(cfg) = config {
            b = b.with_config("wafer-run/iam-guard", cfg);
        }
        b.build().await.expect("build")
    }

    fn with_user(mut msg: Message, user_id: &str, roles: &str) -> Message {
        msg.set_meta("auth.user_id", user_id);
        msg.set_meta("auth.user_roles", roles);
        msg
    }

    #[tokio::test]
    async fn user_with_required_role_from_db_continues() {
        let db = Arc::new(FakeDb::new());
        db.seed(
            "iam_user_roles",
            vec![json!({"id": "r1", "user_id": "u1", "role": "admin"})],
        );
        let wafer = build_wafer(db, Some(json!({"role": "admin"}))).await;

        let msg = with_user(Message::new("http.request"), "u1", "");
        match wafer
            .run_block("wafer-run/iam-guard", msg, InputStream::empty())
            .await
            .collect_buffered()
            .await
        {
            Err(TerminalNotResponse::Continue(_)) => {}
            other => panic!("expected Continue, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn user_without_required_role_from_db_denies() {
        let db = Arc::new(FakeDb::new());
        db.seed(
            "iam_user_roles",
            vec![json!({"id": "r1", "user_id": "u1", "role": "viewer"})],
        );
        let wafer = build_wafer(db, Some(json!({"role": "admin"}))).await;

        let msg = with_user(Message::new("http.request"), "u1", "");
        match wafer
            .run_block("wafer-run/iam-guard", msg, InputStream::empty())
            .await
            .collect_buffered()
            .await
        {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::PERMISSION_DENIED);
            }
            other => panic!("expected PermissionDenied, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn db_unavailable_falls_back_to_meta_roles() {
        let db = Arc::new(FakeDb::new());
        db.set_failure(FailureMode::Unavailable);
        let wafer = build_wafer(db, Some(json!({"role": "admin"}))).await;

        let msg = with_user(Message::new("http.request"), "u1", "admin,viewer");
        match wafer
            .run_block("wafer-run/iam-guard", msg, InputStream::empty())
            .await
            .collect_buffered()
            .await
        {
            Err(TerminalNotResponse::Continue(_)) => {}
            other => panic!("expected Continue, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn db_unavailable_meta_roles_missing_denies() {
        let db = Arc::new(FakeDb::new());
        db.set_failure(FailureMode::Unavailable);
        let wafer = build_wafer(db, Some(json!({"role": "admin"}))).await;

        let msg = with_user(Message::new("http.request"), "u1", "viewer");
        match wafer
            .run_block("wafer-run/iam-guard", msg, InputStream::empty())
            .await
            .collect_buffered()
            .await
        {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::PERMISSION_DENIED);
            }
            other => panic!("expected PermissionDenied, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn no_auth_meta_denies_regardless_of_db() {
        let db = Arc::new(FakeDb::new());
        db.seed(
            "iam_user_roles",
            vec![json!({"id": "r1", "user_id": "u1", "role": "admin"})],
        );
        let wafer = build_wafer(db, Some(json!({"role": "admin"}))).await;

        // No auth.user_id set.
        let msg = Message::new("http.request");
        match wafer
            .run_block("wafer-run/iam-guard", msg, InputStream::empty())
            .await
            .collect_buffered()
            .await
        {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::UNAUTHENTICATED);
            }
            other => panic!("expected Unauthenticated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn required_role_defaults_to_admin_when_unconfigured() {
        let db = Arc::new(FakeDb::new());
        db.seed(
            "iam_user_roles",
            vec![json!({"id": "r1", "user_id": "u1", "role": "admin"})],
        );
        let wafer = build_wafer(db, None).await;

        let msg = with_user(Message::new("http.request"), "u1", "");
        match wafer
            .run_block("wafer-run/iam-guard", msg, InputStream::empty())
            .await
            .collect_buffered()
            .await
        {
            Err(TerminalNotResponse::Continue(_)) => {}
            other => panic!("expected Continue, got {other:?}"),
        }
    }
}
