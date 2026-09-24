//! A stream handle the host never issued is `InvalidArgument` at every
//! streaming-ABI host import that takes one, never `NotFound`.
//!
//! A guest's service clients drive these imports, and `NotFound` is a
//! service's "the thing you asked for does not exist": a guest's
//! `config::get_optional` reading a runtime `NotFound` would answer "unset".
//! A bad handle is a bad argument from the guest.

#[cfg(feature = "wasm")]
mod tests {
    use std::sync::Arc;

    use wafer_run::{
        wasm::WasmiBlock, Block, ErrorCode, InputStream, Message, OutputStream, WaferError,
    };

    const STREAM_HANDLE_WAT: &str = include_str!("fixtures/stream_handle_block.wat");

    /// The fixture never reaches `call_block` (the handle it finishes is
    /// unknown), so every method answers as a context that has nothing.
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
        // Denies every access, as the trait's default `check_resource_access` does.
        fn resource_access_admitted(
            &self,
            _resource: &str,
            _resource_type: wafer_block::types::ResourceType,
            _access: wafer_block::types::ResourceAccess,
        ) -> bool {
            false
        }
    }

    /// The fixture answers one byte per import (`write_chunk`, `finish`,
    /// `read_chunk`, `take_error`): `0x40` + the ordinal of the `ErrorCode`
    /// the import returned. `C` is `InvalidArgument` (3), `E` `NotFound` (5).
    #[tokio::test]
    async fn every_stream_import_answers_an_unknown_handle_with_invalid_argument() {
        assert_eq!(ErrorCode::InvalidArgument.to_ordinal(), 3);
        let wasm = wat::parse_str(STREAM_HANDLE_WAT).expect("WAT fixture parses");
        let block = WasmiBlock::load_from_bytes(&wasm).expect("fixture loads");

        let response = block
            .handle(&MockContext, Message::new("go"), InputStream::empty())
            .await
            .collect_buffered()
            .await
            .expect("the fixture responds");

        assert_eq!(
            String::from_utf8(response.body).expect("ASCII body"),
            "CCCC",
            "write_chunk, finish, read_chunk, take_error"
        );
    }
}
