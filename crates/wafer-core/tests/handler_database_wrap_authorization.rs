//! Task 5 exploit-shape tests — the database handler now authorizes every
//! op arm through `decode_and_authorize` (host-side `ctx.check_resource_access`)
//! instead of the caller-suppliable `wrap.resource` message meta.
//!
//! Before this change, `database.query_raw` and `database.exec_raw` had no
//! authorization call in the handler at all (only the runtime's own
//! meta-gated pre-dispatch check could catch them), and every typed op's
//! `check_wrap_resource` treated an *absent* `wrap.resource` meta as the
//! legacy "skip the check" path. A caller that never set WRAP meta — the
//! "meta-omission vector" — sailed straight through to the service. These
//! tests reconstruct that exact shape (WRAP metas absent on the message)
//! and assert the *ctx*, not the meta, is what gates the call: a denying
//! `Context` must produce `PermissionDenied` for every one of `query_raw`,
//! `exec_raw`, the new `database.ddl` op, and a foreign-collection
//! `list`/`create` — and, via a recording fake `DatabaseService`, that the
//! underlying service method never actually ran. A granting `Context` must
//! let the same requests through and reach the service.

use std::sync::Arc;

use wafer_block::{
    codec,
    common::ServiceOp,
    context::Context,
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    types::ResourceType,
    wire, ErrorCode, Message, WaferError,
};

mod common;

// Recording fake `DatabaseService`, the shared call log, and the
// meta-omission / assertion helpers now live in `tests/common/db_fakes.rs`,
// shared with `handler_database_schema_ops.rs` so both suites exercise the
// exact same fake.
use common::db_fakes::{self, expect_permission_denied, msg_without_wrap_meta, new_calls};

// ---------------------------------------------------------------------------
// Context fakes
// ---------------------------------------------------------------------------

/// `Context` stub that denies every resource-access check — models a caller
/// with no WRAP grant for anything, regardless of what (if any) meta the
/// message carries.
struct DenyCtx;

