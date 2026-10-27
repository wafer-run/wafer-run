//! The guest bounds the runtime states on its wasmi engine rather than
//! inheriting from wasmi's defaults (`runtime::wasm_state::wasmi_config` and
//! the `ResourceLimiter` on `WasmiHostState`).
//!
//! wasmi has moved several of these between releases — the call-depth limit
//! (1024 → 1000 in 1.0), the value-stack limit, and, since 0.45, what a
//! resumable call does when it runs out of fuel (it suspends instead of
//! trapping). Each test here pins one bound end to end through
//! `WasmiBlock::handle`, so an interpreter upgrade that moves a default
//! fails here instead of silently changing what a guest may do.

#[cfg(feature = "wasm")]
mod tests {
    use std::sync::Arc;

    use wafer_block::streams::output::TerminalNotResponse;
    use wafer_run::{
        wasm::WasmiBlock, Block, ErrorCode, FuelLimit, InputStream, Message, OutputStream,
        WaferError,
    };

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

        fn resource_access_admitted(
            &self,
            _resource: &str,
            _resource_type: wafer_block::types::ResourceType,
            _access: wafer_block::types::ResourceAccess,
        ) -> bool {
            false
        }
    }

    /// The `Respond` result `__wafer_handle` returns on success, placed in a
    /// data segment well past offset 0 (where the host writes the inbound
    /// message through the `__wafer_alloc` stub).
    const RESP_JSON: &str = r#"{"action":"Respond","response":{"data":[]}}"#;
    const RESP_PTR: u32 = 4096;

    fn respond_packed() -> i64 {
        ((RESP_PTR as i64) << 32) | RESP_JSON.len() as i64
    }

    fn resp_data_segment() -> String {
        format!(
            r#"(data (i32.const {RESP_PTR}) "{}")"#,
            RESP_JSON.replace('"', "\\\"")
        )
    }

    async fn run(wasm: &[u8]) -> Result<(), String> {
        run_block(WasmiBlock::load_from_bytes(wasm).expect("module should load")).await
    }

    async fn run_block(block: WasmiBlock) -> Result<(), String> {
        match block
            .handle(&MockContext, Message::new("bounds"), InputStream::empty())
            .await
            .collect_buffered()
            .await
        {
            Ok(_) => Ok(()),
            Err(TerminalNotResponse::Error(err)) => Err(format!("{err:?}")),
            Err(other) => panic!("unexpected terminal: {other:?}"),
        }
    }

    /// A guest whose `__wafer_handle` recurses `depth` frames deep through a
    /// non-tail call, then Responds.
    fn recursion_module(depth: u32) -> Vec<u8> {
        let wat = format!(
            r#"
            (module
              (memory (export "memory") 1)
              {data}
              (func $rec (param $n i32) (result i32)
                (if (result i32) (i32.eqz (local.get $n))
                  (then (i32.const 0))
                  (else
                    (i32.add
                      (call $rec (i32.sub (local.get $n) (i32.const 1)))
                      (i32.const 1)))))
              (func (export "__wafer_alloc") (param i32) (result i32) i32.const 0)
              (func (export "__wafer_info") (result i64) i64.const 0)
              (func (export "__wafer_handle") (param i32 i32) (result i64)
                (drop (call $rec (i32.const {depth})))
                (i64.const {packed}))
              (func (export "__wafer_lifecycle") (param i32 i32) (result i64) i64.const 0)
            )
            "#,
            data = resp_data_segment(),
            packed = respond_packed(),
        );
        wat::parse_str(&wat).expect("recursion WAT should parse")
    }

    /// The runtime's call-depth bound is 1024 frames. 1020 nested `$rec`
    /// frames plus `__wafer_handle` fit under it — and do not fit under
    /// wasmi 2.0's own default of 1000, so this fails if the pinned limit is
    /// dropped and the interpreter's default takes over.
    #[tokio::test]
    async fn recursion_within_the_pinned_depth_completes() {
        run(&recursion_module(1020))
            .await
            .expect("1021 frames are within the 1024-frame call-depth bound");
    }

    /// Past the bound the call traps with wasmi's stack-overflow trap — an
    /// error terminal, not a host stack overflow.
    #[tokio::test]
    async fn recursion_past_the_pinned_depth_traps() {
        let err = run(&recursion_module(1100))
            .await
            .expect_err("1100 frames exceed the 1024-frame call-depth bound");
        assert!(
            err.contains("call stack exhausted"),
            "expected a stack-overflow trap, got: {err}"
        );
    }

    /// Unbounded recursion with large frames traps on the value-stack bound
    /// (or the depth bound, whichever it reaches first) instead of growing
    /// host memory without limit.
    #[tokio::test]
    async fn unbounded_recursion_traps() {
        // 128 i64 locals ≈ 1 KiB of value stack per frame.
        let locals = "(local i64)".repeat(128);
        let wat = format!(
            r#"
            (module
              (memory (export "memory") 1)
              (func $rec (param $n i32) (result i32)
                {locals}
                (i32.add (call $rec (local.get $n)) (i32.const 1)))
              (func (export "__wafer_alloc") (param i32) (result i32) i32.const 0)
              (func (export "__wafer_info") (result i64) i64.const 0)
              (func (export "__wafer_handle") (param i32 i32) (result i64)
                (drop (call $rec (i32.const 0)))
                (i64.const 0))
              (func (export "__wafer_lifecycle") (param i32 i32) (result i64) i64.const 0)
            )
            "#,
        );
        let wasm = wat::parse_str(&wat).expect("unbounded-recursion WAT should parse");
        let err = run(&wasm).await.expect_err("unbounded recursion must trap");
        assert!(
            err.contains("call stack exhausted"),
            "expected a stack-overflow trap, got: {err}"
        );
    }

    /// Fuel exhaustion *after* a host trap has been resumed ends the call
    /// with the out-of-fuel trap. Since wasmi 0.45 a resumable call that runs
    /// out of fuel suspends instead of trapping; the resume loop must treat
    /// that as terminal on every resume, not only on the initial call (which
    /// `wasmi_block_test::test_fuel_exhaustion` covers).
    #[tokio::test]
    async fn fuel_exhaustion_after_a_resumed_host_call_ends_the_call() {
        let wat = r#"
            (module
              (import "wafer" "__wafer_host_load_asset"
                (func $load_asset (param i32 i32) (result i32)))
              (memory (export "memory") 1)
              (data (i32.const 16) "asset")
              (func (export "__wafer_alloc") (param i32) (result i32) i32.const 1024)
              (func (export "__wafer_info") (result i64) i64.const 0)
              (func (export "__wafer_handle") (param i32 i32) (result i64)
                ;; Traps out to the host, which resumes the guest...
                (drop (call $load_asset (i32.const 16) (i32.const 5)))
                ;; ...which then spins until the budget is gone.
                (loop $inf (br $inf))
                (unreachable))
              (func (export "__wafer_lifecycle") (param i32 i32) (result i64) i64.const 0)
            )
        "#;
        let wasm = wat::parse_str(wat).expect("WAT should parse");
        // A small budget keeps the spin short; the default would do too.
        let block = WasmiBlock::load_from_bytes_with_fuel(&wasm, FuelLimit::Metered(1_000_000))
            .expect("module should load");
        let err = run_block(block)
            .await
            .expect_err("the post-resume loop must exhaust fuel");
        assert!(
            err.contains("resuming guest after load_asset")
                && err.contains("all fuel consumed by WebAssembly"),
            "expected the out-of-fuel trap on the resume path, got: {err}"
        );
    }

    /// A module whose declared minimum memory is already past the page cap
    /// fails to instantiate: the limiter is consulted for the initial
    /// allocation, not only for `memory.grow`.
    #[tokio::test]
    async fn initial_memory_past_the_cap_fails_instantiation() {
        // Default cap: 256 pages (16 MiB).
        let wat = r#"
            (module
              (memory (export "memory") 257)
              (func (export "__wafer_alloc") (param i32) (result i32) i32.const 0)
              (func (export "__wafer_info") (result i64) i64.const 0)
              (func (export "__wafer_handle") (param i32 i32) (result i64) i64.const 0)
              (func (export "__wafer_lifecycle") (param i32 i32) (result i64) i64.const 0)
            )
        "#;
        let wasm = wat::parse_str(wat).expect("WAT should parse");
        let err = run(&wasm)
            .await
            .expect_err("a 257-page minimum exceeds the 256-page cap");
        assert!(
            err.contains("instantiation"),
            "expected an instantiation failure, got: {err}"
        );
    }
}
