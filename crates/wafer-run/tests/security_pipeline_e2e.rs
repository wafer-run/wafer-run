//! Integration: compose the full security pipeline end-to-end through the
//! real Wafer runtime.
//!
//! Pipeline: auth-validator → iam-guard → ip-rate-limit → readonly-guard → handler
//!
//! Each test drives the same chain but varies one input (no token, wrong role,
//! readonly-write) to assert the right block stops the request.

use std::sync::Arc;

use serde_json::json;
use wafer_block::{
    common::ErrorCode,
    streams::{input::InputStream, output::TerminalNotResponse},
    Block, BlockCategory, BlockInfo, Context, InstanceMode, LifecycleEvent, Message, WaferError,
};
use wafer_block_auth_validator::AuthBlock;
use wafer_block_iam_guard::IAMBlock;
use wafer_block_ip_rate_limit::{RateLimitBlock, SystemClock};
use wafer_block_readonly_guard::ReadonlyGuardBlock;
use wafer_run::Wafer;
use wafer_test_support::{builder::WaferBuilder, fake_crypto::FakeCrypto, fake_db::FakeDb};

/// Always-200 handler used as the last step of the pipeline.
struct OkHandler;

#[async_trait::async_trait]
impl Block for OkHandler {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/ok-handler", "0.1.0", "handler@v1", "Always 200")
            .instance_mode(InstanceMode::Singleton)
            .category(BlockCategory::Infrastructure)
    }

    async fn handle(
        &self,
        _ctx: &dyn Context,
        _msg: Message,
        _input: InputStream,
    ) -> wafer_block::streams::output::OutputStream {
        wafer_block::streams::output::OutputStream::respond(b"{\"ok\":true}".to_vec())
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> Result<(), WaferError> {
        Ok(())
    }
}

fn seed_iam_role(db: &Arc<FakeDb>, user_id: &str, role: &str) {
    db.seed(
        "iam_user_roles",
        vec![json!({"id": format!("r-{user_id}"), "user_id": user_id, "role": role})],
    );
}

async fn build_pipeline(
    db: Arc<FakeDb>,
    crypto: Arc<FakeCrypto>,
    iam_role: &str,
    readonly: bool,
    rate_max: u32,
) -> Arc<Wafer> {
    WaferBuilder::new()
        .with_fake_db(db)
        .with_fake_crypto(crypto)
        .with_admin_block("root")
        .with_block("wafer-run/auth-validator", Arc::new(AuthBlock::new()))
        .with_block("wafer-run/iam-guard", Arc::new(IAMBlock::new()))
        .with_block(
            "wafer-run/ip-rate-limit",
            Arc::new(RateLimitBlock::with_clock(Arc::new(SystemClock))),
        )
        .with_block(
            "wafer-run/readonly-guard",
            Arc::new(ReadonlyGuardBlock::new()),
        )
        .with_block("test/ok-handler", Arc::new(OkHandler))
        .with_config("wafer-run/iam-guard", json!({"role": iam_role}))
        .with_config(
            "wafer-run/readonly-guard",
            json!({"readonly": if readonly { "true" } else { "false" }}),
        )
        .with_config(
            "wafer-run/ip-rate-limit",
            json!({"max_requests": rate_max.to_string(), "window_seconds": "60"}),
        )
        .build()
        .await
        .expect("build")
}

/// Chain the pipeline manually: each step's `Continue(msg)` becomes the next
/// step's input. Returns Ok if the handler responds; Err with the WaferError
/// from whichever step denied.
async fn run_pipeline(wafer: &Arc<Wafer>, initial: Message) -> Result<(), WaferError> {
    let pipeline = [
        "wafer-run/auth-validator",
        "wafer-run/iam-guard",
        "wafer-run/ip-rate-limit",
        "wafer-run/readonly-guard",
        "test/ok-handler",
    ];

    let mut msg = initial;
    let last = pipeline.len() - 1;
    for (i, name) in pipeline.iter().enumerate() {
        match wafer
            .run_block(name, msg.clone(), InputStream::empty())
            .await
            .collect_buffered()
            .await
        {
            Ok(_buf) => {
                if i == last {
                    return Ok(());
                }
                return Err(WaferError::new(
                    ErrorCode::INTERNAL,
                    format!("intermediate block {name} returned Respond"),
                ));
            }
            Err(TerminalNotResponse::Continue(continued)) => {
                msg = continued;
            }
            Err(TerminalNotResponse::Error(e)) => return Err(e),
            Err(other) => {
                return Err(WaferError::new(
                    ErrorCode::INTERNAL,
                    format!("unexpected terminal from {name}: {other:?}"),
                ));
            }
        }
    }
    Ok(())
}

