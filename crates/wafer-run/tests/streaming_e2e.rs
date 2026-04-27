//! End-to-end streaming protocol integration tests.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use wafer_block::{
    stream::StreamEvent,
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
};
use wafer_run::{
    block::{Block, BlockInfo},
    context::Context,
    types::*,
    Wafer,
};

// ---------------------------------------------------------------------------
// Test blocks
// ---------------------------------------------------------------------------

struct EchoBlock;

#[async_trait]
impl Block for EchoBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/echo", "0.1.0", "handler@v1", "echo")
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: Message, input: InputStream) -> OutputStream {
        OutputStream::respond(input.collect_to_bytes().await)
    }
}

struct UpperBlock;

#[async_trait]
impl Block for UpperBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/upper", "0.1.0", "handler@v1", "upper")
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: Message, input: InputStream) -> OutputStream {
        let s = String::from_utf8_lossy(&input.collect_to_bytes().await)
            .to_uppercase()
            .into_bytes();
        OutputStream::respond(s)
    }
}

struct PipeBlock;

#[async_trait]
impl Block for PipeBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/pipe", "0.1.0", "handler@v1", "pipe")
    }
    async fn handle(&self, ctx: &dyn Context, _msg: Message, input: InputStream) -> OutputStream {
        let body = input.collect_to_bytes().await;
        ctx.call_block(
            "test/upper",
            Message::new("fwd"),
            InputStream::from_bytes(body),
        )
        .await
    }
}

struct ErrorBlock;

#[async_trait]
impl Block for ErrorBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/error", "0.1.0", "handler@v1", "error")
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::error(WaferError::new(ErrorCode::Internal, "deliberate"))
    }
}

struct DropBlock;

#[async_trait]
impl Block for DropBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/drop", "0.1.0", "handler@v1", "drop")
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::drop_request()
    }
}

struct MultiChunkBlock;

#[async_trait]
impl Block for MultiChunkBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/multi", "0.1.0", "handler@v1", "multi")
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        let (stream, sink, _cancel) = OutputStream::new_streaming();
        tokio::spawn(async move {
            let _ = sink.send_chunk(b"hello".to_vec()).await;
            let _ = sink.send_chunk(b" ".to_vec()).await;
            let _ = sink.send_chunk(b"world".to_vec()).await;
            let _ = sink.complete(vec![]).await;
        });
        stream
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn single_flow(block: &str) -> wafer_flow::WaferFlow {
    wafer_flow::WaferFlow {
        id: block.to_string(),
        name: format!("Test: {block}"),
        version: "0.0.1".to_string(),
        description: None,
        input: None,
        output: None,
        steps: vec![wafer_flow::Step {
            id: "root".to_string(),
            block: block.to_string(),
            input: None,
            next: None,
            each: None,
            parallel: None,
            description: None,
            config: None,
        }],
        config: None,
        blocks: None,
        config_map: None,
        config_defaults: None,
    }
}

async fn run(w: &Wafer, flow_id: &str, body: Vec<u8>) -> Result<Vec<u8>, TerminalNotResponse> {
    let msg = Message::new("test");
    let input = InputStream::from_bytes(body);
    let output = w.run(flow_id, msg, input).await;
    output.collect_buffered().await.map(|b| b.body)
}

fn setup() -> Wafer {
    let mut w = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer build is infallible");
    for (name, block) in [
        ("test/echo", Arc::new(EchoBlock) as Arc<dyn Block>),
        ("test/upper", Arc::new(UpperBlock)),
        ("test/pipe", Arc::new(PipeBlock)),
        ("test/error", Arc::new(ErrorBlock)),
        ("test/drop", Arc::new(DropBlock)),
        ("test/multi", Arc::new(MultiChunkBlock)),
    ] {
        w.register_block(name, block).unwrap();
        w.add_flow(single_flow(name));
    }
    w
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn echo_round_trip() {
    let mut w = setup();
    w.resolve().await.unwrap();
    let body = run(&w, "test/echo", b"hello streaming".to_vec())
        .await
        .unwrap();
    assert_eq!(body, b"hello streaming");
}

#[tokio::test]
async fn uppercase_transforms() {
    let mut w = setup();
    w.resolve().await.unwrap();
    let body = run(&w, "test/upper", b"hello".to_vec()).await.unwrap();
    assert_eq!(body, b"HELLO");
}

#[tokio::test]
async fn pipe_chains_via_call_block() {
    let mut w = setup();
    w.resolve().await.unwrap();
    let body = run(&w, "test/pipe", b"hello pipe".to_vec()).await.unwrap();
    assert_eq!(body, b"HELLO PIPE");
}

#[tokio::test]
async fn error_returns_terminal() {
    let mut w = setup();
    w.resolve().await.unwrap();
    let result = run(&w, "test/error", vec![]).await;
    match result {
        Err(TerminalNotResponse::Error(e)) => {
            assert_eq!(e.code, ErrorCode::Internal);
            assert_eq!(e.message, "deliberate");
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn drop_returns_terminal() {
    let mut w = setup();
    w.resolve().await.unwrap();
    let result = run(&w, "test/drop", vec![]).await;
    assert!(matches!(result, Err(TerminalNotResponse::Drop)));
}

#[tokio::test]
async fn multi_chunk_individual_events() {
    // Verify the block's direct streaming output (bypassing flow executor).
    let block = MultiChunkBlock;
    let ctx = MockCtx;
    let msg = Message::new("test");
    let output = block.handle(&ctx, msg, InputStream::empty()).await;
    let events: Vec<_> = output.collect().await;
    assert_eq!(events.len(), 4);
    assert_eq!(events[0], StreamEvent::Chunk(b"hello".to_vec()));
    assert_eq!(events[1], StreamEvent::Chunk(b" ".to_vec()));
    assert_eq!(events[2], StreamEvent::Chunk(b"world".to_vec()));
    assert!(matches!(events[3], StreamEvent::Complete { .. }));
}

struct MockCtx;

#[async_trait]
impl Context for MockCtx {
    async fn call_block(&self, _: &str, _: Message, _: InputStream) -> OutputStream {
        OutputStream::error(WaferError::new(ErrorCode::Unimplemented, "mock"))
    }
    fn is_cancelled(&self) -> bool {
        false
    }
    fn config_get(&self, _: &str) -> Option<&str> {
        None
    }
}

#[tokio::test]
async fn multi_chunk_collect_buffered() {
    let mut w = setup();
    w.resolve().await.unwrap();
    let body = run(&w, "test/multi", vec![]).await.unwrap();
    assert_eq!(body, b"hello world");
}

#[tokio::test]
async fn empty_input() {
    let mut w = setup();
    w.resolve().await.unwrap();
    let body = run(&w, "test/echo", vec![]).await.unwrap();
    assert!(body.is_empty());
}
