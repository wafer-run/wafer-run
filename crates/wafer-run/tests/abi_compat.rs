//! Sentinel: the legacy `__wafer_host_call_block` import is gone.
//!
//! If a downstream skill imports it (e.g. a half-migrated consumer), the
//! wasmi loader should reject the module at instantiation. This test pins
//! that behaviour so a future, well-meaning patch cannot silently re-add
//! the legacy import without breaking the build.
//!
//! Note: `Module::new` (called by `WasmiBlock::load_from_bytes`) only parses
//! and validates the wasm format — unknown imports do not surface here.
//! Instantiation is what triggers the linker to resolve imports against the
//! host, so the assertion runs through `Block::handle` (or `info`) which
//! drives instantiation internally.

#[cfg(feature = "wasm")]
mod tests {
    use wafer_run::{
        wasm::WasmiBlock, Block, ErrorCode, InputStream, Message, OutputStream, WaferError,
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

        fn clone_arc(&self) -> std::sync::Arc<dyn wafer_run::context::Context> {
            std::sync::Arc::new(self.clone())
        }
    }

    #[tokio::test]
    async fn legacy_call_block_import_is_rejected() {
        // Minimal wasm that imports the legacy host symbol. The export gives
        // the loader something to discover; instantiation is what fails.
        let wat = r#"
            (module
              (import "wafer" "__wafer_host_call_block"
                (func (param i32 i32 i32 i32 i32 i32) (result i64)))
              (memory (export "memory") 1)
              (func (export "__wafer_handle") (param i32 i32) (result i64) i64.const 0)
            )
        "#;
        let wasm = wat::parse_str(wat).expect("compile wat");

        // load_from_bytes only validates the wasm format — succeeds.
        let block = WasmiBlock::load_from_bytes(&wasm)
            .expect("module parse should succeed; only instantiation should fail");

        // handle() drives the linker.instantiate path. With the legacy import
        // unregistered, the linker has no resolution and instantiation fails.
        let ctx = MockContext;
        let result = block
            .handle(&ctx, Message::new("test.kind"), InputStream::empty())
            .await
            .collect_buffered()
            .await;

        let Err(err) = result else {
            panic!(
                "instantiating a module that imports the legacy __wafer_host_call_block should fail"
            )
        };
        let msg = format!("{err:?}").to_lowercase();
        assert!(
            msg.contains("__wafer_host_call_block")
                || msg.contains("unknown")
                || msg.contains("import"),
            "error should mention the missing legacy import; got: {msg}"
        );
    }
}
