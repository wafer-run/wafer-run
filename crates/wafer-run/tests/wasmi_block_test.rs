//! Integration tests for WasmiBlock — loads the echo_block.wasm and exercises
//! the Block trait (info, handle, lifecycle, error handling, capabilities).

#[cfg(feature = "wasm")]
mod tests {
    use wafer_run::types::{
        Action, ErrorCode, LifecycleEvent, LifecycleType, Message, MetaEntry, Result_, WaferError,
    };
    use wafer_run::wasm::capabilities::BlockCapabilities;
    use wafer_run::wasm::WasmiBlock;
    use wafer_run::Block;

    const ECHO_WASM: &[u8] = include_bytes!("../testdata/echo_block.wasm");

    // -----------------------------------------------------------------------
    // Mock context — minimal implementation for tests that need one.
    // -----------------------------------------------------------------------

    struct MockContext;

    #[async_trait::async_trait]
    impl wafer_run::context::Context for MockContext {
        async fn call_block(&self, _name: &str, msg: &mut Message) -> Result_ {
            Result_ {
                action: Action::Error,
                response: None,
                error: Some(WaferError::new(
                    ErrorCode::Unimplemented,
                    "mock context: call_block not supported",
                )),
                message: Some(msg.clone()),
            }
        }

        fn is_cancelled(&self) -> bool {
            false
        }

        fn config_get(&self, _key: &str) -> Option<&str> {
            None
        }
    }

    // -----------------------------------------------------------------------
    // Test 1: load + info
    // -----------------------------------------------------------------------

    #[test]
    fn test_load_and_info() {
        let block = WasmiBlock::load_from_bytes(ECHO_WASM)
            .expect("echo_block.wasm should load without error");

        let info = block.info();
        assert_eq!(info.name, "example/echo", "block name mismatch");
        assert_eq!(info.version, "0.1.0", "block version mismatch");
        assert_eq!(info.interface, "handler@v1", "block interface mismatch");
    }

    // -----------------------------------------------------------------------
    // Test 2: handle — echo response
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_handle_echo() {
        let block = WasmiBlock::load_from_bytes(ECHO_WASM).expect("echo_block.wasm should load");

        let ctx = MockContext;
        let mut msg = Message {
            kind: "test.echo".to_string(),
            data: vec![],
            meta: vec![MetaEntry {
                key: "x-test".to_string(),
                value: "hello".to_string(),
            }],
        };

        let result = block.handle(&ctx, &mut msg).await;

        // The echo block always responds.
        assert_eq!(result.action, Action::Respond, "expected Respond action");
        assert!(
            result.error.is_none(),
            "unexpected error: {:?}",
            result.error
        );

        let response = result.response.expect("expected a response payload");
        let body: serde_json::Value =
            serde_json::from_slice(&response.data).expect("response data should be valid JSON");

        assert_eq!(body["echo"], true, "echo flag should be true");
        assert_eq!(body["kind"], "test.echo", "kind should be echoed");
        assert_eq!(body["meta_count"], 1, "meta_count should be 1");
    }

    // -----------------------------------------------------------------------
    // Test 3: lifecycle Init — should return Ok(())
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_lifecycle_init() {
        let block = WasmiBlock::load_from_bytes(ECHO_WASM).expect("echo_block.wasm should load");

        let ctx = MockContext;
        let event = LifecycleEvent {
            event_type: LifecycleType::Init,
            data: vec![],
        };

        let result: std::result::Result<(), WaferError> = block.lifecycle(&ctx, event).await;
        assert!(
            result.is_ok(),
            "lifecycle Init should return Ok(()), got: {:?}",
            result
        );
    }

    // -----------------------------------------------------------------------
    // Test 4: invalid WASM bytes are rejected
    // -----------------------------------------------------------------------

    #[test]
    fn test_invalid_wasm_rejected() {
        let garbage: &[u8] = b"this is not a valid wasm module at all!";
        let result = WasmiBlock::load_from_bytes(garbage);
        assert!(
            result.is_err(),
            "loading garbage bytes should return Err, but got Ok"
        );
    }

    // -----------------------------------------------------------------------
    // Test 5: capabilities are returned by block_capabilities()
    // -----------------------------------------------------------------------

    #[test]
    fn test_capabilities_returned() {
        // Load with a restricted capability set (no network, no storage).
        let caps = BlockCapabilities::none();
        let block = WasmiBlock::load_with_capabilities(ECHO_WASM, caps.clone())
            .expect("echo_block.wasm should load with restricted capabilities");

        let returned = block
            .block_capabilities()
            .expect("block_capabilities() should return Some for WasmiBlock");

        // The returned capabilities should match what we provided.
        assert!(!returned.network, "network capability should be false");
        assert!(!returned.raw_sql, "raw_sql capability should be false");
        assert!(!returned.crypto, "crypto capability should be false");
    }
}
