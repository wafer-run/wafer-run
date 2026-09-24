//! A block's storage is its own: driven end to end, each caller block sends
//! the request bytes a `storage::*` client sends through `ctx.call_block` to
//! the REAL `wafer-run/storage` block (the shared storage handler over an
//! in-memory service), which scopes the request and authorizes the caller
//! through the REAL `RuntimeContext::check_resource_access`.
//!
//! The service records the folder of every call it receives and is read
//! host-side (bypassing WRAP, as a test oracle), so each test can say both
//! what the caller was answered and which backend paths were touched.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use serde::Serialize;
use wafer_block::{
    codec,
    common::ServiceOp,
    core_types::{LifecycleEvent, Message, WaferError},
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    types::{ResourceGrant, ResourceType},
    wire::storage as wire,
    Block, BlockInfo, ErrorCode,
};
use wafer_core::interfaces::storage::service::{
    FolderInfo, ListOptions, ObjectInfo, ObjectList, StorageError, StorageService,
};
use wafer_run::{Context, Wafer};

/// Owns the `acme/victim/…` namespace; grants [`ATTACKER`] read access to
/// `acme/victim/shared/*` and nothing else.
const VICTIM: &str = "acme/victim";
/// Holds no grant on anything under [`VICTIM`] except the shared read.
const ATTACKER: &str = "acme/attacker";

/// The victim's object, seeded host-side before every test.
const SECRET_FOLDER: &str = "acme/victim/secret";
const SECRET_KEY: &str = "doc.txt";
const SECRET_BODY: &[u8] = b"victim's secret";
/// An object the victim shares read-only with the attacker.
const SHARED_FOLDER: &str = "acme/victim/shared";
const SHARED_KEY: &str = "readme.txt";

/// In-memory storage keyed by the backend path `{folder}/{key}`, recording
/// the op and folder of every call the handler makes.
#[derive(Default)]
struct MemStorage {
    objects: Mutex<BTreeMap<String, Vec<u8>>>,
    calls: Mutex<Vec<(&'static str, String)>>,
}

impl MemStorage {
    fn record(&self, op: &'static str, folder: &str) {
        self.calls.lock().unwrap().push((op, folder.to_string()));
    }
    fn calls(&self) -> Vec<(&'static str, String)> {
        self.calls.lock().unwrap().clone()
    }
    fn clear_calls(&self) {
        self.calls.lock().unwrap().clear();
    }
    fn object(&self, folder: &str, key: &str) -> Option<Vec<u8>> {
        self.objects
            .lock()
            .unwrap()
            .get(&format!("{folder}/{key}"))
            .cloned()
    }
    fn seed(&self, folder: &str, key: &str, body: &[u8]) {
        self.objects
            .lock()
            .unwrap()
            .insert(format!("{folder}/{key}"), body.to_vec());
    }
}

fn info(key: &str, size: usize) -> ObjectInfo {
    ObjectInfo {
        key: key.to_string(),
        size: size as i64,
        content_type: "text/plain".to_string(),
        last_modified: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap(),
    }
}

#[async_trait]
impl StorageService for MemStorage {
    async fn put(
        &self,
        folder: &str,
        key: &str,
        data: &[u8],
        _content_type: &str,
    ) -> Result<(), StorageError> {
        self.record("put", folder);
        self.seed(folder, key, data);
        Ok(())
    }
    async fn get(&self, folder: &str, key: &str) -> Result<(Vec<u8>, ObjectInfo), StorageError> {
        self.record("get", folder);
        let data = self.object(folder, key).ok_or(StorageError::NotFound)?;
        let size = data.len();
        Ok((data, info(key, size)))
    }
    async fn delete(&self, folder: &str, key: &str) -> Result<(), StorageError> {
        self.record("delete", folder);
        self.objects
            .lock()
            .unwrap()
            .remove(&format!("{folder}/{key}"));
        Ok(())
    }
    async fn list(&self, folder: &str, _opts: &ListOptions) -> Result<ObjectList, StorageError> {
        self.record("list", folder);
        let prefix = format!("{folder}/");
        let objects: Vec<ObjectInfo> = self
            .objects
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(path, data)| path.strip_prefix(&prefix).map(|key| info(key, data.len())))
            .collect();
        let total_count = objects.len() as i64;
        Ok(ObjectList {
            objects,
            total_count,
            next_cursor: None,
        })
    }
    async fn create_folder(&self, name: &str, _public: bool) -> Result<(), StorageError> {
        self.record("create_folder", name);
        Ok(())
    }
    async fn delete_folder(&self, name: &str) -> Result<(), StorageError> {
        self.record("delete_folder", name);
        let prefix = format!("{name}/");
        self.objects
            .lock()
            .unwrap()
            .retain(|path, _| !path.starts_with(&prefix));
        Ok(())
    }
    async fn list_folders(&self) -> Result<Vec<FolderInfo>, StorageError> {
        self.record("list_folders", "");
        Ok(Vec::new())
    }
}

