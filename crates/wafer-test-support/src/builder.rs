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
        Self {
            wafer: Wafer::builder()
                .disable_inventory()
                .disable_lockfile()
                .build()
                .expect("empty wafer build is infallible"),
        }
    }

    /// Register `FakeDb` at `test/fake-db` and alias `wafer-run/database`
    /// so production code (`ctx.call_block("wafer-run/database", ...)`)
    /// is routed to the fake unchanged.
    pub fn with_fake_db(mut self, db: Arc<FakeDb>) -> Self {
        self.wafer
            .register_block("test/fake-db", db)
            .expect("register fake-db");
        self.wafer.add_alias("wafer-run/database", "test/fake-db");
        self
    }

    /// Register `FakeCrypto` at `test/fake-crypto` and alias `wafer-run/crypto`.
    pub fn with_fake_crypto(mut self, crypto: Arc<FakeCrypto>) -> Self {
        self.wafer
            .register_block("test/fake-crypto", crypto)
            .expect("register fake-crypto");
        self.wafer.add_alias("wafer-run/crypto", "test/fake-crypto");
        self
    }

    /// Register an arbitrary block at `name`.
    pub fn with_block(mut self, name: &str, block: Arc<dyn Block>) -> Self {
        self.wafer
            .register_block(name, block)
            .expect("register block");
        self
    }

    /// Provide config for a registered block.
    pub fn with_config(mut self, block: &str, config: serde_json::Value) -> Self {
        self.wafer.add_block_config(block, config);
        self
    }

    /// Set the WRAP admin block — this block bypasses resource access checks.
    /// Use in tests where the block under test needs unrestricted DB/crypto access
    /// (e.g., infrastructure blocks that were written before WRAP naming conventions).
    pub fn with_admin_block(mut self, block_id: &str) -> Self {
        self.wafer.set_admin_block(block_id);
        self
    }

    /// Start the runtime. Returns `Arc<Wafer>`.
    pub async fn build(self) -> Result<Arc<Wafer>, RuntimeError> {
        self.wafer.start().await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wafer_block::{streams::input::InputStream, Message};

    use super::*;

    #[tokio::test]
    async fn builder_routes_database_alias_to_fake() {
        let db = Arc::new(FakeDb::new());
        db.seed("x", vec![json!({"id": "1", "name": "hi"})]);

        let wafer = WaferBuilder::new()
            .with_fake_db(db.clone())
            .build()
            .await
            .unwrap();

        let mut msg = Message::new("database.list");
        msg.set_meta(wafer_block::meta::META_REQ_ACTION, "database.list");
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
