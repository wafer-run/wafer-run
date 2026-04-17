# Security-block test coverage implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship 25 tests (21 unit + 4 integration) covering the four security-middleware blocks, backed by a shared `wafer-test-support` crate providing `FakeDb`, `FakeCrypto`, and `WaferBuilder`.

**Architecture:** New workspace crate `wafer-test-support` depending on `wafer-block` + `wafer-run` (regular deps), consumed as dev-dep only by the four security blocks and the wafer-run integration test. Tests drive the real `Wafer` runtime through `WaferBuilder`; FakeDb/FakeCrypto are full `Block` implementations registered under aliases so production code (which calls `ctx.call_block("wafer-run/database", ...)` via `wafer_core::clients::database`) is routed to the fake unchanged. One small production change: a `Clock` trait in `wafer-block-ip-rate-limit` to make window-reset tests deterministic.

**Tech Stack:** Rust, Cargo workspaces, `parking_lot`, `hmac`+`sha2`+`base64ct` for real HMAC-SHA256, `serde_json`, `async-trait`.

**Spec:** `docs/specs/2026-04-17-security-block-tests-design.md`

---

## Task 1: Scaffold `wafer-test-support` crate

**Files:**
- Create: `crates/wafer-test-support/Cargo.toml`
- Create: `crates/wafer-test-support/src/lib.rs`
- Create: `crates/wafer-test-support/src/fake_db.rs` (empty module stub)
- Create: `crates/wafer-test-support/src/fake_crypto.rs` (empty module stub)
- Create: `crates/wafer-test-support/src/builder.rs` (empty module stub)
- Modify: `Cargo.toml` (workspace members)

- [ ] **Step 1: Add workspace member**

In the workspace root `Cargo.toml`, add `"crates/wafer-test-support"` to the `members` array. Keep alphabetical order; it goes between `wafer-sql-utils` and `wafer-site`.

- [ ] **Step 2: Create `crates/wafer-test-support/Cargo.toml`**

```toml
[package]
name = "wafer-test-support"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
wafer-block.workspace = true
wafer-run.workspace = true
async-trait.workspace = true
serde_json.workspace = true
parking_lot.workspace = true
hmac.workspace = true
sha2.workspace = true
base64ct.workspace = true
```

- [ ] **Step 3: Create the lib entry**

`crates/wafer-test-support/src/lib.rs`:

```rust
//! Test fixtures and helpers for wafer-run block tests.
//!
//! This crate is only a dev-dependency of production crates. It exposes
//! `FakeDb` and `FakeCrypto` (real `Block` implementations backed by
//! in-memory state) and a `WaferBuilder` helper that assembles a running
//! `Wafer` runtime with common test wiring.

pub mod builder;
pub mod fake_crypto;
pub mod fake_db;
```

- [ ] **Step 4: Create empty module stubs**

`crates/wafer-test-support/src/fake_db.rs`:

```rust
//! In-memory database fake implementing the `database@v1` interface.
```

`crates/wafer-test-support/src/fake_crypto.rs`:

```rust
//! Crypto fake implementing the `crypto@v1` interface using real HMAC-SHA256
//! so signature math matches production.
```

`crates/wafer-test-support/src/builder.rs`:

