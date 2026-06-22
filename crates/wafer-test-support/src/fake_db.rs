//! In-memory database fake implementing the `database@v1` interface.

use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;
use wafer_block::{
    common::ErrorCode,
    db::FilterOp,
    streams::{input::InputStream, output::OutputStream},
    Block, BlockCategory, BlockInfo, Context, InstanceMode, LifecycleEvent, Message, WaferError,
};

/// Controls how the fake behaves when dispatched.
#[derive(Debug, Clone, Copy)]
pub enum FailureMode {
    /// Fake handles requests normally.
    None,
    /// Every request returns `ErrorCode::Internal`.
    Unavailable,
    /// Next `N` requests fail, then reset to `None`.
    FailNextCall(u32),
}

pub(crate) struct FakeDbState {
    pub collections: HashMap<String, Vec<serde_json::Value>>,
    pub failure: FailureMode,
}

/// In-memory database fake.
///
/// Implements `database@v1`'s `database.get`, `database.list`, `database.create`,
/// `database.update`, `database.delete`, `database.count` actions. Any other
/// action returns `InvalidArgument` so fixture gaps surface loudly.
///
/// Filter operators are parsed with [`FilterOp::parse_wire`] — the same parser
/// the production database handler uses — so unknown spellings are rejected
/// with `InvalidArgument` exactly like a real backend. Only `Equal` matching
/// is implemented; other (valid) operators also return `InvalidArgument`
/// rather than silently matching nothing.
pub struct FakeDb {
    pub(crate) state: Arc<Mutex<FakeDbState>>,
}

impl Default for FakeDb {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeDb {
    /// Build an empty fake with no collections and no failure injection.
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeDbState {
                collections: HashMap::new(),
                failure: FailureMode::None,
            })),
        }
    }

    /// Insert rows into a collection. Does not touch failure mode.
    ///
    /// Rows are stored as-is and converted to the production wire format
    /// (`Record { id, data }` and `RecordList`) at dispatch time. So a seed
    /// like `json!({"id": "u1", "name": "Alice"})` will be returned by
    /// `database.list` as `{"records": [{"id": "u1", "data": {"name": "Alice"}}], ...}`.
    /// See `to_record` for the exact mapping.
    pub fn seed(&self, collection: &str, rows: Vec<serde_json::Value>) {
        self.state
            .lock()
            .collections
            .entry(collection.to_string())
            .or_default()
            .extend(rows);
    }

    /// Switch the fake into a failure mode so subsequent dispatches return
    /// `ErrorCode::Internal` — used to exercise caller error handling.
    pub fn set_failure(&self, mode: FailureMode) {
        self.state.lock().failure = mode;
    }

    /// Drop all seeded collections and reset failure mode to `None`.
    pub fn clear(&self) {
        let mut s = self.state.lock();
        s.collections.clear();
        s.failure = FailureMode::None;
    }

    /// Returns true and decrements a pending `FailNextCall` counter, if one is active,
    /// or signals Unavailable. Returns false when requests should proceed.
    fn should_fail(&self) -> bool {
        let mut s = self.state.lock();
        match s.failure {
            FailureMode::None => false,
            FailureMode::Unavailable => true,
            FailureMode::FailNextCall(n) => {
                if n <= 1 {
                    s.failure = FailureMode::None;
                } else {
                    s.failure = FailureMode::FailNextCall(n - 1);
                }
                true
            }
        }
    }
}