#[wafer_block::wafer_async_trait]
impl Context for DenyCtx {
    async fn call_block(
        &self,
        _block_name: &str,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn is_cancelled(&self) -> bool {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        unimplemented!("not exercised by decode_and_authorize")
    }

    // `check_resource_access` uses the trait's fail-closed default (deny).
}

/// `Context` stub that grants every resource-access check — models a caller
/// holding a valid WRAP grant for the resource it's requesting.
struct AllowCtx;

#[wafer_block::wafer_async_trait]
impl Context for AllowCtx {
    async fn call_block(
        &self,
        _block_name: &str,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn is_cancelled(&self) -> bool {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn check_resource_access(
        &self,
        _resource: &str,
        _resource_type: ResourceType,
        _is_write: bool,
    ) -> Result<(), WaferError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn expect_success(out: OutputStream) {
    if let Err(TerminalNotResponse::Error(e)) = out.collect_buffered().await {
        panic!("expected success, got error {:?}: {}", e.code, e.message);
    }
}

// ---------------------------------------------------------------------------
// DENY cases — meta absent, ctx denies. Assert PermissionDenied AND that
// the service op never ran.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn query_raw_denied_never_reaches_service() {
    let calls = new_calls();
    let svc = db_fakes::RecordingDb::new(calls.clone());
    let req = wire::database::QueryRawRequest {
        query: "SELECT * FROM my_org__auth__users".into(),
        args: vec![],
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::DATABASE_QUERY_RAW);

    let out =
        wafer_core::interfaces::database::handler::handle_message(&svc, &DenyCtx, &msg, &body)
            .await;
    expect_permission_denied(out).await;

    assert!(
        calls.lock().unwrap().is_empty(),
        "query_raw must not run on a denied request; calls = {:?}",
        calls.lock().unwrap()
    );
}

#[tokio::test]
async fn exec_raw_denied_never_reaches_service() {
    let calls = new_calls();
    let svc = db_fakes::RecordingDb::new(calls.clone());
    let req = wire::database::ExecRawRequest {
        query: "DELETE FROM my_org__auth__users".into(),
        args: vec![],
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::DATABASE_EXEC_RAW);

    let out =
        wafer_core::interfaces::database::handler::handle_message(&svc, &DenyCtx, &msg, &body)
            .await;
    expect_permission_denied(out).await;

    assert!(
        calls.lock().unwrap().is_empty(),
        "exec_raw must not run on a denied request; calls = {:?}",
        calls.lock().unwrap()
    );
}

#[tokio::test]
async fn ddl_denied_never_reaches_service() {
    let calls = new_calls();
    let svc = db_fakes::RecordingDb::new(calls.clone());
    // Even a caller that relabels a DDL statement as `database.ddl` directly
    // (the host-authoritative op, not something forgeable via meta) must
    // still be denied when it has no `__ddl__` grant.
    let req = wire::database::ExecRawRequest {
        query: "CREATE TABLE my_org__auth__evil (id TEXT)".into(),
        args: vec![],
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::DATABASE_DDL);

    let out =
        wafer_core::interfaces::database::handler::handle_message(&svc, &DenyCtx, &msg, &body)
            .await;
    expect_permission_denied(out).await;

    assert!(
        calls.lock().unwrap().is_empty(),
        "database.ddl must not run on a denied request; calls = {:?}",
        calls.lock().unwrap()
    );
}

#[tokio::test]
async fn foreign_collection_list_denied_never_reaches_service() {
    let calls = new_calls();
    let svc = db_fakes::RecordingDb::new(calls.clone());
    let req = wire::database::ListRequest {
        collection: "my_org__other_block__secrets".into(),
        filters: vec![],
        sort: vec![],
        limit: 10,
        offset: 0,
        skip_count: false,
        columns: None,
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::DATABASE_LIST);

    let out =
        wafer_core::interfaces::database::handler::handle_message(&svc, &DenyCtx, &msg, &body)
            .await;
    expect_permission_denied(out).await;

    assert!(
        calls.lock().unwrap().is_empty(),
        "list on a foreign collection must not run; calls = {:?}",
        calls.lock().unwrap()
    );
}

#[tokio::test]
async fn foreign_collection_create_denied_never_reaches_service() {
    let calls = new_calls();
    let svc = db_fakes::RecordingDb::new(calls.clone());
    let req = wire::database::CreateRequest {
        collection: "my_org__other_block__secrets".into(),
        data: Default::default(),
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::DATABASE_CREATE);

    let out =
        wafer_core::interfaces::database::handler::handle_message(&svc, &DenyCtx, &msg, &body)
            .await;
    expect_permission_denied(out).await;

    assert!(
        calls.lock().unwrap().is_empty(),
        "create on a foreign collection must not run; calls = {:?}",
        calls.lock().unwrap()
    );
}

#[tokio::test]
async fn foreign_collection_upsert_denied_never_reaches_service() {
    let calls = new_calls();
    let svc = db_fakes::RecordingDb::new(calls.clone());
    let req = wire::database::UpsertRequest {
        collection: "my_org__other_block__secrets".into(),
        data: vec![("id".into(), serde_json::json!("1"))],
        conflict_columns: vec!["id".into()],
        on_conflict: wire::database::OnConflict::SetColumns(vec!["id".into()]),
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::DATABASE_UPSERT);

    let out =
        wafer_core::interfaces::database::handler::handle_message(&svc, &DenyCtx, &msg, &body)
            .await;
    expect_permission_denied(out).await;

    assert!(
        calls.lock().unwrap().is_empty(),
        "upsert on a foreign collection must not run; calls = {:?}",
        calls.lock().unwrap()
    );
}

/// A granted `DATABASE_UPSERT` reaches the service (proves the handler arm
/// authorizes-then-dispatches rather than always short-circuiting).
#[tokio::test]
async fn granted_ctx_allows_upsert_reaches_service() {
    let calls = new_calls();
    let svc = db_fakes::RecordingDb::new(calls.clone());
    let req = wire::database::UpsertRequest {
        collection: "my_org__auth__users".into(),
        data: vec![
            ("id".into(), serde_json::json!("1")),
            ("name".into(), serde_json::json!("alice")),
        ],
        conflict_columns: vec!["id".into()],
        on_conflict: wire::database::OnConflict::SetColumns(vec!["name".into()]),
    };
    let body = codec::encode(&req).unwrap();

    let out = wafer_core::interfaces::database::handler::handle_message(
        &svc,
        &AllowCtx,
        &msg_without_wrap_meta(ServiceOp::DATABASE_UPSERT),
        &body,
    )
    .await;
    expect_success(out).await;

    assert_eq!(
        *calls.lock().unwrap(),
        vec!["upsert"],
        "a granted upsert should reach the service exactly once"
    );
}

/// A hostile identifier inside a `WindowedCounter` on_conflict is rejected as
/// `InvalidArgument` by `to_upsert_spec` — *before* the service runs — even
/// under a granting `Context`. This is the fail-closed guard on the column
/// names that the windowed-counter builder splices into `CASE`/`SET` text.
#[tokio::test]
async fn upsert_bad_identifier_in_windowed_counter_is_invalid_argument() {
    let calls = new_calls();
    let svc = db_fakes::RecordingDb::new(calls.clone());
    let req = wire::database::UpsertRequest {
        collection: "my_org__auth__users".into(),
        data: vec![
            ("id".into(), serde_json::json!("rl-1")),
            ("key".into(), serde_json::json!("user:1:login")),
        ],
        conflict_columns: vec!["key".into()],
        on_conflict: wire::database::OnConflict::WindowedCounter {
            count_field: "co unt".into(), // not a plain identifier
            window_field: "window_start".into(),
            now: 1000,
            window_cutoff: 940,
            created_fields: vec!["created_at".into()],
            updated_fields: vec!["updated_at".into()],
        },
    };
    let body = codec::encode(&req).unwrap();

    let out = wafer_core::interfaces::database::handler::handle_message(
        &svc,
        &AllowCtx,
        &msg_without_wrap_meta(ServiceOp::DATABASE_UPSERT),
        &body,
    )
    .await;
    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => assert_eq!(
            e.code,
            ErrorCode::InvalidArgument,
            "bad identifier must be InvalidArgument, got {:?}: {}",
            e.code,
            e.message
        ),
        other => panic!("expected an InvalidArgument error terminal, got {other:?}"),
    }
    assert!(
        calls.lock().unwrap().is_empty(),
        "a bad-identifier upsert must be rejected before reaching the service; calls = {:?}",
        calls.lock().unwrap()
    );
}

/// A `WindowedCounter` upsert missing the required string `id` data field is
/// rejected as `InvalidArgument` by `to_upsert_spec` — before the service
/// runs — rather than surfacing later as an opaque `Internal` error out of
/// `extract_windowed_id_key`.
#[tokio::test]
async fn upsert_windowed_counter_missing_id_is_invalid_argument() {
    let calls = new_calls();
    let svc = db_fakes::RecordingDb::new(calls.clone());
    let req = wire::database::UpsertRequest {
        collection: "my_org__auth__users".into(),
        data: vec![("key".into(), serde_json::json!("user:1:login"))],
        conflict_columns: vec!["key".into()],
        on_conflict: wire::database::OnConflict::WindowedCounter {
            count_field: "count".into(),
            window_field: "window_start".into(),
            now: 1000,
            window_cutoff: 940,
            created_fields: vec!["created_at".into()],
            updated_fields: vec!["updated_at".into()],
        },
    };
    let body = codec::encode(&req).unwrap();

    let out = wafer_core::interfaces::database::handler::handle_message(
        &svc,
        &AllowCtx,
        &msg_without_wrap_meta(ServiceOp::DATABASE_UPSERT),
        &body,
    )
    .await;
    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => assert_eq!(
            e.code,
            ErrorCode::InvalidArgument,
            "missing id must be InvalidArgument, got {:?}: {}",
            e.code,
            e.message
        ),
        other => panic!("expected an InvalidArgument error terminal, got {other:?}"),
    }
    assert!(
        calls.lock().unwrap().is_empty(),
        "a missing-id upsert must be rejected before reaching the service; calls = {:?}",
        calls.lock().unwrap()
    );
}

/// A `WindowedCounter` upsert missing the required string `key` data field is
/// rejected as `InvalidArgument` by `to_upsert_spec`.
#[tokio::test]
async fn upsert_windowed_counter_missing_key_is_invalid_argument() {
    let calls = new_calls();
    let svc = db_fakes::RecordingDb::new(calls.clone());
    let req = wire::database::UpsertRequest {
        collection: "my_org__auth__users".into(),
        data: vec![("id".into(), serde_json::json!("rl-1"))],
        conflict_columns: vec!["key".into()],
        on_conflict: wire::database::OnConflict::WindowedCounter {
            count_field: "count".into(),
            window_field: "window_start".into(),
            now: 1000,
            window_cutoff: 940,
            created_fields: vec!["created_at".into()],
            updated_fields: vec!["updated_at".into()],
        },
    };
    let body = codec::encode(&req).unwrap();

    let out = wafer_core::interfaces::database::handler::handle_message(
        &svc,
        &AllowCtx,
        &msg_without_wrap_meta(ServiceOp::DATABASE_UPSERT),
        &body,
    )
    .await;
    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => assert_eq!(
            e.code,
            ErrorCode::InvalidArgument,
            "missing key must be InvalidArgument, got {:?}: {}",
            e.code,
            e.message
        ),
        other => panic!("expected an InvalidArgument error terminal, got {other:?}"),
    }
    assert!(
        calls.lock().unwrap().is_empty(),
        "a missing-key upsert must be rejected before reaching the service; calls = {:?}",
        calls.lock().unwrap()
    );
}