```rust
//! `WaferBuilder` — helper for assembling a test `Wafer` runtime.
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo check -p wafer-test-support`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/wafer-test-support Cargo.toml
git commit -m "feat(wafer-test-support): scaffold crate"
```

---

## Task 2: Implement `FakeDb`

**Files:**
- Modify: `crates/wafer-test-support/src/fake_db.rs`

This task implements `FakeDb` as a full `Block` that dispatches on `msg.action()` and handles the minimum subset of `database@v1` actions the security blocks use.

- [ ] **Step 1: Write the failing self-test**

Append to `crates/wafer-test-support/src/fake_db.rs`:

```rust
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
}
```

- [ ] **Step 2: Run — expect fail**

Run: `cargo test -p wafer-test-support fake_db`
Expected: compile error — `FakeDb`, `FailureMode` not defined.

- [ ] **Step 3: Implement `FakeDb` struct and state**

Replace the contents of `crates/wafer-test-support/src/fake_db.rs` with:

```rust
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
}
```

- [ ] **Step 4: Run — expect pass**

Run: `cargo test -p wafer-test-support fake_db`
Expected: PASS (2 tests).

- [ ] **Step 5: Write a failing dispatch test**

Append to the `tests` module:

```rust
    use wafer_block::streams::output::TerminalNotResponse;
    use wafer_run::Wafer;

    #[tokio::test]
    async fn dispatch_database_list_returns_seeded_rows() {
        let db = Arc::new(FakeDb::new());
        db.seed(
            "users",
            vec![json!({"id": "u1", "name": "Alice"})],
        );

        let mut w = Wafer::new();
        w.register_block("test/fake-db".into(), db.clone()).unwrap();
        w.add_alias("wafer-run/database", "test/fake-db");
        let wafer = w.start().await.unwrap();

        let mut msg = Message::new("database.list");
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
```

Also add to `crates/wafer-test-support/Cargo.toml` under a new `[dev-dependencies]` section:

```toml
[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt"] }
```

- [ ] **Step 6: Run — expect fail**

Run: `cargo test -p wafer-test-support fake_db::tests::dispatch_database_list_returns_seeded_rows`
Expected: FAIL — the `Block` trait is not implemented for `FakeDb`.

- [ ] **Step 7: Implement `Block` for `FakeDb`**

Add **above** the `#[cfg(test)]` block in `fake_db.rs`:

```rust
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

        let body = match input.collect_vec().await {
            Ok(b) => b,
            Err(e) => return OutputStream::error(e),
        };
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
            .cloned()
            .take(limit)
            .collect();
        let body = serde_json::to_vec(&serde_json::json!({
            "records": matching,
            "total": rows.iter().filter(|r| row_matches_filters(r, &filters)).count(),
        }))
        .unwrap();
        OutputStream::respond_bytes(body)
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
                OutputStream::respond_bytes(body)
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
        OutputStream::respond_bytes(body)
    }

    fn handle_update(&self, _req: &serde_json::Value) -> OutputStream {
        OutputStream::respond_bytes(b"{}".to_vec())
    }

    fn handle_delete(&self, _req: &serde_json::Value) -> OutputStream {
        OutputStream::respond_bytes(b"{}".to_vec())
    }

    fn handle_count(&self, req: &serde_json::Value) -> OutputStream {
        let collection = req["collection"].as_str().unwrap_or("");
        let filters = req["filters"].as_array().cloned().unwrap_or_default();
        let state = self.state.lock();
        let empty = Vec::new();
        let rows = state.collections.get(collection).unwrap_or(&empty);
        let n = rows.iter().filter(|r| row_matches_filters(r, &filters)).count();
        let body = serde_json::to_vec(&serde_json::json!({ "count": n })).unwrap();
        OutputStream::respond_bytes(body)
    }
}

fn row_matches_filters(row: &serde_json::Value, filters: &[serde_json::Value]) -> bool {
    for f in filters {
        let field = f["field"].as_str().unwrap_or("");
        let op = f["operator"].as_str().or_else(|| f["op"].as_str()).unwrap_or("eq");
        let expected = &f["value"];
        let actual = &row[field];
        let matched = match op {
            "eq" | "Equal" | "=" => actual == expected,
            _ => false, // Other operators unsupported in the fake; fixture authors seed specifically.
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
```

The `OutputStream::respond_bytes(Vec<u8>)` constructor should exist per the streaming protocol. If the exact name differs, inspect `crates/wafer-block/src/streams/output.rs` and use the equivalent (likely one of `respond`, `respond_with_bytes`, or a `BufferedResponse`-based constructor). Match the existing pattern used by `wafer-core/src/interfaces/database/handler.rs`.

Similarly, `InputStream::collect_vec()` — if a differently-named helper exists (e.g., `read_to_end`, `collect_buffered`, `into_bytes`), use it. Inspect `crates/wafer-block/src/streams/input.rs`.

- [ ] **Step 8: Run — expect pass**

Run: `cargo test -p wafer-test-support`
Expected: 3 tests pass.

- [ ] **Step 9: Commit**

```bash
git add crates/wafer-test-support/
git commit -m "feat(wafer-test-support): FakeDb with database@v1 dispatch"
```

---

## Task 3: Implement `FakeCrypto`

**Files:**
- Modify: `crates/wafer-test-support/src/fake_crypto.rs`

- [ ] **Step 1: Write failing dispatch self-test**

Replace the contents of `crates/wafer-test-support/src/fake_crypto.rs` with:

```rust
//! Crypto fake implementing the `crypto@v1` interface using real HMAC-SHA256.

use std::sync::Arc;

use parking_lot::Mutex;
use wafer_block::{
    common::ErrorCode,
    streams::{input::InputStream, output::OutputStream},
    Block, BlockCategory, BlockInfo, Context, InstanceMode, LifecycleEvent, Message, WaferError,
};

use crate::fake_db::FailureMode;

pub(crate) struct FakeCryptoState {
    pub jwt_secret: Vec<u8>,
    pub failure: FailureMode,
}

pub struct FakeCrypto {
    pub(crate) state: Arc<Mutex<FakeCryptoState>>,
}

impl Default for FakeCrypto {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeCrypto {
    pub fn new() -> Self {
        Self::with_secret(b"test-secret-do-not-use-in-prod".to_vec())
    }

    pub fn with_secret(secret: impl Into<Vec<u8>>) -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeCryptoState {
                jwt_secret: secret.into(),
                failure: FailureMode::None,
            })),
        }
    }

    pub fn set_failure(&self, mode: FailureMode) {
        self.state.lock().failure = mode;
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wafer_block::streams::output::TerminalNotResponse;
    use wafer_run::Wafer;

    #[tokio::test]
    async fn sign_and_verify_roundtrip() {
        let crypto = Arc::new(FakeCrypto::new());
        let mut w = Wafer::new();
        w.register_block("test/fake-crypto".into(), crypto.clone()).unwrap();
        w.add_alias("wafer-run/crypto", "test/fake-crypto");
        let wafer = w.start().await.unwrap();

        let sign_req = json!({"claims": {"sub": "u1"}});
        let sign_msg = Message::new("crypto.jwt_sign");
        let sign_out = wafer
            .run_block(
                "wafer-run/crypto",
                sign_msg,
                InputStream::from_bytes(serde_json::to_vec(&sign_req).unwrap()),
            )
            .await;
        let sign_buf = sign_out.collect_buffered().await.expect("sign ok");
        let sign_resp: serde_json::Value = serde_json::from_slice(&sign_buf.body).unwrap();
        let token = sign_resp["token"].as_str().unwrap().to_string();

        let verify_req = json!({"token": token});
        let verify_msg = Message::new("crypto.jwt_verify");
        let verify_out = wafer
            .run_block(
                "wafer-run/crypto",
                verify_msg,
                InputStream::from_bytes(serde_json::to_vec(&verify_req).unwrap()),
            )
            .await;
        let verify_buf = verify_out.collect_buffered().await.expect("verify ok");
        let verify_resp: serde_json::Value = serde_json::from_slice(&verify_buf.body).unwrap();
        assert_eq!(verify_resp["valid"], true);
        assert_eq!(verify_resp["claims"]["sub"], "u1");
    }

    #[tokio::test]
    async fn verify_fails_on_wrong_secret() {
        let signing = Arc::new(FakeCrypto::with_secret(b"secret-a".to_vec()));
        let verifying = Arc::new(FakeCrypto::with_secret(b"secret-b".to_vec()));

        // Sign with one crypto…
        let mut w1 = Wafer::new();
        w1.register_block("test/fake-crypto".into(), signing.clone()).unwrap();
        w1.add_alias("wafer-run/crypto", "test/fake-crypto");
        let wafer1 = w1.start().await.unwrap();
        let sign_msg = Message::new("crypto.jwt_sign");
        let sign_out = wafer1
            .run_block(
                "wafer-run/crypto",
                sign_msg,
                InputStream::from_bytes(serde_json::to_vec(&json!({"claims": {}})).unwrap()),
            )
            .await;
        let token: String =
            serde_json::from_slice::<serde_json::Value>(&sign_out.collect_buffered().await.unwrap().body)
                .unwrap()["token"]
                .as_str()
                .unwrap()
                .to_string();

        // …verify with another.
        let mut w2 = Wafer::new();
        w2.register_block("test/fake-crypto".into(), verifying.clone()).unwrap();
        w2.add_alias("wafer-run/crypto", "test/fake-crypto");
        let wafer2 = w2.start().await.unwrap();
        let verify_msg = Message::new("crypto.jwt_verify");
        let verify_out = wafer2
            .run_block(
                "wafer-run/crypto",
                verify_msg,
                InputStream::from_bytes(serde_json::to_vec(&json!({"token": token})).unwrap()),
            )
            .await;
        match verify_out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::UNAUTHENTICATED);
            }
            other => panic!("expected signature failure, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run — expect fail**

Run: `cargo test -p wafer-test-support fake_crypto`
Expected: FAIL — `Block` not implemented for `FakeCrypto`.

- [ ] **Step 3: Implement `Block` for `FakeCrypto`**

Add **above** the `#[cfg(test)]` block in `fake_crypto.rs`:

```rust
use hmac::{Hmac, Mac};
use sha2::Sha256;
use base64ct::{Base64UrlUnpadded, Encoding};

type HmacSha256 = Hmac<Sha256>;

#[async_trait::async_trait]
impl Block for FakeCrypto {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test/fake-crypto",
            "0.1.0",
            "crypto@v1",
            "Crypto fake using real HMAC-SHA256",
        )
        .instance_mode(InstanceMode::Singleton)
        .category(BlockCategory::Infrastructure)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        if self.should_fail() {
            return OutputStream::error(WaferError::new(
                ErrorCode::INTERNAL,
                "fake-crypto unavailable",
            ));
        }

        let body = match input.collect_vec().await {
            Ok(b) => b,
            Err(e) => return OutputStream::error(e),
        };
        let req: serde_json::Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                return OutputStream::error(WaferError::new(
                    ErrorCode::INVALID_ARGUMENT,
                    format!("fake-crypto: bad request: {e}"),
                ));
            }
        };

        match msg.action() {
            "crypto.jwt_sign" => self.handle_jwt_sign(&req),
            "crypto.jwt_verify" => self.handle_jwt_verify(&req),
            "crypto.hash" => self.handle_hash(&req),
            other => OutputStream::error(WaferError::new(
                ErrorCode::INVALID_ARGUMENT,
                format!("fake-crypto: action '{other}' not implemented"),
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

impl FakeCrypto {
    fn handle_jwt_sign(&self, req: &serde_json::Value) -> OutputStream {
        let claims = &req["claims"];
        let header = serde_json::json!({"alg": "HS256", "typ": "JWT"});
        let header_b64 = Base64UrlUnpadded::encode_string(&serde_json::to_vec(&header).unwrap());
        let claims_b64 = Base64UrlUnpadded::encode_string(&serde_json::to_vec(claims).unwrap());
        let signing_input = format!("{header_b64}.{claims_b64}");

        let secret = self.state.lock().jwt_secret.clone();
        let mut mac = HmacSha256::new_from_slice(&secret).unwrap();
        mac.update(signing_input.as_bytes());
        let sig_b64 = Base64UrlUnpadded::encode_string(&mac.finalize().into_bytes());

        let token = format!("{signing_input}.{sig_b64}");
        OutputStream::respond_bytes(serde_json::to_vec(&serde_json::json!({"token": token})).unwrap())
    }

    fn handle_jwt_verify(&self, req: &serde_json::Value) -> OutputStream {
        let token = match req["token"].as_str() {
            Some(t) => t,
            None => {
                return OutputStream::error(WaferError::new(
                    ErrorCode::INVALID_ARGUMENT,
                    "fake-crypto: missing token",
                ))
            }
        };
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return OutputStream::error(WaferError::new(
                ErrorCode::UNAUTHENTICATED,
                "invalid signature",
            ));
        }
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let sig_bytes = match Base64UrlUnpadded::decode_vec(parts[2]) {
            Ok(b) => b,
            Err(_) => {
                return OutputStream::error(WaferError::new(
                    ErrorCode::UNAUTHENTICATED,
                    "invalid signature",
                ))
            }
        };

        let secret = self.state.lock().jwt_secret.clone();
        let mut mac = HmacSha256::new_from_slice(&secret).unwrap();
        mac.update(signing_input.as_bytes());
        if mac.verify_slice(&sig_bytes).is_err() {
            return OutputStream::error(WaferError::new(
                ErrorCode::UNAUTHENTICATED,
                "invalid signature",
            ));
        }

        let claims_bytes = match Base64UrlUnpadded::decode_vec(parts[1]) {
            Ok(b) => b,
            Err(_) => {
                return OutputStream::error(WaferError::new(
                    ErrorCode::UNAUTHENTICATED,
                    "invalid claims",
                ))
            }
        };
        let claims: serde_json::Value = match serde_json::from_slice(&claims_bytes) {
            Ok(v) => v,
            Err(_) => {
                return OutputStream::error(WaferError::new(
                    ErrorCode::UNAUTHENTICATED,
                    "invalid claims",
                ))
            }
        };

        OutputStream::respond_bytes(
            serde_json::to_vec(&serde_json::json!({"valid": true, "claims": claims})).unwrap(),
        )
    }

    fn handle_hash(&self, req: &serde_json::Value) -> OutputStream {
        use sha2::Digest;
        let data = req["data"].as_str().unwrap_or("");
        let digest = Sha256::digest(data.as_bytes());
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        OutputStream::respond_bytes(serde_json::to_vec(&serde_json::json!({"hash": hex})).unwrap())
    }
}
```

- [ ] **Step 4: Run — expect pass**

Run: `cargo test -p wafer-test-support fake_crypto`
Expected: 2 tests pass (round-trip + wrong-secret rejection).

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-test-support/src/fake_crypto.rs
git commit -m "feat(wafer-test-support): FakeCrypto with real HMAC-SHA256 JWT"
```

---

## Task 4: Implement `WaferBuilder`

**Files:**
- Modify: `crates/wafer-test-support/src/builder.rs`

- [ ] **Step 1: Write the failing test**

Replace `crates/wafer-test-support/src/builder.rs` with:

```rust
//! `WaferBuilder` — helper for assembling a test `Wafer` runtime.

use std::sync::Arc;

use wafer_block::Block;
use wafer_run::{error::RuntimeError, Wafer};

use crate::{fake_crypto::FakeCrypto, fake_db::FakeDb};

pub struct WaferBuilder {
    wafer: Wafer,
}

impl Default for WaferBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl WaferBuilder {
    pub fn new() -> Self {
        Self { wafer: Wafer::new() }
    }

    /// Register `FakeDb` at `test/fake-db` and alias `wafer-run/database`
    /// so production code (`ctx.call_block("wafer-run/database", ...)`)
    /// is routed to the fake unchanged.
    pub fn with_fake_db(mut self, db: Arc<FakeDb>) -> Self {
        self.wafer
            .register_block("test/fake-db".into(), db)
            .expect("register fake-db");
        self.wafer.add_alias("wafer-run/database", "test/fake-db");
        self
    }

    /// Register `FakeCrypto` at `test/fake-crypto` and alias `wafer-run/crypto`.
    pub fn with_fake_crypto(mut self, crypto: Arc<FakeCrypto>) -> Self {
        self.wafer
            .register_block("test/fake-crypto".into(), crypto)
            .expect("register fake-crypto");
        self.wafer.add_alias("wafer-run/crypto", "test/fake-crypto");
        self
    }

    /// Register an arbitrary block at `name`.
    pub fn with_block(mut self, name: &str, block: Arc<dyn Block>) -> Self {
        self.wafer
            .register_block(name.into(), block)
            .expect("register block");
        self
    }

    /// Provide config for a registered block.
    pub fn with_config(mut self, block: &str, config: serde_json::Value) -> Self {
        self.wafer.add_block_config(block, config);
        self
    }

    /// Start the runtime. Returns `Arc<Wafer>`.
    pub async fn build(self) -> Result<Arc<Wafer>, RuntimeError> {
        self.wafer.start().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wafer_block::{streams::input::InputStream, Message};

    #[tokio::test]
    async fn builder_routes_database_alias_to_fake() {
        let db = Arc::new(FakeDb::new());
        db.seed("x", vec![json!({"id": "1", "name": "hi"})]);

        let wafer = WaferBuilder::new()
            .with_fake_db(db.clone())
            .build()
            .await
            .unwrap();

        let msg = Message::new("database.list");
        let req = json!({
            "collection": "x",
            "filters": [],
            "sort": [],
            "limit": 10,
            "offset": 0,
        });
        let out = wafer
            .run_block(
                "wafer-run/database",
                msg,
                InputStream::from_bytes(serde_json::to_vec(&req).unwrap()),
            )
            .await;
        let buf = out.collect_buffered().await.expect("ok");
        let resp: serde_json::Value = serde_json::from_slice(&buf.body).unwrap();
        assert_eq!(resp["records"].as_array().unwrap().len(), 1);
    }
}
```

- [ ] **Step 2: Run — expect pass**

Run: `cargo test -p wafer-test-support builder`
Expected: 1 test passes.

If `Wafer::add_block_config(name, config)` has a different signature than assumed (e.g., requires an owned String, or is named differently), inspect `crates/wafer-run/src/runtime.rs` and adapt. If `register_block` takes `Arc<dyn Block>` rather than `Arc<impl Block>`, adapt.

- [ ] **Step 3: Commit**

```bash
git add crates/wafer-test-support/src/builder.rs
git commit -m "feat(wafer-test-support): WaferBuilder assembles test runtime"
```

---

## Task 5: Add `Clock` trait seam to `wafer-block-ip-rate-limit`

**Files:**
- Modify: `crates/wafer-block-ip-rate-limit/src/lib.rs`

This is the only production change in the plan. It introduces a tiny `Clock` trait so tests can deterministically advance time past the rate-limit window without `std::thread::sleep`.

- [ ] **Step 1: Write failing test**

Append to `crates/wafer-block-ip-rate-limit/src/lib.rs` at the end of the file:

```rust
#[cfg(test)]
mod clock_seam_tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};

    struct FixedClock {
        base: Instant,
        advance_ms: Arc<AtomicU64>,
    }

    impl Clock for FixedClock {
        fn now(&self) -> Instant {
            self.base + Duration::from_millis(self.advance_ms.load(Ordering::Relaxed))
        }
    }

    #[test]
    fn injected_clock_is_used() {
        let advance = Arc::new(AtomicU64::new(0));
        let clock = Arc::new(FixedClock {
            base: Instant::now(),
            advance_ms: advance.clone(),
        });
        let block = RateLimitBlock::with_clock(clock.clone());
        let t0 = clock.now();
        advance.store(1000, Ordering::Relaxed);
        let t1 = clock.now();
        assert!(t1 - t0 >= Duration::from_millis(1000));
        // Block reference used to prove the constructor compiles.
        let _ = block.info();
    }
}
```

- [ ] **Step 2: Run — expect fail**

Run: `cargo test -p wafer-block-ip-rate-limit clock_seam`
Expected: FAIL — `Clock` trait and `with_clock` not defined.

- [ ] **Step 3: Introduce `Clock` trait and thread through `RateLimitBlock`**

In `crates/wafer-block-ip-rate-limit/src/lib.rs`:

1. Near the top of the file, add:

```rust
/// Source of monotonic time for rate-limit windowing. Injected for tests.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

/// Default production clock backed by `std::time::Instant::now()`.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}
```

2. Change `RateLimitBlock` to hold an `Arc<dyn Clock>`:

```rust
pub struct RateLimitBlock {
    max_requests: u32,
    window: Duration,
    buckets: Mutex<HashMap<String, RateBucket>>,
    clock: Arc<dyn Clock>,
}
```

3. Change `new()` and add `with_clock`:

```rust
impl RateLimitBlock {
    pub fn new() -> Self {
        Self::with_clock(Arc::new(SystemClock))
    }

    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            max_requests: 1000,
            window: Duration::from_secs(60),
            buckets: Mutex::new(HashMap::new()),
            clock,
        }
    }
}
```

4. Inside `handle()`, replace every `Instant::now()` call with `self.clock.now()`. There should be at most two call sites (check counter + window-reset). After the change, `Instant::now()` is used only by `SystemClock::now`.

- [ ] **Step 4: Run — expect pass**

Run: `cargo test -p wafer-block-ip-rate-limit`
Expected: new test passes. All other existing tests unchanged.

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-block-ip-rate-limit/src/lib.rs
git commit -m "feat(wafer-block-ip-rate-limit): Clock trait for deterministic tests"
```

