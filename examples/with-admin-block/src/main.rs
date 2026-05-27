//! Demonstrates the production pattern for typed-resource grants.
//!
//! An admin block declares a typed Storage grant in `BlockInfo::grants`.
//! The application calls `set_admin_block(...)` so the runtime accepts
//! those typed grants. A separate feature block consumes the resource.
//!
//! Production reference: `solobase-core/src/builder.rs:309` calls
//! `wafer.set_admin_block("suppers-ai/admin")` for exactly this reason
//! — the admin block owns the typed Storage grant the `files` feature
//! block needs.
//!
//! Run with: cargo run -p with-admin-block
//! Test with: curl http://localhost:8080/

use std::sync::Arc;

use wafer_run::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,wafer=debug")),
        )
        .init();

    let mut wafer = Wafer::new(Arc::new(StaticConfigSource::default()))?;

    // 1. Register the admin block. Its `BlockInfo::grants` declares
    //    the typed Storage grant the feature block needs at runtime.
    //    Call order relative to other `register_block` calls doesn't
    //    matter — see step 2 for the load-bearing constraint.
    wafer.register_block("example/admin", Arc::new(AdminBlock))?;

    // 2. `set_admin_block` admits typed grants declared on the named
    //    block. Without this call, the typed Storage grant would be
    //    rejected at `seal()` with `RuntimeError::GrantsRejected`
    //    — see Wave 13 PR B (wafer-run PR #166).
    wafer.set_admin_block("example/admin");

    // 3. Register the feature block that actually consumes Storage.
    wafer.register_block("example/folder-lister", Arc::new(FolderListerBlock))?;

    // 4. Wire HTTP + local storage as usual.
    wafer.add_flow_json(wafer_flow_http_server::FLOW_JSON)?;
    wafer.add_block_config(
        wafer_flow_http_server::FLOW_ID,
        serde_json::json!({
            "listen": "0.0.0.0:8080",
            "routes": [{ "path": "/", "block": "example/folder-lister" }]
        }),
    );
    wafer_core::service_blocks::storage::register_with(
        &mut wafer,
        Arc::new(
            wafer_block_local_storage::service::LocalStorageService::new(".")
                .expect("local storage root"),
        ),
    )?;

    tracing::info!("listening on http://localhost:8080");
    let wafer = wafer.start().await?;

    tokio::signal::ctrl_c().await.ok();
    wafer.shutdown().await;
    Ok(())
}

/// Admin block — only purpose here is to declare the typed Storage
/// grant. In production (see solobase) the admin block also serves
/// the admin UI; this example keeps it grant-only for clarity.
struct AdminBlock;

#[async_trait::async_trait]
impl Block for AdminBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("example/admin", "0.0.1", "admin@v1", "Admin")
            .instance_mode(InstanceMode::Singleton)
            .grants(vec![
                ResourceGrant::read("wafer-run/storage", "*").typed(ResourceType::Storage)
            ])
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::respond(Vec::new())
    }
}

/// Feature block that exercises the typed Storage grant via
/// `list_folders`. The call is admitted by the admin block's grant
/// — without `set_admin_block("example/admin")` above, this would
/// fail at WRAP-check time.
struct FolderListerBlock;

#[async_trait::async_trait]
impl Block for FolderListerBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "example/folder-lister",
            "0.0.1",
            "http-handler@v1",
            "Folder lister",
        )
        .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        match wafer_core::clients::storage::list_folders(ctx).await {
            Ok(folders) => {
                let body = serde_json::to_vec(&serde_json::json!({
                    "folders": folders.iter().map(|f| &f.name).collect::<Vec<_>>(),
                }))
                .unwrap_or_default();
                OutputStream::respond(body)
            }
            Err(e) => OutputStream::error(WaferError::new(
                ErrorCode::Internal,
                format!("storage list_folders failed: {e}"),
            )),
        }
    }
}
