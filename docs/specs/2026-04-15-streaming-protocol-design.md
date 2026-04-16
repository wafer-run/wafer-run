# Wafer-Run Streaming-Native Block Protocol

**Status:** Design
**Date:** 2026-04-15
**Scope:** wafer-run runtime, wafer-block types, wafer-block-http-listener, wasmi host/guest ABI, all existing blocks in wafer-run + solobase monorepos
**Prerequisite for:** LLM service refactor (separate spec)

## Summary

Replace wafer-run's one-shot request/response block protocol with a streaming-native protocol. Every block call — across every transport (in-process Rust, HTTP, wasmi guest/host, browser service worker, Cloudflare Workers) — becomes a server-streaming interaction: one-shot input headers plus an optional input byte stream in; a stream of `StreamEvent`s out that terminates with exactly one terminal event.

The existing `BlockResult` / `Action` types are deleted. Buffered block behavior becomes a trivial case of streaming (a stream of length 2: one `Chunk` carrying the body, one `Complete` terminal). Cancellation, backpressure, and error propagation are first-class and uniform across all transports.

## Motivation

Today the protocol is strictly one-shot: `Block::handle(ctx, msg) -> BlockResult` where `BlockResult` has a single optional `Response`. The HTTP listener materializes a full `Vec<u8>` before emitting any body; `ctx.call_block` returns one blob. There is no streaming concept anywhere in the protocol — no `Action::Stream`, no chunk delivery, no cancellation signaling, no backpressure.

This has several concrete costs:

- **LLM traffic can't stream end-to-end.** `provider-llm` returns full completions; `local-llm` bypasses the protocol entirely by forwarding via a service-worker hack (ai-bridge.js) directly to the DOM. Browser UX streams; the wafer-run protocol doesn't carry the stream.
- **Large file uploads have no backpressure.** Inputs are buffered `Vec<u8>`, so uploads must be fully received before a block starts processing.
- **Composition requires buffering.** Piping one block's output into another's input forces a `.collect().await` between stages.
- **No cancellation propagation.** A cancelled HTTP request keeps the block running, which for LLM means paid-for OpenAI tokens are generated after the user has left.
- **Every future streaming use case (progress events, log tailing, event buses, audio) either invents its own out-of-band channel or can't ship.**

The alternative explored during brainstorming — a typed in-process service registry (`ctx.service::<dyn T>()`) exposed alongside the existing JSON message protocol — was rejected because it creates two parallel access paths, leaves third-party wasmi guests permanently unable to stream, and carries a compat-shim smell. With the workspace in active development, the root-cause fix is to make the protocol streaming-native.

## Architectural Principle

One protocol, one mental model: **every block interaction is headers + a byte stream in, and headers + a typed event stream out.** Buffered is a degenerate case of streaming, not a separate path. All transports (in-process, HTTP, wasmi, browser SW, CF Workers) express this same model; only the wire encoding differs.

## Types

All types live in `wafer-block` so they are shared between the runtime and the block SDK.