/// A caller: forwards the storage request it is handed to `wafer-run/storage`
/// from its own context, so the storage handler sees this block as the caller
/// — exactly what a block's `storage::*` client call does.
struct Caller {
    name: &'static str,
    grants: Vec<ResourceGrant>,
}

#[async_trait]
impl Block for Caller {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(self.name, "0.1.0", "test/iface@v1", "calls storage")
            .grants(self.grants.clone())
    }
    async fn lifecycle(&self, _ctx: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
    async fn handle(&self, ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        ctx.call_block("wafer-run/storage", Message::new(msg.kind), input)
            .await
    }
}

async fn build() -> (Arc<Wafer>, Arc<MemStorage>) {
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("Wafer::build");
    let storage = Arc::new(MemStorage::default());
    storage.seed(SECRET_FOLDER, SECRET_KEY, SECRET_BODY);
    storage.seed(SHARED_FOLDER, SHARED_KEY, b"shared");
    wafer_core::service_blocks::storage::register_with(&mut wafer, storage.clone())
        .expect("register wafer-run/storage");
    wafer
        .register_block(
            VICTIM,
            Arc::new(Caller {
                name: VICTIM,
                grants: vec![ResourceGrant::read(ATTACKER, "acme/victim/shared/*")
                    .typed(ResourceType::Storage)],
            }),
        )
        .expect("register victim");
    wafer
        .register_block(
            ATTACKER,
            Arc::new(Caller {
                name: ATTACKER,
                grants: Vec::new(),
            }),
        )
        .expect("register attacker");
    wafer.seal().await.expect("seal");
    (Arc::new(wafer), storage)
}

/// Run `op` with `request` as `caller`: the response body, or the code
/// storage refused it with.
async fn call(
    wafer: &Wafer,
    caller: &str,
    op: &str,
    request: &impl Serialize,
) -> Result<Vec<u8>, ErrorCode> {
    let body = codec::encode(request).expect("encode request");
    let out = wafer
        .run_block(caller, Message::new(op), InputStream::from_bytes(body))
        .await;
    match out.collect_buffered().await {
        Ok(resp) => Ok(resp.body),
        Err(TerminalNotResponse::Error(e)) => Err(e.code),
        Err(other) => panic!("{op}: no response or error: {other:?}"),
    }
}

fn get(folder: &str, key: &str) -> wire::GetRequest {
    wire::GetRequest {
        folder: folder.into(),
        key: key.into(),
    }
}

fn put(folder: &str, key: &str, data: &[u8]) -> wire::PutRequest {
    wire::PutRequest {
        folder: folder.into(),
        key: key.into(),
        data: data.to_vec(),
        content_type: "text/plain".into(),
    }
}

fn delete(folder: &str, key: &str) -> wire::DeleteRequest {
    wire::DeleteRequest {
        folder: folder.into(),
        key: key.into(),
    }
}

fn list(folder: &str) -> wire::ListRequest {
    wire::ListRequest {
        folder: folder.into(),
        prefix: String::new(),
        limit: 100,
        offset: 0,
        cursor: None,
    }
}

/// Every `storage.*` op that carries a folder, addressed at `folder`.
async fn every_folder_op(
    wafer: &Wafer,
    caller: &str,
    folder: &str,
) -> Vec<(&'static str, Result<Vec<u8>, ErrorCode>)> {
    vec![
        (
            "get",
            call(
                wafer,
                caller,
                ServiceOp::STORAGE_GET,
                &get(folder, SECRET_KEY),
            )
            .await,
        ),
        (
            "get_streaming",
            call(
                wafer,
                caller,
                ServiceOp::STORAGE_GET_STREAMING,
                &get(folder, SECRET_KEY),
            )
            .await,
        ),
        (
            "list",
            call(wafer, caller, ServiceOp::STORAGE_LIST, &list(folder)).await,
        ),
        (
            "put",
            call(
                wafer,
                caller,
                ServiceOp::STORAGE_PUT,
                &put(folder, SECRET_KEY, b"overwritten"),
            )
            .await,
        ),
        (
            "delete",
            call(
                wafer,
                caller,
                ServiceOp::STORAGE_DELETE,
                &delete(folder, SECRET_KEY),
            )
            .await,
        ),
        (
            "create_folder",
            call(
                wafer,
                caller,
                ServiceOp::STORAGE_CREATE_FOLDER,
                &wire::CreateFolderRequest {
                    name: folder.into(),
                    public: true,
                },
            )
            .await,
        ),
        (
            "delete_folder",
            call(
                wafer,
                caller,
                ServiceOp::STORAGE_DELETE_FOLDER,
                &wire::DeleteFolderRequest {
                    name: folder.into(),
                },
            )
            .await,
        ),
    ]
}

