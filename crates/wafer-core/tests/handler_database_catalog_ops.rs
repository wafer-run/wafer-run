//! The catalog reads — `database.catalog_tables`, `catalog_columns` and
//! `catalog_row_count` — are authorized exactly as `database.query_raw` is:
//! one `Read` on the raw-SQL sentinel, which [`wrap::check_access`] admits for
//! the admin block alone. They name tables as the catalog spells them (`Foo`,
//! `a-b`), which no per-table grant can name, and together they reach every
//! table whatever its namespace, so a table-name grant — even a wildcard one,
//! even on the caller's own table — must not admit them.
//!
//! The contexts here decide with the real [`wrap::check_access`], so these
//! tests pin the gate the runtime applies, not a stand-in for it.

use std::sync::{Arc, Mutex};

use wafer_block::{
    codec,
    common::ServiceOp,
    context::Context,
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    types::{ResourceAccess, ResourceGrant, ResourceType},
    wire::database as wire,
    wrap::{self, RAW_SQL_RESOURCE},
    Message, WaferError,
};

mod common;

use common::db_fakes::{self, expect_permission_denied, msg_without_wrap_meta, new_calls};

const ADMIN: &str = "my-org/admin";

/// A `Context` whose every resource check is [`wrap::check_access`] for
/// `caller` under `grants`, with [`ADMIN`] as the admin block, and which
/// records each check it was asked for.
struct WrapCtx {
    caller: &'static str,
    grants: Vec<ResourceGrant>,
    asked: Mutex<Vec<(String, ResourceType, ResourceAccess)>>,
}

impl WrapCtx {
    fn new(caller: &'static str, grants: Vec<ResourceGrant>) -> Self {
        Self {
            caller,
            grants,
            asked: Mutex::new(Vec::new()),
        }
    }

    fn asked(&self) -> Vec<(String, ResourceType, ResourceAccess)> {
        self.asked.lock().unwrap().clone()
    }
}

#[wafer_block::wafer_async_trait]
impl Context for WrapCtx {
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
        resource_type: ResourceType,
        access: ResourceAccess,
    ) -> Result<(), WaferError> {
        self.asked
            .lock()
            .unwrap()
            .push((resource.to_string(), resource_type.clone(), access));
        wrap::check_access(
            Some(self.caller),
            resource,
            access,
            Some(&resource_type),
            &self.grants,
            ADMIN,
        )
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

/// Each catalog op with a request naming `table`.
fn catalog_requests(table: &str) -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            ServiceOp::DATABASE_CATALOG_TABLES,
            codec::encode(&wire::CatalogTablesRequest {}).unwrap(),
        ),
        (
            ServiceOp::DATABASE_CATALOG_COLUMNS,
            codec::encode(&wire::CatalogColumnsRequest {
                table: table.into(),
            })
            .unwrap(),
        ),
        (
            ServiceOp::DATABASE_CATALOG_ROW_COUNT,
            codec::encode(&wire::CatalogRowCountRequest {
                table: table.into(),
            })
            .unwrap(),
        ),
    ]
}

/// The service method each catalog op runs, as the recording fake logs it.
fn service_method(op: &str) -> &'static str {
    match op {
        ServiceOp::DATABASE_CATALOG_TABLES => "catalog_tables",
        ServiceOp::DATABASE_CATALOG_COLUMNS => "catalog_columns",
        ServiceOp::DATABASE_CATALOG_ROW_COUNT => "catalog_row_count",
        other => panic!("not a catalog op: {other}"),
    }
}

/// The admin block reaches each catalog op, and each asks for exactly one
/// check: a `Read` of the raw-SQL sentinel — never the table it names.
#[tokio::test]
async fn the_admin_block_reaches_each_catalog_op_through_one_raw_sql_read() {
    for (op, body) in catalog_requests("Odd-Name") {
        let calls = new_calls();
        let svc = db_fakes::RecordingDb::new(Arc::clone(&calls));
        let ctx = WrapCtx::new(ADMIN, Vec::new());
        let out = wafer_core::interfaces::database::handler::handle_message(
            &svc,
            &ctx,
            &msg_without_wrap_meta(op),
            &body,
        )
        .await;
        if let Err(TerminalNotResponse::Error(e)) = out.collect_buffered().await {
            panic!("{op}: expected success, got {:?}: {}", e.code, e.message);
        }
        assert_eq!(
            ctx.asked(),
            [(
                RAW_SQL_RESOURCE.to_string(),
                ResourceType::Db,
                ResourceAccess::Read
            )],
            "{op}"
        );
        assert_eq!(*calls.lock().unwrap(), [service_method(op)], "{op}");
    }
}

/// A block holding a wildcard read-write `Db` grant — and asking about its
/// own table, which it owns outright — is still refused every catalog op,
/// and the service never runs: switching the admin block onto these ops
/// widens no other block's reach.
#[tokio::test]
async fn a_table_grant_does_not_admit_a_catalog_op() {
    let grants = vec![ResourceGrant::read_write("my-org/auth", "*").typed(ResourceType::Db)];
    for (op, body) in catalog_requests("my_org__auth__widgets") {
        let calls = new_calls();
        let svc = db_fakes::RecordingDb::new(Arc::clone(&calls));
        let ctx = WrapCtx::new("my-org/auth", grants.clone());
        expect_permission_denied(
            wafer_core::interfaces::database::handler::handle_message(
                &svc,
                &ctx,
                &msg_without_wrap_meta(op),
                &body,
            )
            .await,
        )
        .await;
        assert!(
            calls.lock().unwrap().is_empty(),
            "{op} must not reach the service; calls = {:?}",
            calls.lock().unwrap()
        );
    }
}