#[async_trait::async_trait]
impl Block for FakeDb {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test/fake-db",
            "0.1.0",
            "database@v1",
            "In-memory database fake for tests",
        )
        .instance_mode(InstanceMode::Singleton)
        .category(BlockCategory::Infrastructure)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        if self.should_fail() {
            return OutputStream::error(WaferError::new(
                ErrorCode::Internal,
                "fake-db unavailable",
            ));
        }

        let body = input.collect_to_bytes().await;
        let req: serde_json::Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                return OutputStream::error(WaferError::new(
                    ErrorCode::InvalidArgument,
                    format!("fake-db: bad request: {e}"),
                ));
            }
        };

        match msg.action() {
            "database.list" => self.handle_list(&req),
            "database.get" => self.handle_get(&req),
            "database.create" => self.handle_create(&req),
            "database.update" => self.handle_update(&req),
            "database.delete" => self.handle_delete(&req),
            "database.count" => self.handle_count(&req),
            other => OutputStream::error(WaferError::new(
                ErrorCode::InvalidArgument,
                format!("fake-db: action '{other}' not implemented"),
            )),
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

/// Convert a flat seeded row `{id, field1, field2, ...}` to the wire format
/// `{id: String, data: {field1, field2, ...}}` expected by `Record`.
///
/// The `id` field is extracted to the top level; all other fields go into `data`.
fn to_record(row: &serde_json::Value) -> serde_json::Value {
    let id = row
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let data: serde_json::Map<String, serde_json::Value> = row
        .as_object()
        .map(|obj| {
            obj.iter()
                .filter(|(k, _)| k.as_str() != "id")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default();
    serde_json::json!({ "id": id, "data": data })
}

impl FakeDb {
    fn handle_list(&self, req: &serde_json::Value) -> OutputStream {
        let collection = req["collection"].as_str().unwrap_or("");
        let filters = req["filters"].as_array().cloned().unwrap_or_default();
        let limit = req["limit"].as_i64().unwrap_or(i64::MAX).max(0) as usize;
        let state = self.state.lock();
        let empty = Vec::new();
        let rows = state.collections.get(collection).unwrap_or(&empty);
        let matching = match matching_rows(rows, &filters) {
            Ok(m) => m,
            Err(e) => return OutputStream::error(e),
        };
        let total_count = matching.len() as i64;
        // Convert to `{id, data}` wire format expected by `RecordList`.
        let records: Vec<serde_json::Value> =
            matching.into_iter().take(limit).map(to_record).collect();
        let body = serde_json::to_vec(&serde_json::json!({
            "records": records,
            "total_count": total_count,
            "page": 0_i64,
            "page_size": limit as i64,
        }))
        .unwrap();
        OutputStream::respond(body)
    }

    fn handle_get(&self, req: &serde_json::Value) -> OutputStream {
        let collection = req["collection"].as_str().unwrap_or("");
        let id = req["id"].as_str().unwrap_or("");
        let state = self.state.lock();
        let empty = Vec::new();
        let rows = state.collections.get(collection).unwrap_or(&empty);
        let found = rows.iter().find(|r| r["id"].as_str() == Some(id)).cloned();
        match found {
            Some(row) => {
                // Convert to `{id, data}` wire format expected by `Record`.
                let body = serde_json::to_vec(&to_record(&row)).unwrap();
                OutputStream::respond(body)
            }
            None => OutputStream::error(WaferError::new(
                ErrorCode::NotFound,
                format!("fake-db: {collection}/{id} not found"),
            )),
        }
    }

    fn handle_create(&self, req: &serde_json::Value) -> OutputStream {
        let collection = req["collection"].as_str().unwrap_or("").to_string();
        // `data` must be a JSON object — `serde_json`'s `IndexMut<&str>` panics
        // on anything else, so a fixture typo (string/number/array `data`)
        // would crash with an opaque message instead of surfacing the mistake
        // the way every other malformed-input path in this fake does.
        let Some(mut obj) = req["data"].as_object().cloned() else {
            return OutputStream::error(WaferError::new(
                ErrorCode::InvalidArgument,
                "fake-db: create requires an object `data`",
            ));
        };
        if obj.get("id").is_none_or(serde_json::Value::is_null) {
            obj.insert(
                "id".to_string(),
                serde_json::Value::String(format!("gen-{}", uuid_like())),
            );
        }
        let data = serde_json::Value::Object(obj);
        self.state
            .lock()
            .collections
            .entry(collection)
            .or_default()
            .push(data.clone());
        let body = serde_json::to_vec(&data).unwrap();
        OutputStream::respond(body)
    }

    fn handle_update(&self, req: &serde_json::Value) -> OutputStream {
        let collection = req["collection"].as_str().unwrap_or("");
        let id = req["id"].as_str().unwrap_or("");
        let data = req["data"].as_object().cloned().unwrap_or_default();
        let mut state = self.state.lock();
        let row = state
            .collections
            .get_mut(collection)
            .and_then(|rows| rows.iter_mut().find(|r| r["id"].as_str() == Some(id)));
        let Some(row) = row else {
            return OutputStream::error(WaferError::new(
                ErrorCode::NotFound,
                format!("fake-db: {collection}/{id} not found"),
            ));
        };
        // Merge the patch into the stored flat row — fields absent from
        // `data` are retained, mirroring the production `UPDATE … SET` path.
        if let Some(obj) = row.as_object_mut() {
            for (k, v) in data {
                obj.insert(k, v);
            }
        }
        // Respond with the updated record in wire format, like production.
        let body = serde_json::to_vec(&to_record(row)).unwrap();
        OutputStream::respond(body)
    }

    fn handle_delete(&self, req: &serde_json::Value) -> OutputStream {
        let collection = req["collection"].as_str().unwrap_or("");
        let id = req["id"].as_str().unwrap_or("");
        let mut state = self.state.lock();
        let removed = state.collections.get_mut(collection).is_some_and(|rows| {
            let before = rows.len();
            rows.retain(|r| r["id"].as_str() != Some(id));
            rows.len() < before
        });
        if !removed {
            return OutputStream::error(WaferError::new(
                ErrorCode::NotFound,
                format!("fake-db: {collection}/{id} not found"),
            ));
        }
        // Production `database.delete` responds with an empty body.
        OutputStream::respond(Vec::new())
    }

    fn handle_count(&self, req: &serde_json::Value) -> OutputStream {
        let collection = req["collection"].as_str().unwrap_or("");
        let filters = req["filters"].as_array().cloned().unwrap_or_default();
        let state = self.state.lock();
        let empty = Vec::new();
        let rows = state.collections.get(collection).unwrap_or(&empty);
        let n = match matching_rows(rows, &filters) {
            Ok(m) => m.len(),
            Err(e) => return OutputStream::error(e),
        };
        let body = serde_json::to_vec(&serde_json::json!({ "count": n })).unwrap();
        OutputStream::respond(body)
    }
}

/// Collect the rows matching all wire-format filters, in seed order.
fn matching_rows<'a>(
    rows: &'a [serde_json::Value],
    filters: &[serde_json::Value],
) -> Result<Vec<&'a serde_json::Value>, WaferError> {
    let mut matching = Vec::new();
    for row in rows {
        if row_matches_filters(row, filters)? {
            matching.push(row);
        }
    }
    Ok(matching)
}

/// Evaluate production wire-format filters against a flat seeded row.
///
/// Operator strings go through [`FilterOp::parse_wire`] — the parser the
/// production database handler uses — so unknown spellings fail with
/// `INVALID_ARGUMENT` instead of silently matching nothing. The fake only
/// implements `Equal`; any other (valid) operator also errors loudly so a
/// fixture gap can't masquerade as an empty result set.
fn row_matches_filters(
    row: &serde_json::Value,
    filters: &[serde_json::Value],
) -> Result<bool, WaferError> {
    for f in filters {
        let field = f["field"].as_str().unwrap_or("");
        // A missing operator defaults to `eq`, matching the serde default on
        // the production `wire::FilterDef`.
        let op = FilterOp::parse_wire(f["operator"].as_str().unwrap_or("eq"))?;
        let expected = &f["value"];
        let actual = &row[field];
        match op {
            FilterOp::Equal => {
                if actual != expected {
                    return Ok(false);
                }
            }
            other => {
                return Err(WaferError::new(
                    ErrorCode::InvalidArgument,
                    format!("fake-db: filter operator {other:?} not implemented (only Equal)"),
                ));
            }
        }
    }
    Ok(true)
}

/// Very small id generator — good enough for tests; not cryptographically random.
fn uuid_like() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    format!("{:016x}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn seed_and_retrieve_rows() {
        let db = FakeDb::new();
        db.seed("users", vec![json!({"id": "u1", "name": "Alice"})]);
        let state = db.state.lock();
        assert_eq!(state.collections.get("users").map(|v| v.len()), Some(1));
    }

    #[test]
    fn failure_mode_unavailable_recorded() {
        let db = FakeDb::new();
        db.set_failure(FailureMode::Unavailable);
        assert!(matches!(db.state.lock().failure, FailureMode::Unavailable));
    }

    #[test]
    fn equal_filter_matches() {
        let row = json!({"id": "u1", "age": 30});
        let eq = json!({"field": "age", "operator": "eq", "value": 30});
        assert!(row_matches_filters(&row, &[eq]).unwrap());
        let ne = json!({"field": "age", "operator": "=", "value": 31});
        assert!(!row_matches_filters(&row, &[ne]).unwrap());
        // Missing operator defaults to `eq`, like `wire::FilterDef`.
        let default_op = json!({"field": "id", "value": "u1"});
        assert!(row_matches_filters(&row, &[default_op]).unwrap());
    }

    #[tokio::test]
    async fn create_rejects_non_object_data() {
        let db = FakeDb::new();
        // A string `data` (a plausible fixture typo) must surface as
        // InvalidArgument, not crash with a serde_json IndexMut panic.
        let out = db.handle_create(&json!({
            "collection": "users",
            "data": "oops-not-an-object",
        }));
        match out.collect_buffered().await {
            Err(wafer_block::streams::output::TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
            }
            other => panic!("expected InvalidArgument error terminal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_with_object_data_generates_id() {
        let db = FakeDb::new();
        let out = db.handle_create(&json!({
            "collection": "users",
            "data": {"name": "Alice"},
        }));
        let buf = out.collect_buffered().await.expect("create should succeed");
        let resp: serde_json::Value = serde_json::from_slice(&buf.body).unwrap();
        assert_eq!(resp["name"], "Alice");
        assert!(
            resp["id"].as_str().is_some_and(|s| s.starts_with("gen-")),
            "create should generate an id when none is supplied"
        );
    }

    #[tokio::test]
    async fn update_mutates_row_and_returns_record() {
        let db = FakeDb::new();
        db.seed(
            "users",
            vec![json!({"id": "u1", "name": "Alice", "age": 30})],
        );
        let out = db.handle_update(&json!({
            "collection": "users",
            "id": "u1",
            "data": {"name": "Bob"},
        }));
        let buf = out.collect_buffered().await.expect("update should succeed");
        let resp: serde_json::Value = serde_json::from_slice(&buf.body).unwrap();
        assert_eq!(resp["id"], "u1");
        assert_eq!(resp["data"]["name"], "Bob");
        assert_eq!(resp["data"]["age"], 30, "untouched fields are retained");

        // The stored row actually changed.
        let state = db.state.lock();
        assert_eq!(state.collections["users"][0]["name"], "Bob");
    }

    #[tokio::test]
    async fn update_missing_row_is_not_found() {
        let db = FakeDb::new();
        let out = db.handle_update(&json!({
            "collection": "users",
            "id": "nope",
            "data": {"name": "Bob"},
        }));
        match out.collect_buffered().await {
            Err(wafer_block::streams::output::TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::NotFound);
            }
            other => panic!("expected NOT_FOUND error terminal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_removes_row_and_missing_is_not_found() {
        let db = FakeDb::new();
        db.seed("users", vec![json!({"id": "u1"}), json!({"id": "u2"})]);

        let out = db.handle_delete(&json!({"collection": "users", "id": "u1"}));
        let buf = out.collect_buffered().await.expect("delete should succeed");
        assert!(
            buf.body.is_empty(),
            "production database.delete responds with an empty body"
        );
        assert_eq!(db.state.lock().collections["users"].len(), 1);

        // Deleting the same row again fails loudly.
        let out = db.handle_delete(&json!({"collection": "users", "id": "u1"}));
        match out.collect_buffered().await {
            Err(wafer_block::streams::output::TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::NotFound);
            }
            other => panic!("expected NOT_FOUND error terminal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn filters_reject_unknown_and_unimplemented_operators() {
        let db = FakeDb::new();
        db.seed("users", vec![json!({"id": "u1", "age": 30})]);

        // Unknown operator spelling → INVALID_ARGUMENT (same as production;
        // the old fake accepted "Equal" and an undocumented "op" key).
        let out = db.handle_list(&json!({
            "collection": "users",
            "filters": [{"field": "age", "operator": "Equal", "value": 30}],
        }));
        match out.collect_buffered().await {
            Err(wafer_block::streams::output::TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
            }
            other => panic!("expected INVALID_ARGUMENT error terminal, got {other:?}"),
        }

        // Valid operator the fake doesn't implement → loud error, not
        // "matches nothing".
        let out = db.handle_count(&json!({
            "collection": "users",
            "filters": [{"field": "age", "operator": "gt", "value": 10}],
        }));
        match out.collect_buffered().await {
            Err(wafer_block::streams::output::TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                assert!(
                    e.message.contains("not implemented"),
                    "message: {}",
                    e.message
                );
            }
            other => panic!("expected INVALID_ARGUMENT error terminal, got {other:?}"),
        }
    }

    use wafer_run::Wafer;

    #[tokio::test]
    async fn dispatch_database_list_returns_seeded_rows() {
        let db = Arc::new(FakeDb::new());
        db.seed("users", vec![json!({"id": "u1", "name": "Alice"})]);

        let mut w = Wafer::builder()
            .disable_inventory()
            .disable_lockfile()
            .build()
            .expect("empty wafer build is infallible");
        w.register_block("test/fake-db", db.clone()).unwrap();
        w.add_alias("wafer-run/database", "test/fake-db")
            .expect("add_alias");
        let wafer = w.start().await.unwrap();

        let mut msg = Message::new("database.list");
        msg.set_meta(wafer_block::META_REQ_ACTION, "database.list");
        let request = json!({
            "collection": "users",
            "filters": [],
            "sort": [],
            "limit": 10,
            "offset": 0,
        });
        let body = serde_json::to_vec(&request).unwrap();
        let out = wafer
            .run_block("wafer-run/database", msg, InputStream::from_bytes(body))
            .await;
        let buf = match out.collect_buffered().await {
            Ok(b) => b,
            Err(e) => panic!("unexpected terminal: {e:?}"),
        };
        let resp: serde_json::Value = serde_json::from_slice(&buf.body).unwrap();
        assert_eq!(resp["records"].as_array().map(|a| a.len()), Some(1));
        assert_eq!(resp["records"][0]["id"], "u1");
    }
}
