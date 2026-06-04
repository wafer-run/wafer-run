//! Integration tests for WasmiBlock — loads the echo_block.wasm and exercises
//! the Block trait (info, handle, lifecycle, error handling, capabilities).

#[cfg(feature = "wasm")]
mod tests {
    use wafer_block::streams::output::TerminalNotResponse;
    use wafer_run::{
        types::{ErrorCode, LifecycleEvent, LifecycleType, Message, MetaEntry, WaferError},
        wasm::{capabilities::BlockCapabilities, WasmiBlock},
        Block, InputStream, OutputStream,
    };

    const ECHO_WASM: &[u8] = include_bytes!("../testdata/echo_block.wasm");

    // -----------------------------------------------------------------------
    // Mock context — minimal implementation for tests that need one.
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

        fn clone_arc(&self) -> std::sync::Arc<dyn wafer_run::context::Context> {
            std::sync::Arc::new(self.clone())
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
        let mut msg = Message::new("test.echo");
        msg.meta.push(MetaEntry {
            key: "x-test".to_string(),
            value: "hello".to_string(),
        });

        let result = block.handle(&ctx, msg, InputStream::empty()).await;

        // The echo block always responds.
        let buf = result.collect_buffered().await;
        assert!(
            buf.is_ok(),
            "expected Respond (Ok), got error: {:?}",
            buf.err()
        );

        let response = buf.unwrap();
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("response body should be valid JSON");

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
            "lifecycle Init should return Ok(()), got: {result:?}"
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
        let block = WasmiBlock::load_with_capabilities(ECHO_WASM, caps)
            .expect("echo_block.wasm should load with restricted capabilities");

        let returned = block
            .block_capabilities()
            .expect("block_capabilities() should return Some for WasmiBlock");

        // The returned capabilities should match what we provided.
        assert!(!returned.network, "network capability should be false");
        assert!(!returned.raw_sql, "raw_sql capability should be false");
        assert!(!returned.crypto, "crypto capability should be false");
    }

    // -----------------------------------------------------------------------
    // Test 6: fuel exhaustion — infinite loop terminates with error
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_fuel_exhaustion() {
        let wasm_bytes = wat::parse_str(
            r#"
            (module
              (memory (export "memory") 1)
              (func (export "__wafer_alloc") (param i32) (result i32) i32.const 0)
              (func (export "__wafer_info") (result i64) i64.const 0)
              (func (export "__wafer_handle") (param i32 i32) (result i64)
                (loop $inf (br $inf))
                (unreachable)
              )
              (func (export "__wafer_lifecycle") (param i32 i32) (result i64) i64.const 0)
            )
            "#,
        )
        .expect("WAT should parse");

        let block =
            WasmiBlock::load_from_bytes(&wasm_bytes).expect("fuel-exhaustion module should load");

        let ctx = MockContext;
        let msg = Message::new("test.fuel");

        let result = block.handle(&ctx, msg, InputStream::empty()).await;

        match result.collect_buffered().await {
            Err(TerminalNotResponse::Error(err)) => {
                let err_msg = format!("{err:?}");
                assert!(
                    err_msg.contains("fuel"),
                    "error should mention fuel exhaustion, got: {err_msg}"
                );
            }
            other => panic!("infinite loop should produce an error, got: {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Tests 7 / 7b removed: they exercised the legacy `__wafer_host_call_block`
    // host import directly via WAT. That import no longer exists — its six
    // streaming replacements (`__wafer_host_stream_*`) are the new ABI surface.
    //
    // - Capability denial is covered by `wasm::stream` unit tests + the
    //   negative-i64 ErrorCode sentinel returned from `__wafer_host_stream_init`
    //   on `allows_call_block` failure.
    // - Body round-trip will be covered end-to-end in Task 16 (E2E dispatch
    //   tests) once the SDK wrappers (Task 9) and typed clients (Tasks 10/11)
    //   land — those tests use real Rust/TinyGo guests rather than hand-rolled
    //   WAT, so they don't need to redeclare each host import by hand.
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Test 7c: a guest that returns a negative i64 from `__wafer_handle`
    // (an error sentinel where a packed (ptr, len) is expected) yields a clean
    // error — not garbage fed to read_guest_bytes.
    //
    // This also exercises an error-return path through
    // `call_guest_resumable_with_attachments`: the host must clear the borrowed
    // Context from the store (RAII `ContextScope`) on this path or the
    // `ContextGuard` strong-count assertion would panic and crash the host. If
    // the guard regressed, this test would abort rather than fail cleanly.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_negative_packed_return_is_rejected() {
        let wasm_bytes = wat::parse_str(
            r#"
            (module
              (memory (export "memory") 1)
              (func (export "__wafer_alloc") (param i32) (result i32) i32.const 0)
              (func (export "__wafer_info") (result i64) i64.const 0)
              ;; Return -1: a negative error sentinel, not a packed (ptr, len).
              (func (export "__wafer_handle") (param i32 i32) (result i64) i64.const -1)
              (func (export "__wafer_lifecycle") (param i32 i32) (result i64) i64.const 0)
            )
            "#,
        )
        .expect("WAT should parse");

        let block =
            WasmiBlock::load_from_bytes(&wasm_bytes).expect("negative-return module should load");

        let ctx = MockContext;
        let msg = Message::new("test.neg");

        let result = block.handle(&ctx, msg, InputStream::empty()).await;

        match result.collect_buffered().await {
            Err(TerminalNotResponse::Error(err)) => {
                let err_msg = format!("{err:?}");
                assert!(
                    err_msg.contains("negative"),
                    "error should mention the negative packed value, got: {err_msg}"
                );
            }
            other => panic!("negative packed return should produce an error, got: {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Test 8: memory limit — growth beyond 256 pages is denied
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_memory_limit() {
        let wasm_bytes = wat::parse_str(
            r#"
            (module
              (memory (export "memory") 1)
              (func (export "__wafer_alloc") (param i32) (result i32) i32.const 0)
              (func (export "__wafer_info") (result i64) i64.const 0)
              (func (export "__wafer_handle") (param i32 i32) (result i64)
                ;; Try to grow by 300 pages (total would be 301 > 256 limit).
                ;; memory.grow returns -1 on failure, previous page count on success.
                (memory.grow (i32.const 300))
                ;; Multiply by 65536 to compute a byte offset in the "new" memory.
                ;; If growth was denied (-1), this becomes 0xFFFF0000 — out of bounds.
                ;; If growth succeeded, this is a valid offset.
                (i32.const 65536)
                (i32.mul)
                ;; Store to that offset — traps on out-of-bounds if growth was denied.
                (i32.const 42)
                (i32.store)
                (i64.const 0)
              )
              (func (export "__wafer_lifecycle") (param i32 i32) (result i64) i64.const 0)
            )
            "#,
        )
        .expect("WAT should parse");

        let block =
            WasmiBlock::load_from_bytes(&wasm_bytes).expect("memory-limit module should load");

        let ctx = MockContext;
        let msg = Message::new("test.mem");

        let result = block.handle(&ctx, msg, InputStream::empty()).await;

        match result.collect_buffered().await {
            Err(TerminalNotResponse::Error(err)) => {
                let err_msg = format!("{err:?}");
                // The error should indicate an out-of-bounds memory access
                assert!(
                    err_msg.contains("WASM handle error")
                        || err_msg.contains("out of bounds")
                        || err_msg.contains("memory"),
                    "error should indicate memory-related failure, got: {err_msg}"
                );
            }
            other => panic!("exceeding memory limit should produce an error, got: {other:?}"),
        }
    }
}
