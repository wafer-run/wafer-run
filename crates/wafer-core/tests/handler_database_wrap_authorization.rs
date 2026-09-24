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
    types::{ResourceAccess, ResourceType},
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
    // Denies every access, as the trait's default `check_resource_access` does.
    fn resource_access_admitted(
        &self,
        _resource: &str,
        _resource_type: wafer_block::types::ResourceType,
        _access: wafer_block::types::ResourceAccess,
    ) -> bool {
        false
    }
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
        _access: ResourceAccess,
    ) -> Result<(), WaferError> {
        Ok(())
    }
    fn resource_access_admitted(
        &self,
        _resource: &str,
        _resource_type: ResourceType,
        _access: ResourceAccess,
    ) -> bool {
        true
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
        limit: Some(10),
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
/// runs.
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

/// A `WindowedCounter` upsert missing the conflict column's value (here
/// `key`) in its data is rejected as `InvalidArgument` by `to_upsert_spec`.
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
/// `InvalidArgument` by `to_upsert_spec`: the counter has no key to conflict
/// on.
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

/// Send a `WindowedCounter` upsert with `data` and `conflict_columns` through
/// the handler under a granting context; return the error code (if any) and
/// the service calls it made.
async fn windowed_counter_through_handler(
    data: Vec<(String, serde_json::Value)>,
    conflict_columns: Vec<String>,
) -> (Option<ErrorCode>, Vec<&'static str>) {
    let calls = new_calls();
    let svc = db_fakes::RecordingDb::new(calls.clone());
    let req = wire::database::UpsertRequest {
        collection: "my_org__auth__users".into(),
        data,
        conflict_columns,
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
    let code = match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => Some(e.code),
        _ => None,
    };
    let recorded = calls.lock().unwrap().clone();
    (code, recorded)
}

/// The counter is keyed by the column `conflict_columns` names, not by a
/// data field literally called `key`: a counter keyed by `ip` whose data is
/// `{id, ip}` reaches the service.
#[tokio::test]
async fn upsert_windowed_counter_keyed_by_another_column_reaches_the_service() {
    let (code, calls) = windowed_counter_through_handler(
        vec![
            ("id".into(), serde_json::json!("rl-1")),
            ("ip".into(), serde_json::json!("10.0.0.1")),
        ],
        vec!["ip".into()],
    )
    .await;
    assert_eq!(code, None, "a counter keyed by `ip` is a valid request");
    assert_eq!(calls, vec!["upsert"]);
}

/// Every part of a `WindowedCounter` request the statement would not write is
/// `InvalidArgument` before the service runs: a data field other than `id`
/// and the conflict column, a second conflict column, or `id` as the
/// counter's key.
#[tokio::test]
async fn upsert_windowed_counter_parts_it_would_not_write_are_invalid_argument() {
    let pair = |k: &str, v: &str| (k.to_string(), serde_json::json!(v));
    for (what, data, conflict) in [
        (
            "an extra data field",
            vec![pair("id", "rl-1"), pair("key", "a"), pair("ip", "b")],
            vec!["ip".to_string()],
        ),
        (
            "a second conflict column",
            vec![pair("id", "rl-1"), pair("ip", "b"), pair("key", "a")],
            vec!["ip".to_string(), "key".to_string()],
        ),
        (
            "`id` as the conflict column",
            vec![pair("id", "rl-1")],
            vec!["id".to_string()],
        ),
        (
            "the conflict value named twice",
            vec![pair("id", "rl-1"), pair("key", "a"), pair("key", "b")],
            vec!["key".to_string()],
        ),
    ] {
        let (code, calls) = windowed_counter_through_handler(data, conflict).await;
        assert_eq!(code, Some(ErrorCode::InvalidArgument), "{what}");
        assert!(calls.is_empty(), "{what} reached the service: {calls:?}");
    }
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
        limit: Some(10),
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

// ---------------------------------------------------------------------------
// Multi-row / multi-op writes — `database.create_many` and `database.batch`.
// ---------------------------------------------------------------------------

/// `Context` stub that grants access to the caller's own collections
/// (`my_org__auth__*`), for read and write, and denies everything else —
/// models a block holding a WRAP grant on its own namespace only.
struct OwnNamespaceCtx;

#[wafer_block::wafer_async_trait]
impl Context for OwnNamespaceCtx {
    async fn call_block(
        &self,
        _block_name: &str,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        unimplemented!("not exercised by the database handler")
    }

    fn is_cancelled(&self) -> bool {
        unimplemented!("not exercised by the database handler")
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        unimplemented!("not exercised by the database handler")
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        unimplemented!("not exercised by the database handler")
    }

    fn check_resource_access(
        &self,
        resource: &str,
        _resource_type: ResourceType,
        _access: ResourceAccess,
    ) -> Result<(), WaferError> {
        if resource.starts_with("my_org__auth__") {
            Ok(())
        } else {
            Err(WaferError::new(
                ErrorCode::PermissionDenied,
                format!("no grant for {resource}"),
            ))
        }
    }
    fn resource_access_admitted(
        &self,
        resource: &str,
        resource_type: ResourceType,
        access: ResourceAccess,
    ) -> bool {
        self.check_resource_access(resource, resource_type, access)
            .is_ok()
    }
}

/// `Context` stub that grants every READ and denies every write.
struct ReadOnlyCtx;

#[wafer_block::wafer_async_trait]
impl Context for ReadOnlyCtx {
    async fn call_block(
        &self,
        _block_name: &str,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        unimplemented!("not exercised by the database handler")
    }

    fn is_cancelled(&self) -> bool {
        unimplemented!("not exercised by the database handler")
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        unimplemented!("not exercised by the database handler")
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        unimplemented!("not exercised by the database handler")
    }

    fn check_resource_access(
        &self,
        resource: &str,
        _resource_type: ResourceType,
        access: ResourceAccess,
    ) -> Result<(), WaferError> {
        if access != ResourceAccess::Read {
            Err(WaferError::new(
                ErrorCode::PermissionDenied,
                format!("read-only grant on {resource}"),
            ))
        } else {
            Ok(())
        }
    }

    fn resource_access_admitted(
        &self,
        resource: &str,
        resource_type: ResourceType,
        access: ResourceAccess,
    ) -> bool {
        self.check_resource_access(resource, resource_type, access)
            .is_ok()
    }
}

fn own_and_foreign_batch() -> Vec<u8> {
    codec::encode(&wire::database::BatchRequest {
        ops: vec![
            wire::database::BatchWrite::Create {
                collection: "my_org__auth__users".into(),
                data: Default::default(),
            },
            wire::database::BatchWrite::Delete {
                collection: "my_org__other_block__secrets".into(),
                id: "1".into(),
            },
        ],
    })
    .unwrap()
}

/// A batch is authorized op by op: an own-namespace write FIRST does not let a
/// foreign-collection write ride along behind it. Nothing runs.
#[tokio::test]
async fn batch_with_one_foreign_collection_is_denied_and_never_reaches_service() {
    let calls = new_calls();
    let svc = db_fakes::RecordingDb::new(calls.clone());

    let out = wafer_core::interfaces::database::handler::handle_message(
        &svc,
        &OwnNamespaceCtx,
        &msg_without_wrap_meta(ServiceOp::DATABASE_BATCH),
        &own_and_foreign_batch(),
    )
    .await;
    expect_permission_denied(out).await;
    assert!(
        calls.lock().unwrap().is_empty(),
        "no op of a partly-foreign batch may run; calls = {:?}",
        calls.lock().unwrap()
    );
}

/// Every op of a batch is a write: a read-only grant on the collections
/// does not authorize one.
#[tokio::test]
async fn batch_is_authorized_as_a_write() {
    let calls = new_calls();
    let svc = db_fakes::RecordingDb::new(calls.clone());
    let body = codec::encode(&wire::database::BatchRequest {
        ops: vec![wire::database::BatchWrite::Update {
            collection: "my_org__auth__users".into(),
            id: "1".into(),
            data: Default::default(),
        }],
    })
    .unwrap();

    let out = wafer_core::interfaces::database::handler::handle_message(
        &svc,
        &ReadOnlyCtx,
        &msg_without_wrap_meta(ServiceOp::DATABASE_BATCH),
        &body,
    )
    .await;
    expect_permission_denied(out).await;
    assert!(calls.lock().unwrap().is_empty());
}

/// A batch confined to the caller's own collections reaches the service once.
#[tokio::test]
async fn batch_on_own_collections_reaches_service() {
    let calls = new_calls();
    let svc = db_fakes::RecordingDb::new(calls.clone());
    let body = codec::encode(&wire::database::BatchRequest {
        ops: vec![
            wire::database::BatchWrite::Create {
                collection: "my_org__auth__users".into(),
                data: Default::default(),
            },
            wire::database::BatchWrite::Delete {
                collection: "my_org__auth__sessions".into(),
                id: "1".into(),
            },
        ],
    })
    .unwrap();

    expect_success(
        wafer_core::interfaces::database::handler::handle_message(
            &svc,
            &OwnNamespaceCtx,
            &msg_without_wrap_meta(ServiceOp::DATABASE_BATCH),
            &body,
        )
        .await,
    )
    .await;
    assert_eq!(*calls.lock().unwrap(), vec!["batch"]);
}

/// `create_many` is authorized as a write on its collection.
#[tokio::test]
async fn create_many_is_authorized_as_a_write_on_its_collection() {
    let calls = new_calls();
    let svc = db_fakes::RecordingDb::new(calls.clone());
    let foreign = codec::encode(&wire::database::CreateManyRequest {
        collection: "my_org__other_block__secrets".into(),
        rows: vec![Default::default()],
    })
    .unwrap();
    let own = codec::encode(&wire::database::CreateManyRequest {
        collection: "my_org__auth__users".into(),
        rows: vec![Default::default()],
    })
    .unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::DATABASE_CREATE_MANY);

    expect_permission_denied(
        wafer_core::interfaces::database::handler::handle_message(
            &svc,
            &OwnNamespaceCtx,
            &msg,
            &foreign,
        )
        .await,
    )
    .await;
    expect_permission_denied(
        wafer_core::interfaces::database::handler::handle_message(&svc, &ReadOnlyCtx, &msg, &own)
            .await,
    )
    .await;
    assert!(calls.lock().unwrap().is_empty());

    expect_success(
        wafer_core::interfaces::database::handler::handle_message(
            &svc,
            &OwnNamespaceCtx,
            &msg,
            &own,
        )
        .await,
    )
    .await;
    assert_eq!(*calls.lock().unwrap(), vec!["create_many"]);
}

// ---------------------------------------------------------------------------
// Guarded writes — `database.insert_guarded` and `database.update_guarded`.
// ---------------------------------------------------------------------------

fn guarded_bodies(
    collection: &str,
    guards: Vec<wire::database::CapGuard>,
) -> [(&'static str, Vec<u8>); 2] {
    [
        (
            ServiceOp::DATABASE_INSERT_GUARDED,
            codec::encode(&wire::database::InsertGuardedRequest {
                collection: collection.into(),
                data: Default::default(),
                guards: guards.clone(),
            })
            .unwrap(),
        ),
        (
            ServiceOp::DATABASE_UPDATE_GUARDED,
            codec::encode(&wire::database::UpdateGuardedRequest {
                collection: collection.into(),
                filters: Vec::new(),
                data: Default::default(),
                guards,
            })
            .unwrap(),
        ),
    ]
}

/// Both guarded ops are writes on their collection: a foreign collection and
/// a read-only grant are refused before the service runs; an own collection
/// reaches it.
#[tokio::test]
async fn guarded_writes_are_authorized_as_writes_on_their_collection() {
    for ((op, foreign), (_, own)) in guarded_bodies("my_org__other_block__secrets", Vec::new())
        .into_iter()
        .zip(guarded_bodies("my_org__auth__users", Vec::new()))
    {
        let calls = new_calls();
        let svc = db_fakes::RecordingDb::new(calls.clone());
        let msg = msg_without_wrap_meta(op);
        expect_permission_denied(
            wafer_core::interfaces::database::handler::handle_message(
                &svc,
                &OwnNamespaceCtx,
                &msg,
                &foreign,
            )
            .await,
        )
        .await;
        expect_permission_denied(
            wafer_core::interfaces::database::handler::handle_message(
                &svc,
                &ReadOnlyCtx,
                &msg,
                &own,
            )
            .await,
        )
        .await;
        assert!(calls.lock().unwrap().is_empty(), "{op}");

        expect_success(
            wafer_core::interfaces::database::handler::handle_message(
                &svc,
                &OwnNamespaceCtx,
                &msg,
                &own,
            )
            .await,
        )
        .await;
        assert_eq!(calls.lock().unwrap().len(), 1, "{op}");
    }
}

/// A malformed guard — a `SumAtMost` field that is not a plain identifier, a
/// filter group, or more than `MAX_WRITE_GUARDS` guards — is
/// `InvalidArgument` before the service runs.
#[tokio::test]
async fn malformed_guards_are_invalid_argument_before_the_service_runs() {
    let leaf = |field: &str| {
        wire::database::FilterNode::Leaf(wire::database::FilterDef {
            field: field.into(),
            operator: "eq".into(),
            value: serde_json::json!("u"),
            column: None,
        })
    };
    let count_below = || wire::database::CapGuard::CountBelow {
        filters: Vec::new(),
        cap: 1,
    };
    let malformed = [
        vec![wire::database::CapGuard::SumAtMost {
            field: "size\"); DROP TABLE x; --".into(),
            filters: Vec::new(),
            add: 1,
            cap: 1,
        }],
        vec![wire::database::CapGuard::CountBelow {
            filters: vec![wire::database::FilterNode::Any {
                any: vec![leaf("a"), leaf("b")],
            }],
            cap: 1,
        }],
        (0..=wire::database::MAX_WRITE_GUARDS)
            .map(|_| count_below())
            .collect(),
    ];
    for guards in malformed {
        for (op, body) in guarded_bodies("my_org__auth__users", guards.clone()) {
            let calls = new_calls();
            let svc = db_fakes::RecordingDb::new(calls.clone());
            let out = wafer_core::interfaces::database::handler::handle_message(
                &svc,
                &OwnNamespaceCtx,
                &msg_without_wrap_meta(op),
                &body,
            )
            .await;
            match out.collect_buffered().await {
                Err(TerminalNotResponse::Error(e)) => {
                    assert_eq!(e.code, ErrorCode::InvalidArgument, "{op}: {}", e.message);
                }
                other => panic!("{op}: expected InvalidArgument, got {other:?}"),
            }
            assert!(calls.lock().unwrap().is_empty(), "{op}");
        }
    }
}