```rust
// The input side: one-shot headers + streaming body bytes.
pub struct Message {
    pub kind: String,
    pub meta: Vec<MetaEntry>,
    // `data: Vec<u8>` field removed — body bytes now flow via InputStream.
}

pub struct InputStream { /* wraps mpsc::Receiver<Vec<u8>> + CancellationToken */ }

impl InputStream {
    pub fn empty() -> Self;
    pub fn from_bytes(bytes: Vec<u8>) -> Self;            // single-chunk stream
    pub fn from_stream<S>(stream: S) -> Self              // arbitrary Stream<Item = Vec<u8>>
    where S: Stream<Item = Vec<u8>> + Send + 'static;
    pub fn cancel_token(&self) -> &CancellationToken;

    /// Drains the stream and concatenates all chunks into a single Vec.
    /// The common helper for buffered blocks that want the whole body.
    pub async fn collect_to_bytes(self) -> Vec<u8>;
}

// Stream implementation so consumers get StreamExt methods (.next(), .collect(), etc.)
impl Stream for InputStream {
    type Item = Vec<u8>;
    // Forwards to the underlying receiver.
}

// The output side: typed event stream terminating with one terminal event.
pub enum StreamEvent {
    /// Body bytes. Non-terminal. Zero or more per stream.
    Chunk(Vec<u8>),

    /// Trailing or mid-stream metadata (e.g., Content-Type declaration,
    /// progress updates, token-usage info). Non-terminal. Zero or more.
    Meta(MetaEntry),

    /// Terminal: stream completed normally. Carries trailing metadata.
    Complete { meta: Vec<MetaEntry> },

    /// Terminal: stream failed. Prior chunks are NOT retroactively invalidated;
    /// consumers decide their own semantics.
    Error(WaferError),

    /// Terminal: block explicitly dropped the request (HTTP 204-equivalent).
    /// Valid only with no preceding chunks.
    Drop,

    /// Terminal: block forwards to another block instead of handling.
    /// Valid only with no preceding chunks.
    Continue(Message),
}

pub struct OutputStream { /* wraps mpsc::Receiver<StreamEvent> + CancellationToken */ }

impl OutputStream {
    /// Buffered block helper: emits one Chunk followed by Complete with no trailing meta.
    pub fn respond(bytes: Vec<u8>) -> Self;

    /// Error helper: emits a single terminal Error event.
    pub fn error(err: WaferError) -> Self;

    /// Drop helper: emits a single terminal Drop event.
    pub fn drop_request() -> Self;

    /// Continue helper: emits a single terminal Continue(msg) event.
    pub fn continue_with(msg: Message) -> Self;

    /// Streaming constructor: returns (stream, sink, cancel_token).
    /// The sink is used by the producing task to yield chunks and emit the terminal.
    /// Default channel capacity is 16; use `new_streaming_with_capacity` to override.
    pub fn new_streaming() -> (OutputStream, OutputSink, CancellationToken);
    pub fn new_streaming_with_capacity(cap: usize) -> (OutputStream, OutputSink, CancellationToken);

    /// The paired cancel token. Fires automatically when the stream is dropped,
    /// or can be triggered explicitly by the consumer (e.g., HTTP adapter on client disconnect).
    pub fn cancel_token(&self) -> &CancellationToken;

    /// Drain the stream into a single buffered response. Concatenates all Chunk payloads;
    /// collects trailing Meta from Complete. Returns Err if the terminal was Error/Drop/Continue.
    pub async fn collect_buffered(self) -> Result<BufferedResponse, TerminalNotResponse>;

    /// Returns a `Stream<Item = Vec<u8>>` view that yields just the Chunk payloads,
    /// filtering out Meta events and stopping at the first terminal.
    /// Useful for piping one block's body into another block's InputStream:
    /// `ctx.call_block(next, msg, InputStream::from_stream(prev.body_stream())).await`.
    /// Non-Chunk terminal events are swallowed by this view; callers that need to
    /// react to Error / Drop / Continue should consume the full stream directly.
    pub fn body_stream(self) -> impl Stream<Item = Vec<u8>> + Send + 'static;
}

impl Stream for OutputStream {
    type Item = StreamEvent;
    // Forwards to the underlying receiver.
}

pub struct OutputSink { /* wraps mpsc::Sender<StreamEvent> */ }

impl OutputSink {
    /// Yield a chunk. Awaits if the channel is full (backpressure).
    /// Returns Err if the consumer dropped the stream — producer should bail.
    pub async fn send_chunk(&self, bytes: Vec<u8>) -> Result<(), SinkClosed>;

    /// Emit a mid-stream Meta event.
    pub async fn send_meta(&self, entry: MetaEntry) -> Result<(), SinkClosed>;

    /// Terminal. Consumes the sink. Exactly one of these MUST be called.
    pub async fn complete(self, meta: Vec<MetaEntry>) -> Result<(), SinkClosed>;
    pub async fn error(self, err: WaferError) -> Result<(), SinkClosed>;
    pub async fn drop_request(self) -> Result<(), SinkClosed>;
    pub async fn continue_with(self, msg: Message) -> Result<(), SinkClosed>;
}

/// Convenience buffered view produced by `OutputStream::collect_buffered()`.
pub struct BufferedResponse {
    pub body: Vec<u8>,       // concatenated Chunks
    pub meta: Vec<MetaEntry>, // mid-stream Meta + Complete.meta, in order
}
```