---

## Task 6: `wafer-block-auth-validator` unit tests

**Files:**
- Modify: `crates/wafer-block-auth-validator/Cargo.toml` (add dev-deps)
- Modify: `crates/wafer-block-auth-validator/src/lib.rs` (append `#[cfg(test)] mod tests`)

- [ ] **Step 1: Add dev-deps**

In `crates/wafer-block-auth-validator/Cargo.toml`, add (or extend) `[dev-dependencies]`:

```toml
[dev-dependencies]
wafer-test-support = { path = "../wafer-test-support" }
tokio = { workspace = true, features = ["macros", "rt"] }
serde_json.workspace = true
```

- [ ] **Step 2: Append test module**

Append to `crates/wafer-block-auth-validator/src/lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use serde_json::json;
    use wafer_block::{
        streams::{input::InputStream, output::TerminalNotResponse},
        Message,
    };
    use wafer_test_support::{builder::WaferBuilder, fake_crypto::FakeCrypto, fake_db::{FailureMode, FakeDb}};

    fn sha256_hex_helper(s: &str) -> String {
        use sha2::{Digest, Sha256};
        let d = Sha256::digest(s.as_bytes());
        d.iter().map(|b| format!("{b:02x}")).collect()
    }

    async fn build_wafer(db: Arc<FakeDb>, crypto: Arc<FakeCrypto>) -> Arc<wafer_run::Wafer> {
        WaferBuilder::new()
            .with_fake_db(db)
            .with_fake_crypto(crypto)
            .with_block("wafer-run/auth-validator", Arc::new(AuthBlock::new()))
            .build()
            .await
            .expect("build")
    }

    async fn sign_jwt(wafer: &Arc<wafer_run::Wafer>, claims: serde_json::Value) -> String {
        let out = wafer
            .run_block(
                "wafer-run/crypto",
                Message::new("crypto.jwt_sign"),
                InputStream::from_bytes(serde_json::to_vec(&json!({"claims": claims})).unwrap()),
            )
            .await;
        let buf = out.collect_buffered().await.expect("sign ok");
        let resp: serde_json::Value = serde_json::from_slice(&buf.body).unwrap();
        resp["token"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn missing_token_returns_unauthenticated() {
        let db = Arc::new(FakeDb::new());
        let crypto = Arc::new(FakeCrypto::new());
        let wafer = build_wafer(db, crypto).await;

        let msg = Message::new("http.request");
        let out = wafer
            .run_block("wafer-run/auth-validator", msg, InputStream::empty())
            .await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::UNAUTHENTICATED);
            }
            other => panic!("expected Unauthenticated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn valid_jwt_sets_auth_meta_and_continues() {
        let db = Arc::new(FakeDb::new());
        let crypto = Arc::new(FakeCrypto::new());
        let wafer = build_wafer(db, crypto.clone()).await;

        let token = sign_jwt(
            &wafer,
            json!({"sub": "u1", "email": "a@b.c", "roles": ["admin"]}),
        )
        .await;

        let mut msg = Message::new("http.request");
        msg.set_meta("req.header.authorization", &format!("Bearer {token}"));
        let out = wafer
            .run_block("wafer-run/auth-validator", msg, InputStream::empty())
            .await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Continue(continued)) => {
                assert_eq!(continued.get_meta("auth.user_id"), "u1");
                assert_eq!(continued.get_meta("auth.user_email"), "a@b.c");
                assert!(continued.get_meta("auth.user_roles").contains("admin"));
            }
            other => panic!("expected Continue, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn invalid_jwt_signature_returns_unauthenticated() {
        let db = Arc::new(FakeDb::new());
        // Signing crypto uses one secret; validating auth-validator is wired to another.
        let signing = Arc::new(FakeCrypto::with_secret(b"secret-a".to_vec()));
        let validating = Arc::new(FakeCrypto::with_secret(b"secret-b".to_vec()));

        // Produce token under `signing`.
        let mut wsign = wafer_run::Wafer::new();
        wsign.register_block("test/fake-crypto".into(), signing.clone()).unwrap();
        wsign.add_alias("wafer-run/crypto", "test/fake-crypto");
        let ws = wsign.start().await.unwrap();
        let token = sign_jwt(&ws, json!({"sub": "u1"})).await;

        // Wire auth-validator with `validating`.
        let wafer = build_wafer(db, validating).await;
        let mut msg = Message::new("http.request");
        msg.set_meta("req.header.authorization", &format!("Bearer {token}"));
        let out = wafer
            .run_block("wafer-run/auth-validator", msg, InputStream::empty())
            .await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::UNAUTHENTICATED);
            }
            other => panic!("expected Unauthenticated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn valid_api_key_sets_auth_meta() {
        let db = Arc::new(FakeDb::new());
        let raw_key = "sb_testkey_abc123";
        db.seed(
            "api_keys",
            vec![json!({
                "id": "k1",
                "key_hash": sha256_hex_helper(raw_key),
                "user_id": "u1",
                "user_email": "a@b.c",
                "revoked_at": null,
            })],
        );
        let crypto = Arc::new(FakeCrypto::new());
        let wafer = build_wafer(db, crypto).await;

        let mut msg = Message::new("http.request");
        msg.set_meta("req.header.x-api-key", raw_key);
        let out = wafer
            .run_block("wafer-run/auth-validator", msg, InputStream::empty())
            .await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Continue(continued)) => {
                assert_eq!(continued.get_meta("auth.user_id"), "u1");
            }
            other => panic!("expected Continue, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_api_key_returns_unauthenticated() {
        let db = Arc::new(FakeDb::new());
        // No api_keys seeded.
        let crypto = Arc::new(FakeCrypto::new());
        let wafer = build_wafer(db, crypto).await;

        let mut msg = Message::new("http.request");
        msg.set_meta("req.header.x-api-key", "sb_not_in_db");
        let out = wafer
            .run_block("wafer-run/auth-validator", msg, InputStream::empty())
            .await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::UNAUTHENTICATED);
            }
            other => panic!("expected Unauthenticated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn db_unavailable_on_api_key_returns_internal_not_bypass() {
        let db = Arc::new(FakeDb::new());
        db.set_failure(FailureMode::Unavailable);
        let crypto = Arc::new(FakeCrypto::new());
        let wafer = build_wafer(db, crypto).await;

        let mut msg = Message::new("http.request");
        msg.set_meta("req.header.x-api-key", "sb_any");
        let out = wafer
            .run_block("wafer-run/auth-validator", msg, InputStream::empty())
            .await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                // Must not silently allow the request; must not be Unauthenticated
                // (which would leak the DB state); must be Internal.
                assert_eq!(e.code, ErrorCode::INTERNAL);
            }
            other => panic!("expected Internal error, got {other:?}"),
        }
    }
}
```

