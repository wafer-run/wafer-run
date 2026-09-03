//! End-to-end: a **dependency-free, std-only** wasm guest that negotiates the
//! JSON host-call codec (`__wafer_host_codec() -> 1`) drives the REAL
//! `wafer-run/database`, `wafer-run/storage` and `wafer-run/config` service
//! blocks — real `wafer-core` handlers, real WRAP + capability enforcement,
//! a real in-memory SQLite backend — with the host transcoding every host-call
//! body and response frame between JSON and MessagePack at the ABI boundary.
//!
//! The guest (`tests/json_host_guest/`) has an EMPTY `[dependencies]` table:
//! no SDK, no serde, no MessagePack encoder. It emits static JSON request
//! bodies and hands the raw response frames straight back, so every assertion
//! below is on bytes the *host* produced. That is the compatibility contract
//! impresspress Plan 3's `wafer_guest.rs` is built against — a toolchain that
//! can only compile `core`+`std` must still be able to reach the platform
//! services.
//!
//! ## Wire shapes this test pins (verified against the handlers, not assumed)
//!
//! - `database.get` answers with ONE frame: a `wire::Record` (`{id, data}`).
//! - `config.get` answers with ONE frame: a `wire::config::GetResponse`
//!   (`{value}`).
//! - `storage.get` answers with TWO frames: a `wire::storage::ObjectInfo`
//!   header and then the object body **verbatim**. Only the header is a wire
//!   DTO; the handler marks the body frames raw on the stream
//!   (`frame.encoding = raw`), so the host transcodes the header and passes
//!   the body through untouched. See `json_guest_storage_round_trip`, which
//!   asserts both halves.
//! - Errors reach a JSON guest through `stream_take_error` as
//!   `serde_json::to_vec(&WaferError)` → `{"code":"NotFound",…}` (the
//!   `ErrorCode` variant name, not a snake_case spelling).
//! - `stream_attach` is MessagePack-only and returns the
//!   `-(ErrorCode::InvalidArgument)` sentinel to a JSON guest.

#![cfg(feature = "wasm")]

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use wafer_block::{streams::input::InputStream, Message};
use wafer_block_sqlite::service::SQLiteDatabaseService;
use wafer_core::{
    interfaces::storage::service::{
        FolderInfo, ListOptions, ObjectInfo, ObjectList, StorageError, StorageService,
    },
    service_blocks::{config::EnvConfigService, database::register_with_tables},
};
use wafer_run::{wasm::WasmiBlock, Wafer};

/// The guest's registered block id. Its WRAP namespace (`test/json-host-guest`
/// → `test__json_host_guest__*`) is what makes the resources below its OWN, so
/// no grant setup is needed and the tests stay about the codec.
const GUEST: &str = "test/json-host-guest";
const CONFIG_KEY: &str = "TEST__JSON_HOST_GUEST__GREETING";
const CONFIG_VALUE: &str = "hi";

/// Path to the prebuilt JSON-codec guest wasm. Built by
/// `scripts/build-fixtures.sh`, or directly with:
///
/// ```bash
/// cargo build --target wasm32-wasip1 --release \
///     --manifest-path crates/wafer-run/tests/json_host_guest/Cargo.toml
/// ```
fn guest_wasm() -> Vec<u8> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/json_host_guest/target/wasm32-wasip1/release/json_host_guest.wasm");
    std::fs::read(&p).unwrap_or_else(|e| {
        panic!(
            "failed to read json-host-guest wasm at {}: {e}\n\
             Did you build it first?\n  scripts/build-fixtures.sh",
            p.display()
        )
    })
}

/// Minimal in-memory [`StorageService`] — enough for a put/get round trip
/// through the REAL `wafer-core` storage handler (which is what carries the
/// WRAP + capability checks this test cares about). Keyed on
/// `"{folder}/{key}"`, the same string the handler authorizes on.
#[derive(Default)]
struct MemStorage {
    objects: Mutex<HashMap<String, (Vec<u8>, String)>>,
}

#[async_trait::async_trait]
impl StorageService for MemStorage {
    async fn put(
        &self,
        folder: &str,
        key: &str,
        data: &[u8],
        content_type: &str,
    ) -> Result<(), StorageError> {
        self.objects.lock().unwrap().insert(
            format!("{folder}/{key}"),
            (data.to_vec(), content_type.to_string()),
        );
        Ok(())
    }