### Invariants

1. Every `OutputStream` yields exactly one terminal event (`Complete` | `Error` | `Drop` | `Continue`) as its last event, after which the underlying channel closes.
2. `Drop` and `Continue` are valid only as the first-and-only event; they cannot follow `Chunk` or `Meta`. This is enforced by a debug assertion in `OutputSink` and a runtime check in the HTTP adapter.
3. `Error` may follow any number of `Chunk` / `Meta` events. Consumers decide whether prior chunks are usable.
4. `Meta` events should precede their semantic effect — e.g., a `Content-Type: text/event-stream` declaration should be emitted before any `Chunk`, because the HTTP adapter commits to SSE framing on seeing it. **Enforcement:** If the HTTP adapter sees a `Content-Type: text/event-stream` Meta after already receiving a `Chunk`, it logs a warning and continues in buffered mode — the late declaration is ignored. This is a runtime check, not a debug assertion, to prevent silent misbehavior in production.
5. Blocks emit raw body bytes in `Chunk` events. Wire-format framing (SSE frames, HTTP chunked encoding, binary WebSocket frames) is the responsibility of the transport adapter, not the block. This keeps blocks transport-agnostic: the same streaming block serves SSE to browsers, chunked JSON to APIs, and binary frames to wasmi guests without change.

### Ergonomic Helpers

**`OutputStream::from_producer`** — platform-aware spawn with auto-complete:

```rust
// Works on native AND wasm32. Closure returns () — if no terminal
// is sent on the sink, Complete { meta: vec![] } is auto-emitted on drop.
OutputStream::from_producer(|sink, cancel| async move {
    sink.send_meta(MetaEntry::content_type("text/event-stream")).await.ok();
    let mut upstream = service.stream(cancel).await.unwrap();
    while let Some(chunk) = upstream.next().await {
        if sink.send_chunk(chunk).await.is_err() { return; }
    }
    // auto-complete on drop
})
```

On native: wraps in `tokio::spawn`. On wasm32: wraps in `wasm_bindgen_futures::spawn_local`. The `spawn_producer` function in `wafer-block/src/spawn.rs` provides the platform abstraction.

**`OutputStream::from_result`** — one-liner for buffered blocks:

```rust
OutputStream::from_result(self.process(&msg, &body).await)
// Ok(bytes) -> respond(bytes), Err(e) -> error(e)
```

**`Context::call_block_buffered`** — shorthand for the 90% call pattern:

```rust
let response = ctx.call_block_buffered("wafer-run/database", msg, &body).await?;
// Returns Result<BufferedResponse, WaferError>
```

Default method on `Context`; builds `InputStream` from `&[u8]`, calls `call_block`, drains via `collect_buffered`, and converts `TerminalNotResponse` to `WaferError`.

**`OutputStream::body_stream_or_error`** — error-propagating body stream:

```rust
let chunks = prev_output.body_stream_or_error();
// Stream<Item = Result<Vec<u8>, WaferError>>
// Propagates Error terminals instead of swallowing them.
```

**`OutputSink` auto-complete on Drop:**

When an `OutputSink` is dropped without having called any terminal method (`complete`, `error`, `drop_request`, `continue_with`), the `Drop` impl auto-emits `Complete { meta: vec![] }` via `try_send`. This makes `from_producer` ergonomic: the closure simply returns and the stream completes. If a terminal was explicitly sent, the auto-complete is suppressed via an internal `terminal_sent` flag.

