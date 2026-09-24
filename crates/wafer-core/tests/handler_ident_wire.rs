//! The database handler admits only plain lowercase identifiers
//! (`[a-z0-9_]`) as collection and column names, and refuses a collection
//! that is not one BEFORE it authorizes the caller on it: no access check
//! runs on a name the executor would not use verbatim, and the service is
//! never reached. Request bytes are encoded from plain JSON (what any peer
//! puts on the wire).

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use wafer_block::{
    codec,
    common::ServiceOp,
    context::Context,
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    types::{ResourceAccess, ResourceType},
    ErrorCode, Message, WaferError,
};
use wafer_core::interfaces::database::handler::handle_message;

mod common;

use common::db_fakes::{new_calls, RecordingDb};

/// A `Context` that records every access check it is asked, and admits all
/// of them when `admit` is set (denies all otherwise).
struct RecordingCtx {
    admit: bool,
    checks: Mutex<Vec<String>>,
}

impl RecordingCtx {
    fn new(admit: bool) -> Self {
        Self {
            admit,
            checks: Mutex::new(Vec::new()),
        }
    }
}

#[wafer_block::wafer_async_trait]
impl Context for RecordingCtx {
    async fn call_block(
        &self,
        _block_name: &str,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        unimplemented!("the database handler makes no block calls")
    }
    fn is_cancelled(&self) -> bool {
        false
    }
    fn config_get(&self, _key: &str) -> Option<&str> {
        None
    }
    fn clone_arc(&self) -> Arc<dyn Context> {
        unimplemented!("the database handler does not clone its context")
    }
    fn check_resource_access(
        &self,
        resource: &str,
        _resource_type: ResourceType,
        _access: ResourceAccess,
    ) -> Result<(), WaferError> {
        self.checks.lock().unwrap().push(resource.to_string());
        if self.admit {
            Ok(())
        } else {
            Err(WaferError::new(ErrorCode::PermissionDenied, "denied"))
        }
    }
    fn resource_access_admitted(
        &self,
        _resource: &str,
        _resource_type: ResourceType,
        _access: ResourceAccess,
    ) -> bool {
        self.admit
    }
}

/// Dispatch `request` for `op`, returning the error code it ended with (or
/// `None` when it succeeded), the access checks the handler ran and the
/// service methods that ran.
async fn run(admit: bool, op: &str, request: &Value) -> (Option<ErrorCode>, Vec<String>, usize) {
    let calls = new_calls();
    let svc = RecordingDb::new(calls.clone());
    let ctx = RecordingCtx::new(admit);
    let body = codec::encode(request).expect("encode request");
    let code = match handle_message(&svc, &ctx, &Message::new(op), &body)
        .await
        .collect_buffered()
        .await
    {
        Ok(_) => None,
        Err(TerminalNotResponse::Error(e)) => Some(e.code),
        Err(other) => panic!("{op}: no response or error: {other:?}"),
    };
    let checks = ctx.checks.lock().unwrap().clone();
    let ran = calls.lock().unwrap().len();
    (code, checks, ran)
}

/// Collection spellings that are not plain lowercase identifiers. Each would
/// have been authorized as written and then reached another table: by
/// character stripping (`acme__ab__t`), by case folding on SQLite, or — the
/// 64-byte name — by PostgreSQL keeping only its first 63 bytes.
fn bad_collections() -> Vec<String> {
    vec![
        "acme__a-b__t".to_string(),
        "Acme__ab__t".to_string(),
        "acme__ab__t;".to_string(),
        "acme__ab__caf\u{e9}".to_string(),
        String::new(),
        format!("acme__ab__{}", "t".repeat(55)),
    ]
}