The exact header meta key names (`req.header.authorization`, `req.header.x-api-key`) must match what the auth-validator's `msg.header(...)` helper reads. Inspect the real meta conventions in `crates/wafer-block/src/meta.rs` or grep for `META_HEADER_PREFIX` to confirm; adapt if the convention differs. The same applies to `Message::set_meta` / `Message::header` pairing.

- [ ] **Step 3: Run — expect pass**

Run: `cargo test -p wafer-block-auth-validator`
Expected: 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/wafer-block-auth-validator/
git commit -m "test(wafer-block-auth-validator): unit tests covering JWT + API key + DB failure paths"
```

---

## Task 7: `wafer-block-iam-guard` unit tests

**Files:**
- Modify: `crates/wafer-block-iam-guard/Cargo.toml`
- Modify: `crates/wafer-block-iam-guard/src/lib.rs`

- [ ] **Step 1: Add dev-deps**

Same as Task 6 Step 1 but in `crates/wafer-block-iam-guard/Cargo.toml`. `FakeCrypto` is not needed here.

```toml
[dev-dependencies]
wafer-test-support = { path = "../wafer-test-support" }
tokio = { workspace = true, features = ["macros", "rt"] }
serde_json.workspace = true
```

- [ ] **Step 2: Append tests**

Append to `crates/wafer-block-iam-guard/src/lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use serde_json::json;
    use wafer_block::{
        streams::{input::InputStream, output::TerminalNotResponse},
        Message,
    };
    use wafer_test_support::{builder::WaferBuilder, fake_db::{FailureMode, FakeDb}};

    async fn build_wafer(
        db: Arc<FakeDb>,
        config: Option<serde_json::Value>,
    ) -> Arc<wafer_run::Wafer> {
        let mut b = WaferBuilder::new()
            .with_fake_db(db)
            .with_block("wafer-run/iam-guard", Arc::new(IAMBlock::new()));
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
        let out = wafer.run_block("wafer-run/iam-guard", msg, InputStream::empty()).await;
        assert!(matches!(out.collect_buffered().await, Err(TerminalNotResponse::Continue(_))));
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
        let out = wafer.run_block("wafer-run/iam-guard", msg, InputStream::empty()).await;
        match out.collect_buffered().await {
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
        let out = wafer.run_block("wafer-run/iam-guard", msg, InputStream::empty()).await;
        assert!(matches!(out.collect_buffered().await, Err(TerminalNotResponse::Continue(_))));
    }

    #[tokio::test]
    async fn db_unavailable_meta_roles_missing_denies() {
        let db = Arc::new(FakeDb::new());
        db.set_failure(FailureMode::Unavailable);
        let wafer = build_wafer(db, Some(json!({"role": "admin"}))).await;

        let msg = with_user(Message::new("http.request"), "u1", "viewer");
        let out = wafer.run_block("wafer-run/iam-guard", msg, InputStream::empty()).await;
        match out.collect_buffered().await {
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
        let out = wafer.run_block("wafer-run/iam-guard", msg, InputStream::empty()).await;
        match out.collect_buffered().await {
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
        // No config — verifies the documented `admin` default.
        let wafer = build_wafer(db, None).await;

        let msg = with_user(Message::new("http.request"), "u1", "");
        let out = wafer.run_block("wafer-run/iam-guard", msg, InputStream::empty()).await;
        assert!(matches!(out.collect_buffered().await, Err(TerminalNotResponse::Continue(_))));
    }
}
```

- [ ] **Step 3: Run — expect pass**

Run: `cargo test -p wafer-block-iam-guard`
Expected: 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/wafer-block-iam-guard/
git commit -m "test(wafer-block-iam-guard): unit tests covering DB + meta fallback paths"
```

---

## Task 8: `wafer-block-readonly-guard` unit tests

**Files:**
- Modify: `crates/wafer-block-readonly-guard/Cargo.toml`
- Modify: `crates/wafer-block-readonly-guard/src/lib.rs`

- [ ] **Step 1: Add dev-deps**

```toml
[dev-dependencies]
wafer-test-support = { path = "../wafer-test-support" }
tokio = { workspace = true, features = ["macros", "rt"] }
serde_json.workspace = true
```

- [ ] **Step 2: Append tests**

Append to `crates/wafer-block-readonly-guard/src/lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use serde_json::json;
    use wafer_block::{
        streams::{input::InputStream, output::TerminalNotResponse},
        Message,
    };
    use wafer_test_support::builder::WaferBuilder;

    async fn build_wafer(config: Option<serde_json::Value>) -> Arc<wafer_run::Wafer> {
        let mut b = WaferBuilder::new().with_block(
            "wafer-run/readonly-guard",
            Arc::new(ReadonlyGuardBlock::new()),
        );
        if let Some(cfg) = config {
            b = b.with_config("wafer-run/readonly-guard", cfg);
        }
        b.build().await.expect("build")
    }

    async fn run(wafer: &Arc<wafer_run::Wafer>, action: &str) -> wafer_block::streams::output::BufferedResponse {
        let out = wafer
            .run_block(
                "wafer-run/readonly-guard",
                Message::new(action),
                InputStream::empty(),
            )
            .await;
        // Accept both Continue and Respond as "allowed".
        match out.collect_buffered().await {
            Ok(buf) => buf,
            Err(TerminalNotResponse::Continue(msg)) => {
                // Continue is expected for middleware — represent as empty buffer with original action.
                wafer_block::streams::output::BufferedResponse {
                    meta: msg.meta().to_vec(),
                    body: Vec::new(),
                }
            }
            Err(e) => panic!("unexpected terminal: {e:?}"),
        }
    }

    async fn expect_denied(wafer: &Arc<wafer_run::Wafer>, action: &str) {
        let out = wafer
            .run_block(
                "wafer-run/readonly-guard",
                Message::new(action),
                InputStream::empty(),
            )
            .await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::PERMISSION_DENIED);
            }
            other => panic!("expected PermissionDenied for action '{action}', got {other:?}"),
        }
    }

    #[tokio::test]
    async fn readonly_off_write_action_continues() {
        let wafer = build_wafer(Some(json!({"readonly": "false"}))).await;
        let _ = run(&wafer, "create").await;
        let _ = run(&wafer, "update").await;
        let _ = run(&wafer, "delete").await;
    }

    #[tokio::test]
    async fn readonly_on_write_actions_all_deny() {
        let wafer = build_wafer(Some(json!({"readonly": "true"}))).await;
        expect_denied(&wafer, "create").await;
        expect_denied(&wafer, "update").await;
        expect_denied(&wafer, "delete").await;
    }

    #[tokio::test]
    async fn readonly_on_read_action_continues() {
        let wafer = build_wafer(Some(json!({"readonly": "true"}))).await;
        let _ = run(&wafer, "retrieve").await;
        let _ = run(&wafer, "list").await;
    }

    #[tokio::test]
    async fn readonly_default_off_allows_writes() {
        let wafer = build_wafer(None).await;
        let _ = run(&wafer, "create").await;
    }
}
```

If `BufferedResponse` has a different public shape than `{ meta, body }` (e.g., private fields), adapt the helper — use only the public API. Worst case, replace the `run` helper with inline match arms per test.

- [ ] **Step 3: Run — expect pass**

Run: `cargo test -p wafer-block-readonly-guard`
Expected: 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/wafer-block-readonly-guard/
git commit -m "test(wafer-block-readonly-guard): unit tests for write-blocking"
```

---

## Task 9: `wafer-block-ip-rate-limit` unit tests

**Files:**
- Modify: `crates/wafer-block-ip-rate-limit/Cargo.toml`
- Modify: `crates/wafer-block-ip-rate-limit/src/lib.rs`

- [ ] **Step 1: Add dev-deps**

```toml
[dev-dependencies]
wafer-test-support = { path = "../wafer-test-support" }
tokio = { workspace = true, features = ["macros", "rt"] }
serde_json.workspace = true
```

- [ ] **Step 2: Append tests**

Append to `crates/wafer-block-ip-rate-limit/src/lib.rs`:

```rust
#[cfg(test)]
mod rate_limit_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use serde_json::json;
    use wafer_block::{
        streams::{input::InputStream, output::TerminalNotResponse},
        Message,
    };
    use wafer_test_support::builder::WaferBuilder;

    struct ControllableClock {
        base: Instant,
        offset_ms: AtomicU64,
    }

    impl ControllableClock {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                base: Instant::now(),
                offset_ms: AtomicU64::new(0),
            })
        }
        fn advance(&self, ms: u64) {
            self.offset_ms.fetch_add(ms, Ordering::Relaxed);
        }
    }

    impl Clock for ControllableClock {
        fn now(&self) -> Instant {
            self.base + Duration::from_millis(self.offset_ms.load(Ordering::Relaxed))
        }
    }

    async fn build_wafer_with_clock(
        clock: Arc<dyn Clock>,
        config: serde_json::Value,
    ) -> Arc<wafer_run::Wafer> {
        WaferBuilder::new()
            .with_block(
                "wafer-run/ip-rate-limit",
                Arc::new(RateLimitBlock::with_clock(clock)),
            )
            .with_config("wafer-run/ip-rate-limit", config)
            .build()
            .await
            .expect("build")
    }

    fn request_from(ip: &str) -> Message {
        let mut msg = Message::new("http.request");
        msg.set_meta("req.remote_addr", ip);
        msg
    }

    #[tokio::test]
    async fn under_limit_continues_with_remaining_meta() {
        std::env::remove_var("RATE_LIMIT_IP");
        let clock = ControllableClock::new();
        let wafer = build_wafer_with_clock(
            clock.clone(),
            json!({"max_requests": "10", "window_seconds": "60"}),
        )
        .await;
        let out = wafer
            .run_block("wafer-run/ip-rate-limit", request_from("1.1.1.1"), InputStream::empty())
            .await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Continue(continued)) => {
                let remaining = continued.get_meta("resp.header.x-ratelimit-remaining");
                assert!(!remaining.is_empty(), "X-RateLimit-Remaining meta missing");
            }
            other => panic!("expected Continue, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn over_limit_denies_with_retry_after() {
        std::env::remove_var("RATE_LIMIT_IP");
        let clock = ControllableClock::new();
        let wafer = build_wafer_with_clock(
            clock.clone(),
            json!({"max_requests": "2", "window_seconds": "60"}),
        )
        .await;

        for _ in 0..2 {
            let out = wafer
                .run_block("wafer-run/ip-rate-limit", request_from("2.2.2.2"), InputStream::empty())
                .await;
            let _ = out.collect_buffered().await;
        }

        // Third request over the limit.
        let out = wafer
            .run_block("wafer-run/ip-rate-limit", request_from("2.2.2.2"), InputStream::empty())
            .await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert!(e.meta.iter().any(|m| m.key.eq_ignore_ascii_case("resp.header.retry-after")
                    || m.key.eq_ignore_ascii_case("retry-after")));
            }
            other => panic!("expected rate-limit error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn window_reset_restores_budget() {
        std::env::remove_var("RATE_LIMIT_IP");
        let clock = ControllableClock::new();
        let wafer = build_wafer_with_clock(
            clock.clone(),
            json!({"max_requests": "1", "window_seconds": "1"}),
        )
        .await;

        // Fire one (OK), one more (rejected).
        let _ = wafer
            .run_block("wafer-run/ip-rate-limit", request_from("3.3.3.3"), InputStream::empty())
            .await
            .collect_buffered()
            .await;
        let blocked = wafer
            .run_block("wafer-run/ip-rate-limit", request_from("3.3.3.3"), InputStream::empty())
            .await
            .collect_buffered()
            .await;
        assert!(matches!(blocked, Err(TerminalNotResponse::Error(_))));

        // Advance past the window.
        clock.advance(1_500);

        // Should succeed again.
        let reset = wafer
            .run_block("wafer-run/ip-rate-limit", request_from("3.3.3.3"), InputStream::empty())
            .await;
        assert!(matches!(reset.collect_buffered().await, Err(TerminalNotResponse::Continue(_))));
    }

    #[tokio::test]
    async fn disable_via_env_skips_entirely() {
        std::env::set_var("RATE_LIMIT_IP", "0");
        let clock = ControllableClock::new();
        let wafer = build_wafer_with_clock(
            clock.clone(),
            json!({"max_requests": "1", "window_seconds": "60"}),
        )
        .await;

        for _ in 0..3 {
            let out = wafer
                .run_block("wafer-run/ip-rate-limit", request_from("4.4.4.4"), InputStream::empty())
                .await;
            assert!(matches!(
                out.collect_buffered().await,
                Err(TerminalNotResponse::Continue(_))
            ));
        }
        std::env::remove_var("RATE_LIMIT_IP");
    }

    #[tokio::test]
    async fn distinct_ips_have_separate_buckets() {
        std::env::remove_var("RATE_LIMIT_IP");
        let clock = ControllableClock::new();
        let wafer = build_wafer_with_clock(
            clock.clone(),
            json!({"max_requests": "1", "window_seconds": "60"}),
        )
        .await;

        for ip in ["5.5.5.5", "6.6.6.6"] {
            let out = wafer
                .run_block("wafer-run/ip-rate-limit", request_from(ip), InputStream::empty())
                .await;
            assert!(matches!(
                out.collect_buffered().await,
                Err(TerminalNotResponse::Continue(_))
            ));
        }
    }
}
```

The meta key `req.remote_addr` must match what `msg.remote_addr()` reads. If the convention is `http.req.remote_addr` or similar, inspect `crates/wafer-block/src/meta.rs` / `types.rs` and adapt. The `resp.header.retry-after` assertion similarly; if the block writes a differently-formatted key, look at the existing `handle()` code around line 115-131 and match.

`std::env::set_var` is process-global and affects other tests if run in parallel. Add `#[serial_test::serial]` attribute if the existing crate uses it, or run with `--test-threads=1` — inspect the crate for conventions.