## The Block Trait

```rust
#[async_trait]
pub trait Block: Send + Sync + 'static {
    async fn handle(
        &self,
        ctx: &dyn Context,
        msg: Message,
        input: InputStream,
    ) -> OutputStream;

    // Lifecycle, info, capabilities unchanged.
}
```

### Buffered block example (most of the codebase)

```rust
#[async_trait]
impl Block for AuthBlock {
    async fn handle(&self, ctx: &dyn Context, msg: Message, mut input: InputStream) -> OutputStream {
        let body: Vec<u8> = input.collect_to_bytes().await;
        let result = self.process(&msg, &body).await;
        match result {
            Ok(response) => OutputStream::respond(response),
            Err(e) => OutputStream::error(e),
        }
    }
}
```

### Streaming block example (LLM chat completion)

```rust
#[async_trait]
impl Block for LlmBlock {
    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let request: ChatRequest = serde_json::from_slice(&msg.meta_value("body")).unwrap();
        let (stream, sink, cancel) = OutputStream::new_streaming();

        let service = self.service.clone();
        tokio::spawn(async move {
            if let Err(_) = sink.send_meta(MetaEntry::content_type("text/event-stream")).await {
                return;
            }

            let mut upstream = match service.chat_stream(request, cancel.clone()).await {
                Ok(s) => s,
                Err(e) => { let _ = sink.error(e.into()).await; return; }
            };

            // Block emits raw body bytes. The HTTP adapter frames them as SSE
            // events because of the `text/event-stream` meta declared above.
            while let Some(token) = upstream.next().await {
                match token {
                    Ok(chunk) => {
                        if sink.send_chunk(chunk.into_bytes()).await.is_err() { return; }
                    }
                    Err(e) => { let _ = sink.error(e.into()).await; return; }
                }
            }

            let _ = sink.complete(vec![]).await;
        });

        stream
    }
}
```

## Context: `ctx.call_block`

```rust
#[async_trait]
pub trait Context: Send + Sync {
    async fn call_block(
        &self,
        name: &str,
        msg: Message,
        input: InputStream,
    ) -> OutputStream;

    // other methods unchanged
}
```

Callers that only need buffered semantics drain the stream immediately:

```rust
let response = ctx.call_block("wafer-run/database", msg, InputStream::empty())
    .await
    .collect_buffered()
    .await?;
```

Callers that stream consume the output natively:

```rust
let mut out = ctx.call_block("wafer-run/llm", msg, InputStream::empty()).await;
while let Some(evt) = out.next().await {
    match evt {
        StreamEvent::Chunk(bytes) => send_to_client(bytes).await?,
        StreamEvent::Meta(_) => {},
        StreamEvent::Complete { .. } => break,
        StreamEvent::Error(e) => return Err(e.into()),
        StreamEvent::Drop | StreamEvent::Continue(_) => break,
    }
}
```

## Cancellation Semantics

Cancellation is **drop-triggered with an explicit token** for cooperative in-flight interruption.