#[tokio::test]
async fn a_non_identifier_collection_is_refused_before_any_access_check() {
    let mut wrong = Vec::new();
    for bad in bad_collections() {
        let bad = bad.as_str();
        let requests = [
            (ServiceOp::DATABASE_LIST, json!({ "collection": bad })),
            (
                ServiceOp::DATABASE_GET,
                json!({ "collection": bad, "id": "x" }),
            ),
            (
                ServiceOp::DATABASE_CREATE,
                json!({ "collection": bad, "data": { "name": "n" } }),
            ),
            (
                ServiceOp::DATABASE_BATCH,
                json!({ "ops": [
                    { "Create": { "collection": "acme__ab__t", "data": { "name": "n" } } },
                    { "Delete": { "collection": bad, "id": "x" } },
                ] }),
            ),
            (ServiceOp::DATABASE_DROP_TABLE, json!({ "table": bad })),
            (
                ServiceOp::DATABASE_ENSURE_TABLE,
                json!({ "table": { "name": bad, "columns": [
                    { "name": "id", "kind": "text", "primary_key": true },
                ] } }),
            ),
        ];
        // Both a denying and an admitting context: the refusal must not
        // depend on the caller's grants, and no check may run first.
        for admit in [false, true] {
            for (op, request) in &requests {
                let (code, checks, ran) = run(admit, op, request).await;
                if code != Some(ErrorCode::InvalidArgument) || !checks.is_empty() || ran != 0 {
                    wrong.push(format!(
                        "{op} {bad:?} (admit={admit}): code={code:?} checks={checks:?} ran={ran}"
                    ));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[tokio::test]
async fn a_non_identifier_column_is_refused_and_never_reaches_the_service() {
    let t = "acme__ab__t";
    let bad_filter = |field: &str| json!([{ "field": field, "value": 1 }]);
    let mut wrong = Vec::new();
    let long = "n".repeat(64);
    for bad in ["na-me", "Name", "name;", "", long.as_str()] {
        let requests = [
            (
                "list sort",
                ServiceOp::DATABASE_LIST,
                json!({ "collection": t, "sort": [{ "field": bad }] }),
            ),
            (
                "list filter",
                ServiceOp::DATABASE_LIST,
                json!({ "collection": t, "filters": bad_filter(bad) }),
            ),
            (
                "list columns",
                ServiceOp::DATABASE_LIST,
                json!({ "collection": t, "columns": [bad] }),
            ),
            (
                "count filter",
                ServiceOp::DATABASE_COUNT,
                json!({ "collection": t, "filters": bad_filter(bad) }),
            ),
            (
                "sum field",
                ServiceOp::DATABASE_SUM,
                json!({ "collection": t, "field": bad }),
            ),
            (
                "create data",
                ServiceOp::DATABASE_CREATE,
                json!({ "collection": t, "data": { bad: 1 } }),
            ),
            (
                "update data",
                ServiceOp::DATABASE_UPDATE,
                json!({ "collection": t, "id": "x", "data": { bad: 1 } }),
            ),
            (
                "update_where data",
                ServiceOp::DATABASE_UPDATE_WHERE,
                json!({ "collection": t, "filters": [], "data": { bad: 1 } }),
            ),
            (
                "increment col",
                ServiceOp::DATABASE_INCREMENT_FIELD_WHERE,
                json!({ "collection": t, "col": bad, "delta": 1, "filters": [] }),
            ),
            (
                "insert_guarded guard filter",
                ServiceOp::DATABASE_INSERT_GUARDED,
                json!({ "collection": t, "data": { "name": "n" }, "guards": [
                    { "CountBelow": { "filters": bad_filter(bad), "cap": 1 } },
                ] }),
            ),
            (
                "batch Update data",
                ServiceOp::DATABASE_BATCH,
                json!({ "ops": [{ "Update": { "collection": t, "id": "x", "data": { bad: 1 } } }] }),
            ),
            (
                "ensure_table column",
                ServiceOp::DATABASE_ENSURE_TABLE,
                json!({ "table": { "name": t, "columns": [
                    { "name": bad, "kind": "text", "primary_key": true },
                ] } }),
            ),
            (
                "add_column column",
                ServiceOp::DATABASE_ADD_COLUMN,
                json!({ "table": t, "column": { "name": bad, "kind": "text", "nullable": true } }),
            ),
        ];
        for (label, op, request) in &requests {
            let (code, _checks, ran) = run(true, op, request).await;
            if code != Some(ErrorCode::InvalidArgument) || ran != 0 {
                wrong.push(format!("{label} {bad:?}: code={code:?} ran={ran}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[tokio::test]
async fn plain_lowercase_names_reach_the_service() {
    // Guard the two tests above against passing for the wrong reason: the
    // same requests with plain names are admitted and served.
    let t = "acme__ab__t";
    for (op, request) in [
        (
            ServiceOp::DATABASE_LIST,
            json!({ "collection": t, "sort": [{ "field": "name_2" }], "columns": ["name_2"] }),
        ),
        (
            ServiceOp::DATABASE_CREATE,
            json!({ "collection": t, "data": { "name_2": 1 } }),
        ),
    ] {
        let (code, checks, ran) = run(true, op, &request).await;
        assert_eq!(code, None, "{op}");
        assert!(checks.iter().any(|c| c == t), "{op}: {checks:?}");
        assert_eq!(ran, 1, "{op}");
    }
}