- [ ] **Step 3: Run — expect pass**

Run: `cargo test -p wafer-block-ip-rate-limit`
Expected: 5 new tests pass (plus the 1 clock seam test from Task 5 = 6 total).

- [ ] **Step 4: Commit**

```bash
git add crates/wafer-block-ip-rate-limit/
git commit -m "test(wafer-block-ip-rate-limit): unit tests with deterministic clock"
```

---

## Task 10: Integration test — security pipeline end-to-end

**Files:**
- Create: `crates/wafer-run/tests/security_pipeline_e2e.rs`
- Modify: `crates/wafer-run/Cargo.toml` (add `wafer-test-support` + security blocks as dev-deps if missing)

- [ ] **Step 1: Add dev-deps**

In `crates/wafer-run/Cargo.toml` under `[dev-dependencies]`:

```toml
wafer-test-support = { path = "../wafer-test-support" }
wafer-block-auth-validator = { path = "../wafer-block-auth-validator" }
wafer-block-iam-guard = { path = "../wafer-block-iam-guard" }
wafer-block-readonly-guard = { path = "../wafer-block-readonly-guard" }
wafer-block-ip-rate-limit = { path = "../wafer-block-ip-rate-limit" }
# tokio should already be present under dev-dependencies — verify.
```

- [ ] **Step 2: Create the integration test file**