- The consumer triggers cancellation by dropping `OutputStream`, OR by calling `cancel_token().cancel()` explicitly.
- The producing block receives the cancellation via the `CancellationToken` returned from `OutputStream::new_streaming()`. It MUST `select!` this token against any long-running await (HTTP requests to upstream providers, WebLLM generation, DB cursors, etc.) to abort promptly.
- `OutputSink::send_chunk` returns `Err(SinkClosed)` after the consumer is gone; producers that don't `select!` on the token will discover cancellation on their next send attempt. Both signals exist and are compatible; neither is redundant.
- HTTP adapters wire client-disconnect into cancellation (axum's `Request::extensions` / CF Workers' `AbortSignal` / browser SW's `ReadableStream` cancel).

The `CancellationToken` is `tokio_util::sync::CancellationToken` (no custom type).

**Input-side cancellation:** When a block returns an `OutputStream` before fully consuming its `InputStream`, the runtime fires the `InputStream`'s `CancellationToken`. This signals the upstream producer (e.g., the HTTP adapter streaming a request body) to stop sending. For large uploads, this means a block that rejects on the first chunk causes the upload to abort rather than buffering the full body. Transport adapters are responsible for wiring the `InputStream` cancel token to their upstream source (e.g., dropping the axum `Body`).

## Backpressure

Pull-based throughout. `OutputStream` is a `Stream<Item = StreamEvent>`; consumers pull at their own pace. When a task-to-task decoupling exists (producer task feeding a consumer task via the sink), a bounded `tokio::sync::mpsc::channel(N)` sits between them. The producing task's `sink.send_chunk().await` awaits when full, which naturally slows the producer, which in turn slows whatever the producer is draining from upstream (e.g., stops polling an OpenAI SSE stream, which triggers TCP flow control, which slows the OpenAI server). End-to-end backpressure.

Default channel capacity: **16**. Override via `OutputStream::new_streaming_with_capacity(n)`.

## Transports

### In-process Rust (native binary, whole-app WASM for browser SW, whole-app WASM for CF)

Direct pass-through. The native dispatcher in `wafer-run/crates/wafer-run/src/context.rs` becomes:

```rust
// Lookup block by name, clone Arc, call handle.
let block = self.all_blocks.get(name)?.clone();
block.handle(&sub_ctx, msg, input).await
```

The `OutputStream` returned by the callee is the `OutputStream` the caller sees. No serialization. No copying. Cancellation propagates via the shared `CancellationToken`.

### HTTP listener (`wafer-block-http-listener`)

```rust
pub async fn wafer_output_to_response(
    mut stream: OutputStream,
) -> axum::http::Response<Body> { ... }
```

The adapter peeks at events until it either:

1. Sees a `StreamEvent::Meta` with `Content-Type: text/event-stream` → switches to streaming mode. Wraps the remainder of the stream in `axum::body::Body::from_stream`, mapping each subsequent `StreamEvent::Chunk` to an SSE frame. `Complete` closes the body cleanly; `Error` emits a final SSE `event: error` frame and closes.
2. Sees `Chunk` or `Complete` without a preceding SSE `Meta` declaration → buffered mode. Collects until terminal, emits one `Response` body.
3. Sees `Drop` first → emits 204.
4. Sees `Continue(msg)` first → looks up the target block from the message and re-dispatches.
   Continue re-dispatch is limited to a maximum depth of **8**. If the depth is exceeded, the adapter emits a 508 (Loop Detected) response with a JSON error body. This prevents infinite forwarding loops between blocks.
5. Sees `Error` at any point → emits appropriate error response (500 + JSON body for pre-headers, or SSE error frame for mid-stream in streaming mode).

Client disconnect (axum's `Body` cancel) triggers `output.cancel_token().cancel()`, which the producing block sees via its token.

Input side: axum `Body` stream → `InputStream::from_stream(request.into_body())`. Chunked transfer encoding and large uploads work natively.

### Browser service worker adapter (`solobase-web`)

Symmetric to axum. Incoming `fetch`'s `request.body()` becomes an `InputStream`. Outgoing response is constructed as a `ReadableStream` whose pull callback wraps the `OutputStream` via `wasm_bindgen_futures::stream::stream_from_rust`. `Content-Type: text/event-stream` causes the `ReadableStream` to emit SSE-framed chunks.

Cancellation: the `ReadableStream`'s `cancel` callback fires the `CancellationToken`.

### Cloudflare Workers adapter (`solobase-cloudflare`)

Uses `workers-rs` `Response::from_stream` for output and `Request::stream()` for input. Same shape as axum; `AbortSignal` from the Worker environment wires to the cancellation token.

### wasmi host ↔ guest ABI

This is the only novel ABI work. The existing `__wafer_host_call_block` single-shot trap is replaced by a small family of host imports supporting pull-based chunk flow. Shaped to align with the WASI Preview 3 `stream<T>` primitive so that when wasmi ships P3 streams (expected 12–18 months out per the component-model roadmap), the guest SDK can migrate without runtime-side changes.

Guest-visible ABI (all trap-resumable):

```
__wafer_host_call_begin(name_ptr, name_len, msg_ptr, msg_len) -> call_handle: u32
__wafer_host_call_input_send(call_handle, chunk_ptr, chunk_len) -> result: u32
    // result: 0 = accepted, 1 = consumer cancelled (stop sending)
__wafer_host_call_input_close(call_handle) -> ()
__wafer_host_call_output_recv(call_handle, buf_ptr, buf_cap) -> event_kind: u32, len_written: u32
    // event_kind: 0=Chunk, 1=Meta, 2=Complete, 3=Error, 4=Drop, 5=Continue
    // Traps and resumes once per event; host queues events and delivers on each call.
__wafer_host_call_cancel(call_handle) -> ()
    // Explicitly cancel the call; fires the shared CancellationToken.
__wafer_host_call_end(call_handle) -> ()
    // Releases the host-side resources. Called after the terminal event is received.
```

Symmetric pattern for guests that implement a `Block` themselves (host calling into guest):

```
Guest exports:
__wafer_guest_handle_begin(msg_ptr, msg_len) -> call_handle: u32
__wafer_guest_handle_input_recv(call_handle, buf_ptr, buf_cap) -> len_read: u32, end: u32
    // end: 0 = more chunks may follow, 1 = input stream closed (no more chunks)
__wafer_guest_handle_output_send(call_handle, event_kind: u32, payload_ptr, payload_len) -> result: u32
    // result: 0 = accepted, 1 = consumer cancelled (stop sending)
__wafer_guest_handle_cancel(call_handle) -> ()
__wafer_guest_handle_end(call_handle) -> ()
```

The guest SDK hides this ABI behind the same `InputStream` / `OutputStream` types that in-process Rust uses. From a block author's perspective, writing a wasmi guest block looks identical to writing a native block — same trait, same types, same helpers.

Implementation detail: the ABI uses wasmi's `TypedResumableCall` for each host import that awaits asynchronously (e.g., `call_output_recv` when no event is yet queued). The existing resumable-call loop is extended to handle repeated traps per call rather than a single trap-resolve-resume cycle.

## Standards Adopted

Per "adopt existing standards at each boundary, design minimally only where needed":

| Boundary | Standard Used |
|---|---|
| Internal Rust stream type | `futures::Stream` |
| Task-to-task channel | `tokio::sync::mpsc` (bounded) |
| Cancellation | `tokio_util::sync::CancellationToken` |
| HTTP out (streaming) | Server-Sent Events (text/event-stream) |
| HTTP in (streaming) | Chunked Transfer Encoding |
| wasmi host↔guest ABI | Hand-rolled pull-based, aligned with WASI Preview 3 `stream<T>` direction |

## Removals

The following types and code paths are deleted outright:

- `BlockResult` struct
- `Action` enum (variants absorbed into `StreamEvent` terminals)
- `Result_` type alias
- `Message::data` field (moved to `InputStream`)
- `Response` struct (subsumed by `BufferedResponse` for buffered collectors; the streaming case never materializes this type)
- `wafer_result_to_response` (replaced by `wafer_output_to_response`)
- `discovery::streaming = false` hardcoded field (streaming is now universal)
- ai-bridge.js postMessage bridge in solobase-web (browser WebLLM calls become a normal `BrowserLlmService` impl flowing through the streaming protocol; the LLM refactor spec covers the details)

## Migration

Single atomic change across both monorepos. The Rust type system is the migration guide: change the trait, recompile, fix errors the compiler points to, recompile until clean. No compat shims, no dual-path transition.

Order of operations within the single PR:

1. Land new types in `wafer-block`: `InputStream`, `OutputStream`, `OutputSink`, `StreamEvent`, `BufferedResponse`. Remove old `BlockResult`, `Action`, `Response`.
2. Update `Block::handle` signature in `wafer-block`.
3. Update `Context::call_block` signature in `wafer-block` and its native impl in `wafer-run`.
4. Update every existing block's `handle` implementation:
   - Service blocks (`wafer-run/database`, `wafer-run/storage`, `wafer-run/sqlite`, `wafer-run/postgres`, `wafer-run/local-storage`, `wafer-run/s3`): mostly wrap existing logic with `OutputStream::respond(old_body)`.
   - Solobase feature blocks (all in `solobase-core/src/blocks/`): same wrap pattern.
   - Only LLM-related blocks warrant real streaming code — which is explicitly deferred to Spec 2.
5. Update HTTP listener (`wafer-block-http-listener/src/lib.rs`) to produce `wafer_output_to_response`.
6. Update browser service worker adapter in `solobase-web`.
7. Update CF Workers adapter in `solobase-cloudflare`.
8. Extend wasmi host imports in `wafer-run/crates/wafer-run/src/wasm/wasmi_loader.rs`. Update guest-side SDK (`wafer-run/sdks/rust`) to expose the new ABI behind the streaming types.
9. Remove `BlockResult`, `Action`, old ABI imports.
10. Update tests across both workspaces. Add new tests covering streaming termination invariants, cancellation propagation, backpressure, and SSE framing.

## Testing Strategy

The new design's composability makes blocks trivially testable as pure async functions.

### Unit tests per block

```rust
#[tokio::test]
async fn auth_block_rejects_invalid_token() {
    let block = AuthBlock::new(fake_store());
    let ctx = FakeContext::new();
    let out = block.handle(
        &ctx,
        Message::new("POST").with_meta("Authorization", "Bearer bad"),
        InputStream::empty(),
    ).await;
    let buffered = out.collect_buffered().await;
    assert!(matches!(buffered, Err(TerminalNotResponse::Error(_))));
}
```

### Streaming behavior tests

```rust
#[tokio::test]
async fn llm_block_yields_tokens_and_completes() {
    let service = fake_llm_service_returning(["hello", " ", "world"]);
    let block = LlmBlock::new(Arc::new(service));
    let ctx = FakeContext::new();
    let mut out = block.handle(&ctx, chat_msg(), InputStream::empty()).await;

    let mut events = vec![];
    while let Some(e) = out.next().await { events.push(e); }

    assert!(matches!(events[0], StreamEvent::Meta(_)));
    assert_eq!(events[1], StreamEvent::Chunk(b"hello".to_vec()));
    assert_eq!(events[2], StreamEvent::Chunk(b" ".to_vec()));
    assert_eq!(events[3], StreamEvent::Chunk(b"world".to_vec()));
    assert!(matches!(events[4], StreamEvent::Complete { .. }));
}
```

### Cancellation tests

```rust
#[tokio::test]
async fn dropping_output_stream_cancels_producer() {
    let cancel_observer = Arc::new(AtomicBool::new(false));
    let service = fake_llm_service_observing_cancel(cancel_observer.clone());
    let block = LlmBlock::new(Arc::new(service));
    let ctx = FakeContext::new();

    let out = block.handle(&ctx, chat_msg(), InputStream::empty()).await;
    drop(out);

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(cancel_observer.load(Ordering::SeqCst));
}
```

### Composition tests

```rust
#[tokio::test]
async fn pipeline_streams_end_to_end() {
    let runtime = TestRuntime::new()
        .with_block("a/upper", upper_case_block())   // chunk -> CHUNK
        .with_block("a/reverse", reverse_block());   // CHUNK -> KNUHC

    let input = InputStream::from_bytes(b"hello".to_vec());
    let stage1 = runtime.call("a/upper", msg(), input).await;
    let stage2 = runtime.call("a/reverse", msg(), InputStream::from_stream(stage1.body_stream())).await;

    let buffered = stage2.collect_buffered().await.unwrap();
    assert_eq!(buffered.body, b"OLLEH");
}
```

### Transport-level integration tests

- axum listener with an SSE-declaring streaming block; assert EventSource-style framing on the wire.
- Browser service-worker adapter exercised via wasm-bindgen-test (exists already; add SSE case).
- CF Workers adapter via `workers-rs` test harness.
- wasmi host↔guest round-trip: a guest block that emits a known stream; the host receives it through the new ABI; assert events match.
- Backpressure: producer that floods chunks into a slow consumer; assert producer await time grows proportional to consumer lag (channel fills, producer blocks on send).

### Invariant tests

- Terminal-event presence: every well-formed block emits exactly one terminal.
- `Drop` / `Continue` invariant: a block that emits a `Chunk` then `Continue` fails a debug assertion in `OutputSink::continue_with`.
- Cancellation propagation: `CancellationToken` fires uniformly across all four contexts.

## Out of Scope (Explicit)

- **LLM service refactor itself** — separate spec (Spec 2). This spec only delivers the protocol; the LLM trait + impls + block consolidation come next.
- **gRPC or network streaming between wafer-run nodes.** Not needed today; when/if adopted, it becomes a separate transport adapter; the protocol above is transport-agnostic enough that no core changes are required.
- **WASI Preview 3 stream adoption.** Tracked for future migration; the current ABI is shaped to enable it when wasmi ships the runtime support.
- **Bidirectional message streaming (client-streaming of typed messages, not byte streams).** The current design supports byte-streamed inputs, which covers file upload and similar use cases. Streaming typed messages into a block is not in demand; if needed later, `Message` itself could gain a tail stream of `Message`s, but that's a separate future evolution.
- **Flow/pipeline streaming plumbing.** The declarative flow engine (`wafer-flow`) currently composes blocks via `call_block`. Flows inherit streaming support automatically through the updated `call_block` signature — each flow step's `OutputStream` can be piped to the next step's `InputStream` via `body_stream()` or consumed directly. No flow engine changes are needed for this spec. If flows later need streaming-aware routing logic (e.g., "route based on first chunk"), that's a separate evolution.

## Open Implementation Notes

- The default channel capacity (16) is an informed guess. Benchmark before shipping: LLM token streams may prefer 32–64 for bursty upstream SSE; file chunk streams may prefer 4–8 for larger chunk sizes. The `_with_capacity` escape hatch exists for tuning.
- The exact ergonomics of `OutputStream::collect_buffered` on `Error` / `Drop` / `Continue` terminals (returned as `TerminalNotResponse` variants) should be finalized during implementation; the current sketch may be split into separate methods (`try_collect_buffered` vs `collect_or_handle`) if callers commonly need different patterns.
- `MetaEntry::content_type(...)` helper constructors should cover common cases (`text/event-stream`, `application/json`, `application/octet-stream`, etc.) so blocks don't hand-construct meta entries.
- Error-frame format for SSE when an error occurs mid-stream: `event: error\ndata: {json}\n\n`, followed by stream close. Confirm against typical EventSource client parsers; browsers accept this.
- **Drop-on-slow-consumer variant.** The default `OutputStream::new_streaming(cap)` applies strict backpressure: `send_chunk` awaits when the channel is full. For real-time use cases (WebSocket game ticks, telemetry streams, live logs) a drop-on-slow variant is preferable — when the channel is full, drop the oldest queued chunk and accept the new one. Plan to add `OutputStream::new_streaming_dropping(cap)` as an alternative constructor when the first WebSocket or gaming block materializes. Requires no protocol change; purely an `OutputSink` send-semantics switch.
- **WebSocket / long-lived bidirectional adapter.** Not in this spec's scope, but the protocol is intentionally shaped to support it: one WS upgrade = one `Block::handle` invocation running for the connection's lifetime, incoming WS messages mapped to `InputStream` chunks (one message = one chunk), outgoing `Chunk` events mapped to WS frames, cancellation mapped to WS close. An adapter can be added in `wafer-block-http-listener` (or a dedicated crate) without protocol changes.
