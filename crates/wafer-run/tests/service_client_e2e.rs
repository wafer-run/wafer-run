//! End-to-end (TODO #103): a WASM guest calls a **wafer-core** typed service
//! client and gets a real response back through the host.
//!
//! The guest (`tests/service_client_guest/`) is the first fixture compiled
//! against `wafer-core --features wasm-component`; its `__wafer_handle` calls
//! `wafer_core::clients::config::get(key)`, which drives wafer-core's sync
//! `call_service` over the streaming-ABI host imports. The `FakeConfigBlock`
//! below stands in for `wafer-run/config`, decoding the `config.get` request
//! and returning a codec-encoded `GetResponse`.
//!
//! This proves the chain wafer-core PR1 implemented actually runs end to end:
//!
//!   guest → wafer_core::clients::config::get → call_service → host streaming
//!   ABI → FakeConfigBlock → codec wire → back through the ABI → guest response
//!
//! The config key the guest reads (`test__service_client_guest__greeting`) is
//! **owned by the guest** under the WRAP namespace convention
//! (`resource_owner(...) == "test/service-client-guest"`), so the access passes
//! WRAP's own-resource rule without any admin/grant setup — keeping the test
//! about `call_service`, not about WRAP.

#![cfg(feature = "wasm")]

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use wafer_block::{
    codec,
    common::{ErrorCode, ServiceOp},
    streams::{input::InputStream, output::OutputStream},
    types::BlockInfo,
    wire::config::{GetRequest, GetResponse},
    Block, Context, Message, WaferError,
};
use wafer_run::{wasm::WasmiBlock, Wafer};

/// Config key the guest reads. Namespaced so the guest owns it under WRAP
/// (`{org}__{block}__{name}` → `test/service-client-guest`).
const CONFIG_KEY: &str = "test__service_client_guest__greeting";
const CONFIG_VALUE: &str = "hello-from-config-block";

/// Path to the prebuilt service-client guest wasm. Its crate has its own
/// `[workspace]` and is excluded from the parent workspace, so build it via:
///
/// ```bash
/// cargo build --target wasm32-wasip1 --release \
///     --manifest-path crates/wafer-run/tests/service_client_guest/Cargo.toml
/// ```
fn service_client_guest_wasm() -> Vec<u8> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/service_client_guest/target/wasm32-wasip1/release/service_client_guest.wasm");
    std::fs::read(&p).unwrap_or_else(|e| {
        panic!(
            "failed to read service-client guest wasm at {}: {e}\n\
             Did you build it first?\n  cargo build --target wasm32-wasip1 --release \\\n    \
             --manifest-path crates/wafer-run/tests/service_client_guest/Cargo.toml",
            p.display()
        )
    })
}

/// Test stand-in for `wafer-run/config`: answers `config.get` for the one key
/// the guest asks for with a known value; everything else is an error.
struct FakeConfigBlock;

#[async_trait]
impl Block for FakeConfigBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/config",
            "0.1.0",
            "config@v1",
            "Test stub — answers config.get with a fixed value",
        )
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        if msg.kind != ServiceOp::CONFIG_GET {
            return OutputStream::error(WaferError::new(
                ErrorCode::Unimplemented,
                format!("fake config: unexpected kind {}", msg.kind),
            ));
        }
        let body = input.collect_to_bytes().await;
        let req: GetRequest = match codec::decode(&body) {
            Ok(r) => r,
            Err(e) => {
                return OutputStream::error(WaferError::new(
                    ErrorCode::InvalidArgument,
                    format!("fake config: undecodable GetRequest: {}", e.message),
                ))
            }
        };
        if req.key != CONFIG_KEY {
            return OutputStream::error(WaferError::new(
                ErrorCode::NotFound,
                format!("fake config: unexpected key {}", req.key),
            ));
        }
        let resp = GetResponse {
            value: CONFIG_VALUE.to_string(),
        };
        match codec::encode(&resp) {
            Ok(bytes) => OutputStream::respond(bytes),
            Err(e) => OutputStream::error(e),
        }
    }
}

#[tokio::test]
async fn wasm_guest_calls_wafer_core_config_client() {
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("Wafer::build");

    wafer
        .register_block("wafer-run/config", Arc::new(FakeConfigBlock))
        .expect("register fake config");

    let wasm = service_client_guest_wasm();
    let block = WasmiBlock::load_from_bytes(&wasm).expect("load service-client guest wasm");
    wafer
        .register_block("test/service-client-guest", Arc::new(block))
        .expect("register service-client-guest");

    let wafer = wafer.start().await.expect("start runtime");

    // Drive the guest: it calls `wafer_core::clients::config::get(CONFIG_KEY)`
    // and returns the resolved value as the response body.
    let output = wafer
        .run_block(
            "test/service-client-guest",
            Message::new("test.config_get"),
            InputStream::from_bytes(CONFIG_KEY.as_bytes().to_vec()),
        )
        .await;

    let buf = output
        .collect_buffered()
        .await
        .expect("guest config_get should complete with a buffered response");

    assert_eq!(
        String::from_utf8_lossy(&buf.body),
        CONFIG_VALUE,
        "guest should return the value FakeConfigBlock resolved for the config key — \
         proves wafer-core's wasm-component call_service drove the full request/response cycle"
    );
}