Create `crates/wafer-run/tests/security_pipeline_e2e.rs`:

```rust
//! Integration: compose the full security pipeline end-to-end through the
//! real Wafer runtime.
//!
//! Pipeline: auth-validator → iam-guard → ip-rate-limit → readonly-guard → handler

use std::sync::Arc;

use serde_json::json;
use wafer_block::{
    common::ErrorCode,
    streams::{input::InputStream, output::TerminalNotResponse},
    Block, BlockCategory, BlockInfo, Context, InstanceMode, LifecycleEvent, Message, WaferError,
};
use wafer_block_auth_validator::AuthBlock;
use wafer_block_iam_guard::IAMBlock;
use wafer_block_ip_rate_limit::{Clock, RateLimitBlock};
use wafer_block_readonly_guard::ReadonlyGuardBlock;
use wafer_run::Wafer;
use wafer_test_support::{builder::WaferBuilder, fake_crypto::FakeCrypto, fake_db::FakeDb};

/// A terminal handler block used as the last step of the pipeline. Returns
/// a canned 200 OK response so successful composition is visible.
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
        wafer_block::streams::output::OutputStream::respond_bytes(b"{\"ok\":true}".to_vec())
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> Result<(), WaferError> {
        Ok(())
    }
}

struct ZeroClock;
impl Clock for ZeroClock {
    fn now(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}

fn seed_auth(db: &Arc<FakeDb>, user_id: &str, role: &str) {
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
        .with_block("wafer-run/auth-validator", Arc::new(AuthBlock::new()))
        .with_block("wafer-run/iam-guard", Arc::new(IAMBlock::new()))
        .with_block(
            "wafer-run/ip-rate-limit",
            Arc::new(RateLimitBlock::with_clock(Arc::new(ZeroClock))),
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

/// Thin flow runner — the integration test chains blocks by calling each
/// in sequence and passing the `Continue` message through. This mirrors
/// what the flow executor does, without requiring a flow definition.
async fn run_pipeline(wafer: &Arc<Wafer>, initial: Message) -> Result<(), WaferError> {
    let pipeline = [
        "wafer-run/auth-validator",
        "wafer-run/iam-guard",
        "wafer-run/ip-rate-limit",
        "wafer-run/readonly-guard",
        "test/ok-handler",
    ];

    let mut msg = initial;
    for (i, name) in pipeline.iter().enumerate() {
        let out = wafer.run_block(name, msg.clone(), InputStream::empty()).await;
        match out.collect_buffered().await {
            Ok(_buf) => {
                // Last step returns Respond — we're done.
                if i == pipeline.len() - 1 {
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
                ))
            }
        }
    }
    Ok(())
}

async fn signed_jwt(wafer: &Arc<Wafer>, claims: serde_json::Value) -> String {
    let out = wafer
        .run_block(
            "wafer-run/crypto",
            Message::new("crypto.jwt_sign"),
            InputStream::from_bytes(serde_json::to_vec(&json!({"claims": claims})).unwrap()),
        )
        .await;
    let buf = out.collect_buffered().await.expect("sign ok");
    let resp: serde_json::Value = serde_json::from_slice(&buf.body).unwrap();
    resp["token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn happy_path_authed_read_succeeds() {
    std::env::remove_var("RATE_LIMIT_IP");
    let db = Arc::new(FakeDb::new());
    seed_auth(&db, "u1", "admin");
    let crypto = Arc::new(FakeCrypto::new());
    let wafer = build_pipeline(db, crypto.clone(), "admin", false, 1000).await;
    let token = signed_jwt(&wafer, json!({"sub": "u1", "email": "a@b.c", "roles": ["admin"]})).await;

    let mut msg = Message::new("retrieve");
    msg.set_meta("req.header.authorization", &format!("Bearer {token}"));
    msg.set_meta("req.remote_addr", "9.9.9.9");

    assert!(run_pipeline(&wafer, msg).await.is_ok());
}

#[tokio::test]
async fn unauthenticated_request_stops_at_auth() {
    std::env::remove_var("RATE_LIMIT_IP");
    let db = Arc::new(FakeDb::new());
    let crypto = Arc::new(FakeCrypto::new());
    let wafer = build_pipeline(db, crypto, "admin", false, 1000).await;

    let mut msg = Message::new("retrieve");
    msg.set_meta("req.remote_addr", "9.9.9.9");
    let err = run_pipeline(&wafer, msg).await.expect_err("pipeline should deny");
    assert_eq!(err.code, ErrorCode::UNAUTHENTICATED);
}

#[tokio::test]
async fn authed_wrong_role_stops_at_iam() {
    std::env::remove_var("RATE_LIMIT_IP");
    let db = Arc::new(FakeDb::new());
    seed_auth(&db, "u1", "viewer"); // user has viewer, guard requires admin
    let crypto = Arc::new(FakeCrypto::new());
    let wafer = build_pipeline(db, crypto.clone(), "admin", false, 1000).await;
    let token = signed_jwt(&wafer, json!({"sub": "u1", "roles": ["viewer"]})).await;

    let mut msg = Message::new("retrieve");
    msg.set_meta("req.header.authorization", &format!("Bearer {token}"));
    msg.set_meta("req.remote_addr", "9.9.9.9");
    let err = run_pipeline(&wafer, msg).await.expect_err("pipeline should deny");
    assert_eq!(err.code, ErrorCode::PERMISSION_DENIED);
}

#[tokio::test]
async fn readonly_mode_blocks_writes_through_pipeline() {
    std::env::remove_var("RATE_LIMIT_IP");
    let db = Arc::new(FakeDb::new());
    seed_auth(&db, "u1", "admin");
    let crypto = Arc::new(FakeCrypto::new());
    let wafer = build_pipeline(db, crypto.clone(), "admin", true, 1000).await;
    let token = signed_jwt(&wafer, json!({"sub": "u1", "roles": ["admin"]})).await;

    let mut msg = Message::new("create");
    msg.set_meta("req.header.authorization", &format!("Bearer {token}"));
    msg.set_meta("req.remote_addr", "9.9.9.9");
    let err = run_pipeline(&wafer, msg).await.expect_err("pipeline should deny");
    assert_eq!(err.code, ErrorCode::PERMISSION_DENIED);
    assert!(err.message.to_lowercase().contains("read-only"));
}
```