    async fn get(&self, folder: &str, key: &str) -> Result<(Vec<u8>, ObjectInfo), StorageError> {
        let (data, content_type) = self
            .objects
            .lock()
            .unwrap()
            .get(&format!("{folder}/{key}"))
            .cloned()
            .ok_or(StorageError::NotFound)?;
        let info = ObjectInfo {
            key: key.to_string(),
            size: data.len() as i64,
            content_type,
            last_modified: chrono::Utc::now(),
        };
        Ok((data, info))
    }

    async fn delete(&self, folder: &str, key: &str) -> Result<(), StorageError> {
        self.objects
            .lock()
            .unwrap()
            .remove(&format!("{folder}/{key}"));
        Ok(())
    }

    async fn list(&self, _folder: &str, _opts: &ListOptions) -> Result<ObjectList, StorageError> {
        Ok(ObjectList::default())
    }

    async fn create_folder(&self, _name: &str, _public: bool) -> Result<(), StorageError> {
        Ok(())
    }

    async fn delete_folder(&self, _name: &str) -> Result<(), StorageError> {
        Ok(())
    }

    async fn list_folders(&self) -> Result<Vec<FolderInfo>, StorageError> {
        Ok(Vec::new())
    }
}

/// Build an unstarted `Wafer` carrying the three REAL service blocks the guest
/// calls: `wafer-run/database` (real handler over in-memory SQLite),
/// `wafer-run/storage` (real handler over [`MemStorage`]) and
/// `wafer-run/config` (real handler over [`EnvConfigService`] seeded with
/// `config`).
///
/// Shape borrowed from `wrap_hostile_guest_e2e.rs::build_wafer_with_real_db`;
/// no admin block and no WRAP grants — the guest is an ordinary unprivileged
/// caller that only reaches its own namespace.
async fn build_wafer_with_real_services(config: &[(&str, &str)]) -> Wafer {
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("Wafer::build");

    let sqlite = Arc::new(SQLiteDatabaseService::open_in_memory().expect("open in-memory sqlite"));
    register_with_tables(&mut wafer, sqlite, vec![]).expect("register wafer-run/database");

    wafer_core::service_blocks::storage::register_with(&mut wafer, Arc::new(MemStorage::default()))
        .expect("register wafer-run/storage");

    let cfg = EnvConfigService::new();
    for (k, v) in config {
        wafer_core::interfaces::config::service::ConfigService::set(&cfg, k, v);
    }
    wafer_core::service_blocks::config::register_with(&mut wafer, Arc::new(cfg))
        .expect("register wafer-run/config");

    wafer
}

/// Register the guest, start the runtime, and drive one `kind` through it.
async fn run(kind: &str) -> wafer_block::streams::output::BufferedResponse {
    let mut wafer = build_wafer_with_real_services(&[(CONFIG_KEY, CONFIG_VALUE)]).await;
    let block = WasmiBlock::load_from_bytes(&guest_wasm()).expect("load json-host-guest wasm");
    wafer
        .register_block(GUEST, Arc::new(block))
        .expect("register json-host-guest");
    let wafer = wafer.start().await.expect("start runtime");
    wafer
        .run_block(GUEST, Message::new(kind), InputStream::empty())
        .await
        .collect_buffered()
        .await
        .expect("buffered response")
}

#[tokio::test]
async fn json_guest_creates_its_table_and_reads_back_a_record() {
    let out = run("test.roundtrip").await;
    let v: serde_json::Value = serde_json::from_slice(&out.body)
        .unwrap_or_else(|e| panic!("get frame is not JSON ({e}): {:?}", out.body));
    assert_eq!(v["id"], "n1", "wire::Record.id (frame: {v})");
    assert_eq!(v["data"]["body"], "hello", "wire::Record.data (frame: {v})");
}

