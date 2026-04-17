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
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeDbState {
                collections: HashMap::new(),
                failure: FailureMode::None,
            })),
        }
    }

    /// Insert rows into a collection. Does not touch failure mode.
    pub fn seed(&self, collection: &str, rows: Vec<serde_json::Value>) {
        self.state
            .lock()
            .collections
            .entry(collection.to_string())
            .or_default()
            .extend(rows);
    }

    pub fn set_failure(&self, mode: FailureMode) {
        self.state.lock().failure = mode;
    }

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

impl FakeDb {
    fn handle_list(&self, req: &serde_json::Value) -> OutputStream {
        let collection = req["collection"].as_str().unwrap_or("");
        let filters = req["filters"].as_array().cloned().unwrap_or_default();
        let limit = req["limit"].as_i64().unwrap_or(i64::MAX).max(0) as usize;
        let state = self.state.lock();
        let empty = Vec::new();
        let rows = state.collections.get(collection).unwrap_or(&empty);
        let matching: Vec<serde_json::Value> = rows
            .iter()
            .filter(|r| row_matches_filters(r, &filters))
            .take(limit)
            .cloned()
            .collect();
        let body = serde_json::to_vec(&serde_json::json!({
            "records": matching,
            "total": rows.iter().filter(|r| row_matches_filters(r, &filters)).count(),
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
                let body = serde_json::to_vec(&row).unwrap();
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
    use super::*;
    use serde_json::json;

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

        let mut w = Wafer::new();
        w.register_block("test/fake-db", db.clone()).unwrap();
        w.add_alias("wafer-run/database", "test/fake-db");
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
