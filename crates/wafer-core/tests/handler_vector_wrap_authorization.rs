//! Authorization tests for the vector service handler.
//!
//! The vector handler routes every op through `decode_and_authorize` keyed on
//! the decoded index name (`ResourceType::Vector`). These tests drive the
//! handler with fake `Context`s that allow / deny / allow-reads-only, and a
//! recording fake `VectorService`, asserting that denied ops never reach the
//! service and that a read-only grant cannot satisfy a write op.

use wafer_block::{
    codec,
    common::ServiceOp,
    context::Context,
    streams::{input::InputStream, output::OutputStream},
    types::{ResourceAccess, ResourceType},
    wire::vector as wire,
    ErrorCode, Message, WaferError,
};
use wafer_core::interfaces::vector::handler::handle_message;

// --- Context stubs -------------------------------------------------------

/// Grants every resource access check.
struct AllowCtx;
/// Denies every resource access check.
struct DenyCtx;
/// Grants reads, denies writes — exercises the write-flag path.
struct ReadOnlyCtx;

macro_rules! ctx_boilerplate {
    ($name:ty) => {
        #[wafer_block::wafer_async_trait]
        impl Context for $name {
            async fn call_block(
                &self,
                _block_name: &str,
                _msg: Message,
                _input: InputStream,
            ) -> OutputStream {
                unimplemented!("not exercised by decode_and_authorize")
            }
            fn is_cancelled(&self) -> bool {
                false
            }
            fn config_get(&self, _key: &str) -> Option<&str> {
                None
            }
            fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
                unimplemented!("not exercised by decode_and_authorize")
            }
            fn check_resource_access(
                &self,
                resource: &str,
                _resource_type: ResourceType,
                access: ResourceAccess,
            ) -> Result<(), WaferError> {
                check_impl::<$name>(resource, access)
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
    };
}

trait Policy {
    fn allow(access: ResourceAccess) -> bool;
}
impl Policy for AllowCtx {
    fn allow(_access: ResourceAccess) -> bool {
        true
    }
}
impl Policy for DenyCtx {
    fn allow(_access: ResourceAccess) -> bool {
        false
    }
}
impl Policy for ReadOnlyCtx {
    fn allow(access: ResourceAccess) -> bool {
        access == ResourceAccess::Read
    }
}

fn check_impl<P: Policy>(resource: &str, access: ResourceAccess) -> Result<(), WaferError> {
    if P::allow(access) {
        Ok(())
    } else {
        Err(WaferError::new(
            ErrorCode::PermissionDenied,
            format!("WRAP: denied for resource '{resource}' (access={access})"),
        ))
    }
}

ctx_boilerplate!(AllowCtx);
ctx_boilerplate!(DenyCtx);
ctx_boilerplate!(ReadOnlyCtx);

// --- Recording fake vector service --------------------------------------

mod vec_fakes {
    use std::sync::{Arc, Mutex};

    use wafer_block::wire::vector::{
        DescribeIndexResponse, MetadataFilter, SearchMode, VectorEntry, VectorIndexConfig,
        VectorMatch,
    };
    use wafer_core::interfaces::vector::service::{Result as VResult, VectorService};

    #[derive(Clone, Default)]
    pub struct Recording {
        pub calls: Arc<Mutex<Vec<String>>>,
    }
    impl Recording {
        fn note(&self, s: &str) {
            self.calls.lock().unwrap().push(s.to_string());
        }
        pub fn ran(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[wafer_block::wafer_async_trait]
    impl VectorService for Recording {
        async fn create_index(&self, _c: VectorIndexConfig) -> VResult<()> {
            self.note("create_index");
            Ok(())
        }
        async fn delete_index(&self, _n: &str) -> VResult<()> {
            self.note("delete_index");
            Ok(())
        }
        async fn upsert(&self, _i: &str, _e: Vec<VectorEntry>) -> VResult<()> {
            self.note("upsert");
            Ok(())
        }
        async fn query(
            &self,
            _i: &str,
            _v: Vec<f32>,
            _k: usize,
            _f: Option<MetadataFilter>,
            _m: SearchMode,
            _q: Option<String>,
        ) -> VResult<Vec<VectorMatch>> {
            self.note("query");
            Ok(vec![])
        }
        async fn delete(&self, _i: &str, _ids: Vec<String>) -> VResult<()> {
            self.note("delete");
            Ok(())
        }
        async fn count(&self, _i: &str) -> VResult<u64> {
            self.note("count");
            Ok(0)
        }
        async fn list_indexes(&self, _p: &str) -> VResult<Vec<String>> {
            self.note("list_indexes");
            Ok(vec![])
        }
        async fn describe_index(&self, _i: &str) -> VResult<DescribeIndexResponse> {
            self.note("describe_index");
            Ok(DescribeIndexResponse {
                exists: false,
                columns: vec![],
                keyword_search: false,
            })
        }
        async fn list_ids(&self, _i: &str, _f: MetadataFilter) -> VResult<Vec<String>> {
            self.note("list_ids");
            Ok(vec![])
        }
        async fn rename_index(&self, _from: &str, _to: &str) -> VResult<()> {
            self.note("rename_index");
            Ok(())
        }
    }
}

// --- Helpers -------------------------------------------------------------

const IDX: &str = "my_org__vector__docs";
/// `IDX` as an index created before index names had to be lowercase.
const LEGACY_IDX: &str = "my_org__vector__Docs";

fn msg(kind: &str) -> Message {
    Message::new(kind)
}

async fn terminal_err(out: OutputStream) -> Option<WaferError> {
    match out.collect_buffered().await {
        Ok(_) => None,
        Err(wafer_block::streams::output::TerminalNotResponse::Error(e)) => Some(e),
        _ => None,
    }
}

fn body_for(op: &str) -> Vec<u8> {
    let encoded = match op {
        ServiceOp::VECTOR_CREATE_INDEX => codec::encode(&wire::CreateIndexRequest {
            config: wire::VectorIndexConfig {
                name: IDX.into(),
                model: "m".into(),
                dimensions: 3,
                metric: wire::DistanceMetric::Cosine,
                keyword_search: false,
            },
        }),
        ServiceOp::VECTOR_DELETE_INDEX => {
            codec::encode(&wire::DeleteIndexRequest { name: IDX.into() })
        }
        ServiceOp::VECTOR_UPSERT => codec::encode(&wire::UpsertRequest {
            index: IDX.into(),
            entries: vec![],
        }),
        ServiceOp::VECTOR_QUERY => codec::encode(&wire::QueryRequest {
            index: IDX.into(),
            vector: vec![],
            top_k: 1,
            filter: None,
            mode: wire::SearchMode::Vector,
            keyword_query: None,
        }),
        ServiceOp::VECTOR_DELETE => codec::encode(&wire::DeleteRequest {
            index: IDX.into(),
            ids: vec![],
        }),
        ServiceOp::VECTOR_COUNT => codec::encode(&wire::CountRequest { index: IDX.into() }),
        ServiceOp::VECTOR_LIST_INDEXES => codec::encode(&wire::ListIndexesRequest {
            // The namespace prefix of IDX — list authorizes on the prefix itself.
            prefix: "my_org__vector__".into(),
        }),
        ServiceOp::VECTOR_DESCRIBE_INDEX => {
            codec::encode(&wire::DescribeIndexRequest { index: IDX.into() })
        }
        ServiceOp::VECTOR_LIST_IDS => codec::encode(&wire::ListIdsRequest {
            index: IDX.into(),
            filter: wire::MetadataFilter::default(),
        }),
        ServiceOp::VECTOR_RENAME_INDEX => codec::encode(&wire::RenameIndexRequest {
            from: LEGACY_IDX.into(),
            to: IDX.into(),
        }),
        other => panic!("no body for op {other}"),
    };
    encoded.expect("encode")
}

const ALL_OPS: &[&str] = &[
    ServiceOp::VECTOR_CREATE_INDEX,
    ServiceOp::VECTOR_DELETE_INDEX,
    ServiceOp::VECTOR_UPSERT,
    ServiceOp::VECTOR_QUERY,
    ServiceOp::VECTOR_DELETE,
    ServiceOp::VECTOR_COUNT,
    ServiceOp::VECTOR_LIST_INDEXES,
    ServiceOp::VECTOR_DESCRIBE_INDEX,
    ServiceOp::VECTOR_LIST_IDS,
    ServiceOp::VECTOR_RENAME_INDEX,
];
const WRITE_OPS: &[&str] = &[
    ServiceOp::VECTOR_CREATE_INDEX,
    ServiceOp::VECTOR_DELETE_INDEX,
    ServiceOp::VECTOR_UPSERT,
    ServiceOp::VECTOR_DELETE,
    ServiceOp::VECTOR_RENAME_INDEX,
];
const READ_OPS: &[&str] = &[
    ServiceOp::VECTOR_QUERY,
    ServiceOp::VECTOR_COUNT,
    ServiceOp::VECTOR_LIST_INDEXES,
    ServiceOp::VECTOR_DESCRIBE_INDEX,
    ServiceOp::VECTOR_LIST_IDS,
];

// --- Tests ---------------------------------------------------------------

#[tokio::test]
async fn every_op_denied_never_reaches_service() {
    for op in ALL_OPS {
        let svc = vec_fakes::Recording::default();
        let out = handle_message(&svc, &DenyCtx, &msg(op), &body_for(op)).await;
        let err = terminal_err(out)
            .await
            .unwrap_or_else(|| panic!("op {op} should be denied"));
        assert_eq!(err.code, ErrorCode::PermissionDenied, "op {op}");
        assert!(
            svc.ran().is_empty(),
            "op {op} must not reach the service on deny; ran = {:?}",
            svc.ran()
        );
    }
}

#[tokio::test]
async fn every_op_allowed_reaches_service() {
    for op in ALL_OPS {
        let svc = vec_fakes::Recording::default();
        let out = handle_message(&svc, &AllowCtx, &msg(op), &body_for(op)).await;
        assert!(
            terminal_err(out).await.is_none(),
            "op {op} should succeed under AllowCtx"
        );
        assert_eq!(svc.ran().len(), 1, "op {op} should reach the service once");
    }
}

#[tokio::test]
async fn read_only_grant_denies_write_ops() {
    for op in WRITE_OPS {
        let svc = vec_fakes::Recording::default();
        let out = handle_message(&svc, &ReadOnlyCtx, &msg(op), &body_for(op)).await;
        let err = terminal_err(out)
            .await
            .unwrap_or_else(|| panic!("write op {op} must be denied by a read-only grant"));
        assert_eq!(err.code, ErrorCode::PermissionDenied, "op {op}");
        assert!(svc.ran().is_empty(), "write op {op} must not run");
    }
}

#[tokio::test]
async fn read_only_grant_allows_read_ops() {
    for op in READ_OPS {
        let svc = vec_fakes::Recording::default();
        let out = handle_message(&svc, &ReadOnlyCtx, &msg(op), &body_for(op)).await;
        assert!(
            terminal_err(out).await.is_none(),
            "read op {op} should succeed under a read-only grant"
        );
        assert_eq!(svc.ran().len(), 1, "read op {op} should reach the service");
    }
}

/// Every op is listed in exactly one of the read and write sets, so the
/// grant-shape tests above cover every op.
#[test]
fn every_op_is_classified_read_or_write() {
    for op in ServiceOp::VECTOR_OPS {
        assert!(ALL_OPS.contains(op), "op {op} missing from ALL_OPS");
        assert!(
            WRITE_OPS.contains(op) != READ_OPS.contains(op),
            "op {op} must be in exactly one of WRITE_OPS / READ_OPS"
        );
    }
}

// --- rename_index: both names are authorized -----------------------------

/// Grants everything except writes to one named resource.
struct DenyWriteTo(&'static str);

#[wafer_block::wafer_async_trait]
impl Context for DenyWriteTo {
    async fn call_block(
        &self,
        _block_name: &str,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        unimplemented!("not exercised by decode_and_authorize")
    }
    fn is_cancelled(&self) -> bool {
        false
    }
    fn config_get(&self, _key: &str) -> Option<&str> {
        None
    }
    fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
        unimplemented!("not exercised by decode_and_authorize")
    }
    fn check_resource_access(
        &self,
        resource: &str,
        _resource_type: ResourceType,
        access: ResourceAccess,
    ) -> Result<(), WaferError> {
        if resource == self.0 && access == ResourceAccess::Write {
            Err(WaferError::new(
                ErrorCode::PermissionDenied,
                format!("WRAP: denied for resource '{resource}' (access={access})"),
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

/// A rename empties one index and fills another, so a caller needs write
/// access to both: denying either name alone refuses the op before it
/// reaches the service.
#[tokio::test]
async fn rename_index_needs_write_access_to_both_names() {
    for denied in [LEGACY_IDX, IDX] {
        let svc = vec_fakes::Recording::default();
        let op = ServiceOp::VECTOR_RENAME_INDEX;
        let out = handle_message(&svc, &DenyWriteTo(denied), &msg(op), &body_for(op)).await;
        let err = terminal_err(out)
            .await
            .unwrap_or_else(|| panic!("rename must be denied when {denied} is not writable"));
        assert_eq!(err.code, ErrorCode::PermissionDenied, "denied {denied}");
        assert!(err.message.contains(denied), "{}", err.message);
        assert!(svc.ran().is_empty(), "denied {denied}: ran {:?}", svc.ran());
    }
}

/// Names that break the rename rule are refused as malformed before any
/// authorization — the same answer with or without grants — and never
/// reach the service.
#[tokio::test]
async fn rename_index_refuses_non_legacy_names_before_authorizing() {
    for (from, to) in [
        (IDX, IDX),
        (LEGACY_IDX, "my_org__vector__other"),
        ("my_org__vector__Docs", "my_org__vector__Docs"),
        ("my-org__vector__Docs", "my-org__vector__docs"),
    ] {
        let body = codec::encode(&wire::RenameIndexRequest {
            from: from.into(),
            to: to.into(),
        })
        .expect("encode");
        let svc = vec_fakes::Recording::default();
        let out = handle_message(&svc, &DenyCtx, &msg(ServiceOp::VECTOR_RENAME_INDEX), &body).await;
        let err = terminal_err(out)
            .await
            .unwrap_or_else(|| panic!("{from} -> {to} must be refused"));
        assert_eq!(
            err.code,
            ErrorCode::InvalidArgument,
            "{from} -> {to}: {}",
            err.message
        );
        assert!(svc.ran().is_empty(), "{from} -> {to}: ran {:?}", svc.ran());
    }
}
