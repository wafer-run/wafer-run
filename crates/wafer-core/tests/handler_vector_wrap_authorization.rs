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
    types::ResourceType,
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
                is_write: bool,
            ) -> Result<(), WaferError> {
                check_impl::<$name>(resource, is_write)
            }
        }
    };
}

trait Policy {
    fn allow(is_write: bool) -> bool;
}
impl Policy for AllowCtx {
    fn allow(_is_write: bool) -> bool {
        true
    }
}
impl Policy for DenyCtx {
    fn allow(_is_write: bool) -> bool {
        false
    }
}
impl Policy for ReadOnlyCtx {
    fn allow(is_write: bool) -> bool {
        !is_write
    }
}

fn check_impl<P: Policy>(resource: &str, is_write: bool) -> Result<(), WaferError> {
    if P::allow(is_write) {
        Ok(())
    } else {
        Err(WaferError::new(
            ErrorCode::PermissionDenied,
            format!("WRAP: denied for resource '{resource}' (write={is_write})"),
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
        MetadataFilter, SearchMode, VectorEntry, VectorIndexConfig, VectorMatch,
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
    }
}

// --- Helpers -------------------------------------------------------------

const IDX: &str = "suppers_ai__vector__docs";

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
];
const WRITE_OPS: &[&str] = &[
    ServiceOp::VECTOR_CREATE_INDEX,
    ServiceOp::VECTOR_DELETE_INDEX,
    ServiceOp::VECTOR_UPSERT,
    ServiceOp::VECTOR_DELETE,
];
const READ_OPS: &[&str] = &[ServiceOp::VECTOR_QUERY, ServiceOp::VECTOR_COUNT];

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
