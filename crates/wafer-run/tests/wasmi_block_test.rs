//! Integration tests for WasmiBlock — loads the echo_block.wasm and exercises
//! the Block trait (info, handle, lifecycle, error handling, capabilities).

#[cfg(feature = "wasm")]
mod tests {
    use wafer_block::streams::output::TerminalNotResponse;
    use wafer_run::{
        wasm::{capabilities::BlockCapabilities, WasmiBlock},
        Block, ErrorCode, FuelLimit, InputStream, LifecycleEvent, LifecycleType, Message,
        MetaEntry, OutputStream, ResourceLimits, WaferError,
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
    // Test 6b: configurable fuel budget — a heavy-but-bounded guest traps under
    // the default 100M cap, but runs to completion under FuelLimit::Unmetered
    // (and under a high Metered budget).
    //
    // The guest's `__wafer_handle` spins a counted loop sized to consume well
    // over 100M wasmi fuel units, then returns a valid `{"action":"Respond"}`
    // packed (ptr, len) pointing at a data segment. Under the metered default
    // the loop exhausts fuel and traps; with metering disabled (or a budget far
    // above the loop's cost) it finishes and Responds.
    // -----------------------------------------------------------------------

    /// Build a WAT module whose `__wafer_handle` burns ~`iters` loop iterations
    /// (≈5 fuel units each) before returning a `Respond` with empty data. The
    /// `Respond` JSON lives in a data segment at offset 16; the function returns
    /// the packed `(ptr << 32) | len`.
    fn heavy_loop_module(iters: u32) -> Vec<u8> {
        // The guest ABI result the host decodes into OutputStream::respond(&[]).
        const RESP_JSON: &str = r#"{"action":"Respond","response":{"data":[]}}"#;
        // Placed well past offset 0: the host's `__wafer_alloc` stub returns 0,
        // so the inbound serialized message is written at the start of memory.
        // Keeping the response data high avoids that clobbering it.
        const PTR: u32 = 4096;
        let len = RESP_JSON.len() as u32;
        let packed: i64 = ((PTR as i64) << 32) | (len as i64);
        // WAT string literals delimit on `"`, so escape the JSON's quotes.
        let resp_wat = RESP_JSON.replace('"', "\\\"");
        let wat = format!(
            r#"
            (module
              (memory (export "memory") 1)
              (data (i32.const {PTR}) "{resp_wat}")
              (func (export "__wafer_alloc") (param i32) (result i32) i32.const 0)
              (func (export "__wafer_info") (result i64) i64.const 0)
              (func (export "__wafer_handle") (param i32 i32) (result i64)
                (local $i i32)
                (local.set $i (i32.const {iters}))
                (block $done
                  (loop $spin
                    (br_if $done (i32.eqz (local.get $i)))
                    (local.set $i (i32.sub (local.get $i) (i32.const 1)))
                    (br $spin)))
                (i64.const {packed})
              )
              (func (export "__wafer_lifecycle") (param i32 i32) (result i64) i64.const 0)
            )
            "#,
        );
        wat::parse_str(&wat).expect("heavy-loop WAT should parse")
    }

    #[tokio::test]
    async fn test_fuel_budget_metered_vs_unmetered() {
        // 50M iterations × ~5 fuel/iter ≈ 250M fuel — comfortably over the 100M
        // default cap, but trivial wall-clock work when unmetered.
        let wasm_bytes = heavy_loop_module(50_000_000);
        let ctx = MockContext;

        // 1. Default (metered 100M): the heavy loop exhausts fuel → trap.
        let metered = WasmiBlock::load_from_bytes(&wasm_bytes)
            .expect("heavy-loop module should load (default fuel)");
        let metered_out = metered
            .handle(
                &ctx,
                Message::new("test.fuel.metered"),
                InputStream::empty(),
            )
            .await;
        match metered_out.collect_buffered().await {
            Err(TerminalNotResponse::Error(err)) => {
                let err_msg = format!("{err:?}");
                assert!(
                    err_msg.contains("fuel"),
                    "default-fuel run should trap on fuel exhaustion, got: {err_msg}"
                );
            }
            other => panic!("default-fuel heavy loop should trap, got: {other:?}"),
        }

        // 2. Unmetered: the loop runs to completion and Responds (no fuel trap).
        let unmetered = WasmiBlock::load_from_bytes_with_fuel(&wasm_bytes, FuelLimit::Unmetered)
            .expect("heavy-loop module should load (unmetered)");
        let unmetered_out = unmetered
            .handle(
                &ctx,
                Message::new("test.fuel.unmetered"),
                InputStream::empty(),
            )
            .await;
        let resp = unmetered_out
            .collect_buffered()
            .await
            .expect("unmetered heavy loop should run to completion and Respond");
        assert!(
            resp.body.is_empty(),
            "unmetered Respond should carry the empty data payload, got: {:?}",
            resp.body
        );

        // 3. A high Metered budget (1B) is also enough for the loop to finish.
        let high =
            WasmiBlock::load_from_bytes_with_fuel(&wasm_bytes, FuelLimit::Metered(1_000_000_000))
                .expect("heavy-loop module should load (high metered)");
        let high_out = high
            .handle(&ctx, Message::new("test.fuel.high"), InputStream::empty())
            .await;
        high_out
            .collect_buffered()
            .await
            .expect("high-budget heavy loop should run to completion and Respond");
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

    // -----------------------------------------------------------------------
    // Test 8b: configurable memory cap — an allocation-heavy guest OOMs under
    // the default 256-page (16 MiB) cap but runs to completion under a higher
    // cap (1024 pages / 64 MiB).
    //
    // The guest's `__wafer_handle` grows linear memory by 299 pages (total 300,
    // above the 256 default but below 1024), writes a sentinel into the freshly
    // grown high page to force a trap if growth was denied, then returns a valid
    // `{"action":"Respond"}` packed (ptr, len). Under the default cap the grow
    // is denied → the store traps; under the raised cap it succeeds and Responds.
    // -----------------------------------------------------------------------

    /// Build a WAT module whose `__wafer_handle` grows memory by `grow_pages`
    /// (starting from 1 page), writes a byte into the last grown page, then
    /// returns a `Respond` with empty data. If the host denies the grow,
    /// `memory.grow` returns -1 and the subsequent store traps out-of-bounds.
    fn memory_grow_module(grow_pages: u32) -> Vec<u8> {
        const RESP_JSON: &str = r#"{"action":"Respond","response":{"data":[]}}"#;
        // The response data lives in the first (always-present) page so growth
        // success/failure never affects where the host reads the result from.
        const PTR: u32 = 256;
        let len = RESP_JSON.len() as u32;
        let packed: i64 = ((PTR as i64) << 32) | (len as i64);
        // Byte offset inside the last newly-grown page: (1 + grow_pages - 1)
        // pages in, i.e. `grow_pages * 65536`. If the grow was denied this
        // offset is out of bounds and the i32.store traps.
        let probe_offset: u32 = grow_pages * 65536;
        let resp_wat = RESP_JSON.replace('"', "\\\"");
        let wat = format!(
            r#"
            (module
              (memory (export "memory") 1)
              (data (i32.const {PTR}) "{resp_wat}")
              (func (export "__wafer_alloc") (param i32) (result i32) i32.const 0)
              (func (export "__wafer_info") (result i64) i64.const 0)
              (func (export "__wafer_handle") (param i32 i32) (result i64)
                ;; Grow by `grow_pages`. Returns -1 on denial, prev pages on ok.
                ;; Drop the result; the probe store below traps if growth failed.
                (drop (memory.grow (i32.const {grow_pages})))
                ;; Write a sentinel into the last grown page — traps OOB if denied.
                (i32.store (i32.const {probe_offset}) (i32.const 42))
                (i64.const {packed})
              )
              (func (export "__wafer_lifecycle") (param i32 i32) (result i64) i64.const 0)
            )
            "#,
        );
        wat::parse_str(&wat).expect("memory-grow WAT should parse")
    }

    #[tokio::test]
    async fn test_memory_cap_default_vs_raised() {
        // Grow by 299 pages → 300 total, above the 256 default but below 1024.
        let wasm_bytes = memory_grow_module(299);
        let ctx = MockContext;

        // 1. Default (256-page cap): the grow to 300 pages is denied → trap.
        let bounded = WasmiBlock::load_from_bytes(&wasm_bytes)
            .expect("memory-grow module should load (default cap)");
        let bounded_out = bounded
            .handle(&ctx, Message::new("test.mem.default"), InputStream::empty())
            .await;
        match bounded_out.collect_buffered().await {
            Err(TerminalNotResponse::Error(err)) => {
                let err_msg = format!("{err:?}");
                assert!(
                    err_msg.contains("WASM handle error")
                        || err_msg.contains("out of bounds")
                        || err_msg.contains("memory"),
                    "default-cap run should trap on the denied grow, got: {err_msg}"
                );
            }
            other => panic!("default-cap memory grow should trap, got: {other:?}"),
        }

        // 2. Raised cap (1024 pages / 64 MiB): the grow succeeds and Responds.
        let raised = WasmiBlock::load_from_bytes_with_limits(
            &wasm_bytes,
            ResourceLimits {
                memory_pages: 1024,
                ..ResourceLimits::default()
            },
        )
        .expect("memory-grow module should load (raised cap)");
        let raised_out = raised
            .handle(&ctx, Message::new("test.mem.raised"), InputStream::empty())
            .await;
        let resp = raised_out
            .collect_buffered()
            .await
            .expect("raised-cap memory grow should run to completion and Respond");
        assert!(
            resp.body.is_empty(),
            "raised-cap Respond should carry the empty data payload, got: {:?}",
            resp.body
        );
    }
}
