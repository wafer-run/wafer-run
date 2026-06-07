//! Verifies the Wafer → WasmiBlock asset-loader bridge.
//!
//! Both orderings must work:
//!   A. register block first, then call `set_asset_loader`
//!   B. call `set_asset_loader` first, then register block
//!
//! In both cases the WasmiBlock should observe the custom loader when its
//! `__wafer_host_load_asset` host import fires.

#[cfg(feature = "wasm")]
mod tests {
    use std::sync::{Arc, Mutex};

    use wafer_run::{
        asset_loader::{AssetLoadStatus, LoadAssetCallback},
        types::Message,
        wasm::WasmiBlock,
        Block, InputStream, OutputStream,
    };

    // -----------------------------------------------------------------------
    // Recording loader — remembers every asset-id it was asked to load.
    // -----------------------------------------------------------------------

    struct RecordingLoader {
        seen: Mutex<Vec<String>>,
        reply: AssetLoadStatus,
    }

    impl RecordingLoader {
        fn ready() -> Arc<Self> {
            Arc::new(Self {
                seen: Mutex::new(Vec::new()),
                reply: AssetLoadStatus::Ready,
            })
        }

        fn calls(&self) -> Vec<String> {
            self.seen.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl LoadAssetCallback for RecordingLoader {
        async fn load(&self, asset_id: &str) -> AssetLoadStatus {
            self.seen.lock().unwrap().push(asset_id.to_string());
            self.reply.clone()
        }
    }

    // -----------------------------------------------------------------------
    // Minimal mock context (same pattern as wasmi_block_test.rs).
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
            use wafer_run::types::{ErrorCode, WaferError};
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

        fn clone_arc(&self) -> std::sync::Arc<dyn wafer_run::context::Context> {
            std::sync::Arc::new(self.clone())
        }
    }

    // -----------------------------------------------------------------------
    // Shared WAT fixture — reuses tests/fixtures/load_asset_block.wat.
    // -----------------------------------------------------------------------

    const LOAD_ASSET_WAT: &str = include_str!("fixtures/load_asset_block.wat");

    fn compiled_wasm() -> Vec<u8> {
        wat::parse_str(LOAD_ASSET_WAT).expect("WAT fixture should parse")
    }

    // -----------------------------------------------------------------------
    // Helper: invoke the block's handle once and return the body JSON.
    // -----------------------------------------------------------------------

    async fn invoke_once(block: &WasmiBlock) -> serde_json::Value {
        let ctx = MockContext;
        let msg = Message::new("test.load_asset");
        let result = block.handle(&ctx, msg, InputStream::empty()).await;
        let response = result
            .collect_buffered()
            .await
            .expect("block should respond");
        serde_json::from_slice(&response.body).expect("response body should be valid JSON")
    }

    // -----------------------------------------------------------------------
    // Test A: register block first, then call Wafer::set_asset_loader.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn set_loader_after_register_propagates() {
        let mut wafer = wafer_run::Wafer::builder()
            .disable_inventory()
            .disable_lockfile()
            .build()
            .expect("empty wafer build is infallible");

        // Register block before the loader is set.
        let wasm_bytes = compiled_wasm();
        let block = WasmiBlock::load_from_bytes(&wasm_bytes).expect("WAT block should load");
        let block = Arc::new(block);
        wafer
            .register_block("test/load-asset", block.clone())
            .expect("register_block should succeed");

        // Now install the recording loader.
        let loader = RecordingLoader::ready();
        let callback: Arc<dyn LoadAssetCallback> = loader.clone();
        wafer.set_asset_loader(&callback);

        // Drive a handle call — the loader should be reached via the host import.
        let body = invoke_once(&block).await;

        assert_eq!(
            body.get("status").and_then(|v| v.as_i64()),
            Some(0),
            "Ready → status 0; body was {body}"
        );
        assert_eq!(
            loader.calls(),
            vec!["ffmpeg".to_string()],
            "recording loader should have been called once with 'ffmpeg'"
        );
    }

    // -----------------------------------------------------------------------
    // Test B: call Wafer::set_asset_loader first, then register block.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn set_loader_before_register_propagates() {
        let mut wafer = wafer_run::Wafer::builder()
            .disable_inventory()
            .disable_lockfile()
            .build()
            .expect("empty wafer build is infallible");

        // Install the recording loader before any block is registered.
        let loader = RecordingLoader::ready();
        let callback: Arc<dyn LoadAssetCallback> = loader.clone();
        wafer.set_asset_loader(&callback);

        // Now register the block.
        let wasm_bytes = compiled_wasm();
        let block = WasmiBlock::load_from_bytes(&wasm_bytes).expect("WAT block should load");
        let block = Arc::new(block);

        wafer
            .register_block("test/load-asset", block.clone())
            .expect("register_block should succeed");

        // Drive a handle call — the loader should be reached via the host import.
        let body = invoke_once(&block).await;

        assert_eq!(
            body.get("status").and_then(|v| v.as_i64()),
            Some(0),
            "Ready → status 0; body was {body}"
        );
        assert_eq!(
            loader.calls(),
            vec!["ffmpeg".to_string()],
            "recording loader should have been called once with 'ffmpeg'"
        );
    }

    // -----------------------------------------------------------------------
    // Test C: no loader set → NoopAssetLoader → status 2 (Failed).
    // Ensures the default behaviour is preserved and we didn't accidentally
    // break the noop path.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn no_loader_uses_noop() {
        // Bypass Wafer entirely — just a bare WasmiBlock with no loader injected.
        let wasm_bytes = compiled_wasm();
        let block = WasmiBlock::load_from_bytes(&wasm_bytes).expect("WAT block should load");
        let body = invoke_once(&block).await;

        assert_eq!(
            body.get("status").and_then(|v| v.as_i64()),
            Some(2),
            "NoopAssetLoader → Failed → status 2; body was {body}"
        );
    }
}