/// The finding: a plain folder that spells another block's namespace used to
/// reach that namespace, because WRAP admitted every plain path and nothing
/// scoped it. It now resolves inside the caller's own namespace, so every op
/// — read, write, delete, folder delete — touches only `acme/attacker/…` and
/// the victim's object is neither read nor changed.
#[tokio::test]
async fn a_plain_folder_naming_another_block_stays_in_the_callers_namespace() {
    let (wafer, storage) = build().await;

    let results = every_folder_op(&wafer, ATTACKER, SECRET_FOLDER).await;

    for (op, result) in &results {
        if let Ok(body) = result {
            assert!(
                !body.windows(SECRET_BODY.len()).any(|w| w == SECRET_BODY),
                "{op}: the attacker was answered with the victim's object"
            );
        }
    }
    assert_eq!(
        results[0].1,
        Err(ErrorCode::NotFound),
        "get: `{SECRET_FOLDER}` from {ATTACKER} is its own (empty) folder"
    );
    for (op, folder) in storage.calls() {
        assert!(
            folder.starts_with("acme/attacker/"),
            "{op} reached the backend at {folder:?}, outside the caller's namespace"
        );
    }
    assert_eq!(
        storage.object(SECRET_FOLDER, SECRET_KEY).as_deref(),
        Some(SECRET_BODY),
        "the victim's object must be untouched by the attacker's writes and deletes"
    );
}

/// The explicit form of the same reach is refused for every op, and the
/// service never runs. Passes before the fix too — the `@` path was always
/// grant-checked — and stays here as the guard beside the plain-folder case.
#[tokio::test]
async fn an_explicit_path_into_another_block_needs_a_grant() {
    let (wafer, storage) = build().await;

    let explicit = format!("@{SECRET_FOLDER}");
    for (op, result) in every_folder_op(&wafer, ATTACKER, &explicit).await {
        assert_eq!(
            result.map(|_| ()),
            Err(ErrorCode::PermissionDenied),
            "{op} on {explicit} from {ATTACKER}"
        );
    }
    assert_eq!(
        storage.calls(),
        Vec::new(),
        "no denied op may reach the service"
    );
    assert_eq!(
        storage.object(SECRET_FOLDER, SECRET_KEY).as_deref(),
        Some(SECRET_BODY)
    );
}

/// A Storage grant admits exactly what it names: the attacker may read the
/// shared object by its explicit path, and may not write it.
#[tokio::test]
async fn a_grant_admits_the_explicit_path_it_names() {
    let (wafer, storage) = build().await;
    let shared = format!("@{SHARED_FOLDER}");

    let body = call(
        &wafer,
        ATTACKER,
        ServiceOp::STORAGE_GET,
        &get(&shared, SHARED_KEY),
    )
    .await
    .expect("the read grant admits the shared object");
    assert!(body.ends_with(b"shared"), "got {body:?}");
    assert_eq!(
        storage.calls(),
        vec![("get", SHARED_FOLDER.to_string())],
        "the backend is handed the path without its `@`"
    );
    storage.clear_calls();

    assert_eq!(
        call(
            &wafer,
            ATTACKER,
            ServiceOp::STORAGE_PUT,
            &put(&shared, SHARED_KEY, b"x")
        )
        .await
        .map(|_| ()),
        Err(ErrorCode::PermissionDenied),
        "a read grant does not admit a write"
    );
    assert_eq!(storage.calls(), Vec::new());
}

/// The owner reaches its own objects both ways: a plain folder resolves into
/// its namespace, and `@` naming its own namespace is the same path. The
/// empty folder is the namespace root.
#[tokio::test]
async fn the_owner_reaches_its_objects_by_plain_and_explicit_folder() {
    let (wafer, storage) = build().await;

    for folder in ["secret".to_string(), format!("@{SECRET_FOLDER}")] {
        let body = call(
            &wafer,
            VICTIM,
            ServiceOp::STORAGE_GET,
            &get(&folder, SECRET_KEY),
        )
        .await
        .unwrap_or_else(|code| panic!("{VICTIM} reading {folder:?}: {code:?}"));
        assert!(body.ends_with(SECRET_BODY), "{folder:?}: got {body:?}");
    }

    call(
        &wafer,
        VICTIM,
        ServiceOp::STORAGE_PUT,
        &put("", "root.txt", b"r"),
    )
    .await
    .expect("the owner writes its namespace root");
    assert_eq!(
        storage.object(VICTIM, "root.txt").as_deref(),
        Some(&b"r"[..])
    );
}

/// A folder or key that climbs out of the caller's namespace is refused as
/// malformed before it is authorized or reaches the service — whether the
/// climb starts from a plain folder or from an explicit one the caller owns.
#[tokio::test]
async fn a_path_that_climbs_out_of_the_namespace_is_refused() {
    let (wafer, storage) = build().await;

    let requests = [
        ("../victim/secret".to_string(), SECRET_KEY),
        ("x".to_string(), "../../victim/secret/doc.txt"),
        (format!("@{ATTACKER}/../victim/secret"), SECRET_KEY),
        ("a//b".to_string(), SECRET_KEY),
        ("@".to_string(), SECRET_KEY),
    ];
    for (folder, key) in requests {
        assert_eq!(
            call(&wafer, ATTACKER, ServiceOp::STORAGE_GET, &get(&folder, key))
                .await
                .map(|_| ()),
            Err(ErrorCode::InvalidArgument),
            "get {folder:?} / {key:?}"
        );
    }
    assert_eq!(storage.calls(), Vec::new());
}