/// A `WindowedCounter` upsert with empty `conflict_columns` is rejected as
/// `InvalidArgument` by `to_upsert_spec`, rather than the executor silently
/// defaulting the conflict target to the literal `"key"`.
#[tokio::test]
async fn upsert_windowed_counter_empty_conflict_columns_is_invalid_argument() {
    let calls = new_calls();
    let svc = db_fakes::RecordingDb::new(calls.clone());
    let req = wire::database::UpsertRequest {
        collection: "my_org__auth__users".into(),
        data: vec![
            ("id".into(), serde_json::json!("rl-1")),
            ("key".into(), serde_json::json!("user:1:login")),
        ],
        conflict_columns: vec![],
        on_conflict: wire::database::OnConflict::WindowedCounter {
            count_field: "count".into(),
            window_field: "window_start".into(),
            now: 1000,
            window_cutoff: 940,
            created_fields: vec!["created_at".into()],
            updated_fields: vec!["updated_at".into()],
        },
    };
    let body = codec::encode(&req).unwrap();

    let out = wafer_core::interfaces::database::handler::handle_message(
        &svc,
        &AllowCtx,
        &msg_without_wrap_meta(ServiceOp::DATABASE_UPSERT),
        &body,
    )
    .await;
    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => assert_eq!(
            e.code,
            ErrorCode::InvalidArgument,
            "empty conflict_columns must be InvalidArgument, got {:?}: {}",
            e.code,
            e.message
        ),
        other => panic!("expected an InvalidArgument error terminal, got {other:?}"),
    }
    assert!(
        calls.lock().unwrap().is_empty(),
        "an empty-conflict-columns upsert must be rejected before reaching the service; calls = {:?}",
        calls.lock().unwrap()
    );
}