/// `storage.put` then `storage.get` over JSON, asserting BOTH frames of the
/// two-frame response: the `ObjectInfo` header (a wire DTO, transcoded to
/// JSON) and the object body (raw bytes, passed through verbatim because the
/// handler marked the body frames `frame.encoding = raw`).
///
/// The guest returns them joined by a newline. Together they prove the whole
/// path: the JSON `PutRequest` the guest wrote was transcoded, authorized
/// against a FOLDER-level `storage_folders` grant, and stored; then read back
/// with the header transcoded and the body left alone.
#[tokio::test]
async fn json_guest_storage_round_trip() {
    let out = run("test.storage").await;
    let sep = out
        .body
        .iter()
        .position(|b| *b == b'\n')
        .unwrap_or_else(|| {
            panic!(
                "expected `header\\nbody`, got: {:?}",
                String::from_utf8_lossy(&out.body)
            )
        });
    let (header, body) = (&out.body[..sep], &out.body[sep + 1..]);

    let v: serde_json::Value = serde_json::from_slice(header)
        .unwrap_or_else(|e| panic!("storage.get header frame is not JSON ({e}): {header:?}"));
    assert_eq!(v["key"], "a.txt", "ObjectInfo.key (frame: {v})");
    assert_eq!(v["size"], 2, "ObjectInfo.size — the guest PUT two bytes");
    assert_eq!(v["content_type"], "text/plain", "ObjectInfo.content_type");

    assert_eq!(
        body,
        [104u8, 105],
        "the raw body frame must reach the JSON guest verbatim (b\"hi\")"
    );
}

/// C1 regression, end to end: the guest holds the FOLDER
/// `test/json-host-guest` and asks `storage.get` for key `../../other`. The
/// composed resource (`test/json-host-guest/../../other`) sits textually
/// beneath the grant, so nothing in the capability match would stop it — the
/// storage handler refuses the unnormalized path outright, before
/// authorization, and the JSON guest sees `InvalidArgument`.
#[tokio::test]
async fn json_guest_cannot_escape_its_storage_folder() {
    let out = run("test.storage_escape").await;
    let v: serde_json::Value = serde_json::from_slice(&out.body)
        .unwrap_or_else(|e| panic!("refusal is not JSON ({e}): {:?}", out.body));
    assert_eq!(
        v["code"], "InvalidArgument",
        "a `..` key must be refused as a malformed request (error: {v})"
    );
    assert!(
        v["message"]
            .as_str()
            .is_some_and(|m| m.contains("storage.get")),
        "the refusal must name the op it rejected (error: {v})"
    );
}

#[tokio::test]
async fn json_guest_reads_config() {
    let out = run("test.config").await;
    let v: serde_json::Value = serde_json::from_slice(&out.body)
        .unwrap_or_else(|e| panic!("config.get frame is not JSON ({e}): {:?}", out.body));
    assert_eq!(v["value"], CONFIG_VALUE, "config GetResponse.value");
}

#[tokio::test]
async fn json_guest_receives_errors_as_json() {
    let out = run("test.error").await;
    let v: serde_json::Value = serde_json::from_slice(&out.body)
        .unwrap_or_else(|e| panic!("take_error payload is not JSON ({e}): {:?}", out.body));
    assert_eq!(v["code"], "NotFound", "WaferError.code (error: {v})");
}

#[tokio::test]
async fn json_guest_cannot_attach() {
    let out = run("test.attach").await;
    let code = wafer_block::ErrorCode::InvalidArgument;
    assert_eq!(
        String::from_utf8_lossy(&out.body),
        format!("attach={}", -(code as i32)),
        "stream_attach must refuse a JSON-codec guest with the InvalidArgument sentinel"
    );
}

/// Same guest wasm, but an operator capability override narrows `collections`
/// to a table the guest does not use. Declared ∩ override is empty, so the
/// very first schema op is capability-denied.
///
/// The narrowing goes through block config, not `WasmiBlock::load_with_
/// capabilities`: `Wafer::start()` recomputes effective capabilities as
/// declared ∩ config and pushes them into the block, so any set passed at load
/// time is overwritten by the guest's own `BlockInfo` declaration.
#[tokio::test]
async fn json_guest_cannot_touch_another_table() {
    let mut wafer = build_wafer_with_real_services(&[]).await;
    let block = WasmiBlock::load_from_bytes(&guest_wasm()).expect("load json-host-guest wasm");
    wafer
        .register_block(GUEST, Arc::new(block))
        .expect("register json-host-guest");
    wafer.add_block_config(
        GUEST,
        serde_json::json!({
            "capabilities": { "collections": { "Only": ["test__json_host_guest__other"] } }
        }),
    );
    let wafer = wafer.start().await.expect("start runtime");

    let out = wafer
        .run_block(GUEST, Message::new("test.roundtrip"), InputStream::empty())
        .await
        .collect_buffered()
        .await
        .expect("buffered response");
    let v: serde_json::Value = serde_json::from_slice(&out.body)
        .unwrap_or_else(|e| panic!("denial is not JSON ({e}): {:?}", out.body));
    assert_eq!(
        v["code"], "PermissionDenied",
        "WaferError.code (error: {v})"
    );
}
