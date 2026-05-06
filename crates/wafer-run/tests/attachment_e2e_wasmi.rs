//! End-to-end wasmi test for the attachment ABI.
//!
//! A wasmi guest opens a streaming call to itself, attaches a fixed binary
//! payload via [`wafer_sdk::stream::CallStream::attach`] (→ host
//! `__wafer_host_stream_attach`), finishes the call, and returns the bytes
//! the callee echoed back.
//!
//! The runtime carries the attachment to the callee via
//! `Context::call_block_with_attachments`; the callee retrieves it via
//! [`wafer_sdk::lookup_attachment`] (→ host `__wafer_host_lookup_attachment`)
//! and responds with its bytes. If the round-tripped bytes match the original
//! payload, the full producer + carry-forward + lookup chain is correct.
//!
//! The guest fixture lives at `tests/attachment_dispatch/`; it is its own
//! `[workspace]` and must be built ahead of running this test. See
//! [`attachment_dispatch_guest_wasm`] for the exact command.

#![cfg(feature = "wasm")]

use std::{path::PathBuf, sync::Arc};

use wafer_block::{streams::input::InputStream, Message};
use wafer_run::{wasm::WasmiBlock, Wafer};

/// Path to the prebuilt guest wasm. Build it with:
///
/// ```bash
/// cargo build --target wasm32-wasip1 --release \
///     --manifest-path crates/wafer-run/tests/attachment_dispatch/Cargo.toml
/// ```
fn attachment_dispatch_guest_wasm() -> Vec<u8> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/attachment_dispatch/target/wasm32-wasip1/release/attachment_dispatch_guest.wasm");
    std::fs::read(&p).unwrap_or_else(|e| {
        panic!(
            "failed to read attachment-dispatch guest wasm at {}: {e}\n\
             Did you build it first?\n  cargo build --target wasm32-wasip1 --release \\\n    \
             --manifest-path crates/wafer-run/tests/attachment_dispatch/Cargo.toml",
            p.display()
        )
    })
}

#[tokio::test]
async fn wasmi_guest_attaches_and_callee_looks_up() {
    let wasm = attachment_dispatch_guest_wasm();
    let block = WasmiBlock::load_from_bytes(&wasm).expect("load attachment-dispatch guest wasm");

    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("Wafer::build");

    wafer
        .register_block("test/attachment-dispatch-guest", Arc::new(block))
        .expect("register attachment-dispatch-guest");

    let wafer = wafer.start().await.expect("start runtime");

    // Drive the "attach_then_call" entry. The guest opens a streaming call
    // back into itself under "test.echo_attachment", attaches a fixed payload,
    // finishes, and concatenates the chunks the callee yielded. The body of
    // the buffered terminal frame is exactly the bytes the callee saw via
    // lookup_attachment — i.e. the original payload, round-tripped through
    // the full stream-attach + carry-forward + lookup chain.
    let output = wafer
        .run_block(
            "test/attachment-dispatch-guest",
            Message::new("test.attach_then_call"),
            InputStream::empty(),
        )
        .await;

    let buf = output
        .collect_buffered()
        .await
        .expect("attach_then_call should complete with a buffered response");

    assert_eq!(
        buf.body,
        vec![0xAA, 0xBB, 0xCC, 0xDD],
        "round-tripped attachment bytes don't match — \
         attachment lost or corrupted between guest attach and callee lookup"
    );
}
