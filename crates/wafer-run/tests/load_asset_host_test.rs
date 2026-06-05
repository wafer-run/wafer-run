//! Integration tests for the `__wafer_host_load_asset` wasmi host import.
//!
//! Drives a minimal WAT block that calls `__wafer_host_load_asset("ffmpeg")`,
//! then emits a JSON response containing the returned i32 status code. We
//! assert that the scripted `LoadAssetCallback` is invoked and its status
//! code (0=Ready, 1=Pending, 2=Failed) reaches the guest.

#[cfg(feature = "wasm")]
mod tests {
    use std::sync::Arc;

    use wafer_run::{
        asset_loader::{AssetLoadError, AssetLoadStatus, LoadAssetCallback},
        types::{ErrorCode, Message, WaferError},
        wasm::WasmiBlock,
        Block, InputStream, OutputStream,
    };

    const LOAD_ASSET_WAT: &str = include_str!("fixtures/load_asset_block.wat");

    // -----------------------------------------------------------------------
    // Mock context (reused pattern from wasmi_block_test.rs).
    // -----------------------------------------------------------------------

    #[derive(Clone)]
    struct MockContext;

    #[async_trait::async_trait]
    impl wafer_run::context::Context for MockContext {
        async fn call_block(
            &self,
            _name: &str,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            OutputStream::error(WaferError::new(
                ErrorCode::Unimplemented,
                "mock context: call_block not supported",
            ))
        }

        fn is_cancelled(&self) -> bool {
            false
        }

        fn config_get(&self, _key: &str) -> Option<&str> {
            None
        }

        fn clone_arc(&self) -> Arc<dyn wafer_run::context::Context> {
            Arc::new(self.clone())
        }
    }

    // -----------------------------------------------------------------------
    // Scripted loader that returns a predetermined status.
    // -----------------------------------------------------------------------

    struct ScriptedLoader {
        reply: AssetLoadStatus,
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl ScriptedLoader {
        fn new(reply: AssetLoadStatus) -> Self {
            Self {
                reply,
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl LoadAssetCallback for ScriptedLoader {
        async fn load(&self, asset_id: &str) -> AssetLoadStatus {
            self.calls.lock().unwrap().push(asset_id.to_string());
            self.reply.clone()
        }
    }

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    async fn drive_block(loader: Arc<ScriptedLoader>) -> serde_json::Value {
        let wasm_bytes = wat::parse_str(LOAD_ASSET_WAT).expect("WAT fixture should parse");
        let block =
            WasmiBlock::load_from_bytes(&wasm_bytes).expect("load_asset WAT block should load");
        block.set_asset_loader(loader.clone());

        let ctx = MockContext;
        let msg = Message::new("test.load_asset");
        let result = block.handle(&ctx, msg, InputStream::empty()).await;

        let response = result
            .collect_buffered()
            .await
            .expect("block should Respond");
        serde_json::from_slice(&response.body).expect("response body should be valid JSON")
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn ready_status_returns_zero() {
        let loader = Arc::new(ScriptedLoader::new(AssetLoadStatus::Ready));
        let body = drive_block(loader.clone()).await;

        assert_eq!(
            body.get("status").and_then(|v| v.as_i64()),
            Some(0),
            "Ready should map to i32 status 0; body was {body}"
        );

        assert_eq!(*loader.calls.lock().unwrap(), vec!["ffmpeg".to_string()]);
    }

    #[tokio::test]
    async fn failed_status_returns_two() {
        let loader = Arc::new(ScriptedLoader::new(AssetLoadStatus::Failed(
            AssetLoadError::Network("offline".into()),
        )));
        let body = drive_block(loader.clone()).await;

        assert_eq!(
            body.get("status").and_then(|v| v.as_i64()),
            Some(2),
            "Failed should map to i32 status 2; body was {body}"
        );

        assert_eq!(*loader.calls.lock().unwrap(), vec!["ffmpeg".to_string()]);
    }
}