async fn signed_jwt(wafer: &Arc<Wafer>, claims: serde_json::Value) -> String {
    match wafer
        .run_block(
            "wafer-run/crypto",
            Message::new("crypto.jwt_sign"),
            InputStream::from_bytes(serde_json::to_vec(&json!({"claims": claims})).unwrap()),
        )
        .await
        .collect_buffered()
        .await
    {
        Ok(buf) => {
            let resp: serde_json::Value = serde_json::from_slice(&buf.body).unwrap();
            resp["token"].as_str().unwrap().to_string()
        }
        other => panic!("sign failed: {other:?}"),
    }
}

#[tokio::test]
async fn happy_path_authed_read_succeeds() {
    std::env::remove_var("RATE_LIMIT_IP");
    let db = Arc::new(FakeDb::new());
    seed_iam_role(&db, "u1", "admin");
    let crypto = Arc::new(FakeCrypto::new());
    let wafer = build_pipeline(db, crypto.clone(), "admin", false, 1000).await;
    let token = signed_jwt(
        &wafer,
        json!({"sub": "u1", "email": "a@b.c", "roles": ["admin"]}),
    )
    .await;

    let mut msg = Message::new("retrieve");
    msg.set_meta("http.header.authorization", format!("Bearer {token}"));
    msg.set_meta("req.client.ip", "9.9.9.9");
    msg.set_meta("req.action", "retrieve");

    assert!(run_pipeline(&wafer, msg).await.is_ok());
}

#[tokio::test]
async fn unauthenticated_request_stops_at_auth() {
    std::env::remove_var("RATE_LIMIT_IP");
    let db = Arc::new(FakeDb::new());
    let crypto = Arc::new(FakeCrypto::new());
    let wafer = build_pipeline(db, crypto, "admin", false, 1000).await;

    let mut msg = Message::new("retrieve");
    msg.set_meta("req.client.ip", "9.9.9.9");
    msg.set_meta("req.action", "retrieve");
    let err = run_pipeline(&wafer, msg)
        .await
        .expect_err("pipeline should deny");
    assert_eq!(err.code, ErrorCode::UNAUTHENTICATED);
}

#[tokio::test]
async fn authed_wrong_role_stops_at_iam() {
    std::env::remove_var("RATE_LIMIT_IP");
    let db = Arc::new(FakeDb::new());
    seed_iam_role(&db, "u1", "viewer"); // user has viewer; guard requires admin
    let crypto = Arc::new(FakeCrypto::new());
    let wafer = build_pipeline(db, crypto.clone(), "admin", false, 1000).await;
    let token = signed_jwt(&wafer, json!({"sub": "u1", "roles": ["viewer"]})).await;

    let mut msg = Message::new("retrieve");
    msg.set_meta("http.header.authorization", format!("Bearer {token}"));
    msg.set_meta("req.client.ip", "9.9.9.9");
    msg.set_meta("req.action", "retrieve");
    let err = run_pipeline(&wafer, msg)
        .await
        .expect_err("pipeline should deny");
    assert_eq!(err.code, ErrorCode::PERMISSION_DENIED);
}

#[tokio::test]
async fn readonly_mode_blocks_writes_through_pipeline() {
    std::env::remove_var("RATE_LIMIT_IP");
    let db = Arc::new(FakeDb::new());
    seed_iam_role(&db, "u1", "admin");
    let crypto = Arc::new(FakeCrypto::new());
    let wafer = build_pipeline(db, crypto.clone(), "admin", true, 1000).await;
    let token = signed_jwt(&wafer, json!({"sub": "u1", "roles": ["admin"]})).await;

    let mut msg = Message::new("create");
    msg.set_meta("http.header.authorization", format!("Bearer {token}"));
    msg.set_meta("req.client.ip", "9.9.9.9");
    msg.set_meta("req.action", "create");
    let err = run_pipeline(&wafer, msg)
        .await
        .expect_err("pipeline should deny");
    assert_eq!(err.code, ErrorCode::PERMISSION_DENIED);
    assert!(err.message.to_lowercase().contains("read-only"));
}