If a real flow-based test is preferred over the manual `run_pipeline` chain, use `wafer_flow::WaferFlow` construction instead (see `crates/wafer-run/tests/integration_test.rs` for the pattern). The manual chain is simpler and proves the same thing: meta flows from block to block correctly through `Continue`.

- [ ] **Step 3: Run — expect pass**

Run: `cargo test -p wafer-run --test security_pipeline_e2e`
Expected: 4 tests pass.

- [ ] **Step 4: Run the full workspace as a regression check**

Run: `cargo test --workspace`
Expected: no pre-existing tests regressed; 25 new tests total in the security blocks + helper crate.

- [ ] **Step 5: Run clippy**

Run: `cargo clippy --workspace --no-deps -- -D warnings`
Expected: clean on new code. Pre-existing warnings in `wafer-block-sqlite` / `wafer-sql-utils` (the PI approximation ones) are not your concern.

- [ ] **Step 6: Commit**

```bash
git add crates/wafer-run/
git commit -m "test(wafer-run): security-pipeline end-to-end integration tests"
```

---

## Post-implementation checklist

- [ ] `cargo test --workspace` — 25 new tests pass.
- [ ] `cargo clippy --workspace --no-deps -- -D warnings` — clean on new code.
- [ ] No production source file contains test-only dispatch logic.
- [ ] `wafer-test-support` is only a dev-dep of `wafer-block-auth-validator`, `wafer-block-iam-guard`, `wafer-block-readonly-guard`, `wafer-block-ip-rate-limit`, and `wafer-run` — never a runtime dep of any production crate.
- [ ] The `Clock` trait addition to `wafer-block-ip-rate-limit` preserves behavior: default `SystemClock` is used by `RateLimitBlock::new()`, existing callers untouched.
