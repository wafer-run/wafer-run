//! In-memory database fake implementing the `database@v1` interface.

use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;
use wafer_block::{
    common::ErrorCode,
    streams::{input::InputStream, output::OutputStream},
    Block, BlockCategory, BlockInfo, Context, InstanceMode, LifecycleEvent, Message, WaferError,
};

/// Controls how the fake behaves when dispatched.
#[derive(Debug, Clone, Copy)]
pub enum FailureMode {
    /// Fake handles requests normally.
    None,
    /// Every request returns `ErrorCode::INTERNAL`.
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
    /// `ErrorCode::INTERNAL` — used to exercise caller error handling.
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
                ErrorCode::INTERNAL,
                "fake-db unavailable",
            ));
        }

        let body = input.collect_to_bytes().await;
        let req: serde_json::Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                return OutputStream::error(WaferError::new(
                    ErrorCode::INVALID_ARGUMENT,
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
                ErrorCode::INVALID_ARGUMENT,
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
        // Filter and convert to `{id, data}` wire format expected by `RecordList`.
        let matching: Vec<serde_json::Value> = rows
            .iter()
            .filter(|r| row_matches_filters(r, &filters))
            .take(limit)
            .map(to_record)
            .collect();
        let total_count = rows
            .iter()
            .filter(|r| row_matches_filters(r, &filters))
            .count() as i64;
        let body = serde_json::to_vec(&serde_json::json!({
            "records": matching,
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
                ErrorCode::NOT_FOUND,
                format!("fake-db: {collection}/{id} not found"),
            )),
        }
    }

    fn handle_create(&self, req: &serde_json::Value) -> OutputStream {
        let collection = req["collection"].as_str().unwrap_or("").to_string();
        let mut data = req["data"].clone();
        if data["id"].is_null() {
            data["id"] = serde_json::Value::String(format!("gen-{}", uuid_like()));
        }
        self.state
            .lock()
            .collections
            .entry(collection)
            .or_default()
            .push(data.clone());
        let body = serde_json::to_vec(&data).unwrap();
        OutputStream::respond(body)
    }

    fn handle_update(&self, _req: &serde_json::Value) -> OutputStream {
        OutputStream::respond(b"{}".to_vec())
    }

    fn handle_delete(&self, _req: &serde_json::Value) -> OutputStream {
        OutputStream::respond(b"{}".to_vec())
    }

    fn handle_count(&self, req: &serde_json::Value) -> OutputStream {
        let collection = req["collection"].as_str().unwrap_or("");
        let filters = req["filters"].as_array().cloned().unwrap_or_default();
        let state = self.state.lock();
        let empty = Vec::new();
        let rows = state.collections.get(collection).unwrap_or(&empty);
        let n = rows
            .iter()
            .filter(|r| row_matches_filters(r, &filters))
            .count();
        let body = serde_json::to_vec(&serde_json::json!({ "count": n })).unwrap();
        OutputStream::respond(body)
    }
}

fn row_matches_filters(row: &serde_json::Value, filters: &[serde_json::Value]) -> bool {
    for f in filters {
        let field = f["field"].as_str().unwrap_or("");
        let op = f["operator"]
            .as_str()
            .or_else(|| f["op"].as_str())
            .unwrap_or("eq");
        let expected = &f["value"];
        let actual = &row[field];
        let matched = match op {
            "eq" | "Equal" | "=" => actual == expected,
            _ => false,
        };
        if !matched {
            return false;
        }
    }
    true
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