// ---------------------------------------------------------------------------
// ALLOW case — granted ctx lets the request through to the service, for
// every op the DENY cases above cover.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn granted_ctx_allows_query_raw_exec_raw_ddl_and_typed_ops() {
    let calls = new_calls();
    let svc = db_fakes::RecordingDb::new(calls.clone());

    let query_raw_body = codec::encode(&wire::database::QueryRawRequest {
        query: "SELECT 1".into(),
        args: vec![],
    })
    .unwrap();
    expect_success(
        wafer_core::interfaces::database::handler::handle_message(
            &svc,
            &AllowCtx,
            &msg_without_wrap_meta(ServiceOp::DATABASE_QUERY_RAW),
            &query_raw_body,
        )
        .await,
    )
    .await;

    let exec_raw_body = codec::encode(&wire::database::ExecRawRequest {
        query: "DELETE FROM t".into(),
        args: vec![],
    })
    .unwrap();
    expect_success(
        wafer_core::interfaces::database::handler::handle_message(
            &svc,
            &AllowCtx,
            &msg_without_wrap_meta(ServiceOp::DATABASE_EXEC_RAW),
            &exec_raw_body,
        )
        .await,
    )
    .await;

    let ddl_body = codec::encode(&wire::database::ExecRawRequest {
        query: "CREATE TABLE my_org__auth__widgets (id TEXT)".into(),
        args: vec![],
    })
    .unwrap();
    expect_success(
        wafer_core::interfaces::database::handler::handle_message(
            &svc,
            &AllowCtx,
            &msg_without_wrap_meta(ServiceOp::DATABASE_DDL),
            &ddl_body,
        )
        .await,
    )
    .await;

    let list_body = codec::encode(&wire::database::ListRequest {
        collection: "my_org__auth__users".into(),
        filters: vec![],
        sort: vec![],
        limit: 10,
        offset: 0,
        skip_count: false,
        columns: None,
    })
    .unwrap();
    expect_success(
        wafer_core::interfaces::database::handler::handle_message(
            &svc,
            &AllowCtx,
            &msg_without_wrap_meta(ServiceOp::DATABASE_LIST),
            &list_body,
        )
        .await,
    )
    .await;

    let create_body = codec::encode(&wire::database::CreateRequest {
        collection: "my_org__auth__users".into(),
        data: Default::default(),
    })
    .unwrap();
    expect_success(
        wafer_core::interfaces::database::handler::handle_message(
            &svc,
            &AllowCtx,
            &msg_without_wrap_meta(ServiceOp::DATABASE_CREATE),
            &create_body,
        )
        .await,
    )
    .await;

    assert_eq!(
        *calls.lock().unwrap(),
        vec!["query_raw", "exec_raw", "exec_raw", "list", "create"],
        "every op should have reached the service exactly once, in order"
    );
}
