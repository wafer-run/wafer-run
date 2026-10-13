//! A flow forwards its terminal step's streamed response instead of
//! collecting it (see the executor's module docs, "Streamed responses").
//!
//! The terminal step is a top-level step with no `next` that ends the flow
//! — the last step, or one a `next` jumped to — and no `each` / `parallel`.
//! When its block's leading meta declares streaming and a chunk follows, the
//! flow's output leads with the flow message's `resp.*` entries (middleware
//! headers included) and then yields the block's own stream, as it is
//! produced. Every other step output is collected exactly as before.
//!
//! Load-bearing on the executor change: the tests marked "fails on main"
//! wait on a producer that is gated after its first chunk, so a flow that
//! collects never returns within the timeout. The others are regression
//! guards for what must still be collected.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use futures::StreamExt;
use serde_json::json;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use wafer_block::{
    http_codec::{collect_http_response, open_response, OpenedResponse},
    StreamEvent,
};
use wafer_block_security_headers::SecurityHeadersBlock;
use wafer_flow::WaferFlow;
use wafer_run::*;

/// How long a test waits for something that must happen promptly.
const PROMPTLY: Duration = Duration::from_secs(2);

/// Shared between a test and its producers.
#[derive(Default)]
struct Probe {
    /// Released by the test to let a gated producer go on.
    gate: Notify,
    /// Notified (with a stored permit) when a producer sees its cancel token
    /// fire.
    cancelled: Notify,
}

impl Probe {
    async fn assert_producer_cancelled(&self, why: &str) {
        tokio::time::timeout(PROMPTLY, self.cancelled.notified())
            .await
            .unwrap_or_else(|_| panic!("the step's producer was not cancelled: {why}"));
    }
}

/// What a fixture block's producer sends.
#[derive(Clone, Copy)]
enum Script {
    /// SSE trying to loosen the middleware's `X-Frame-Options`: `data:
    /// one`, then waits for the gate, then `data: two` and `Complete`.
    GatedSse,
    /// SSE sent whole, with no gate: `data: one`, `data: two`, `Complete`.
    QuickSse,
    /// `application/octet-stream` carrying JSON in two chunks: `[1,` `2]`.
    JsonStream,
    /// SSE whose leading meta also holds a header no transport can send;
    /// sends a chunk and then waits only for cancellation.
    UnsendableSse,
    /// SSE: `data: one`, then waits for the gate, then an `Error` terminal.
    SseThenError,
    /// HTML whose content type leads the body (no streaming declaration):
    /// `<p>`, `hi</p>`, `Complete`.
    LeadingMetaHtml,
    /// SSE that fails before any body: the content type, then an `Error`.
    SseErrorBeforeBody,
    /// A marker-declared download (`resp.stream`) of a PNG with a declared
    /// length of 6: `abc`, then waits for the gate, then `def`, `Complete`.
    GatedDownload,
}

struct Producer {
    name: &'static str,
    script: Script,
    probe: Arc<Probe>,
}

fn entry(key: &str, value: &str) -> MetaEntry {
    MetaEntry {
        key: key.to_string(),
        value: value.to_string(),
    }
}

/// A step's attempt to loosen the security-headers middleware's
/// `X-Frame-Options: DENY`, which the flow's overlay rules refuse.
fn loosened_framing() -> MetaEntry {
    entry("resp.header.X-Frame-Options", "SAMEORIGIN")
}

#[async_trait::async_trait]
impl Block for Producer {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(self.name, "0.0.1", "http-handler@v1", "streaming fixture")
            .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        let script = self.script;
        let probe = self.probe.clone();
        OutputStream::from_producer(move |sink, cancel| async move {
            let sse = || entry("resp.content_type", "text/event-stream");
            match script {
                Script::GatedSse => {
                    let _ = sink.send_meta(sse()).await;
                    let _ = sink.send_meta(loosened_framing()).await;
                    let _ = sink.send_chunk(b"data: one\n\n".to_vec()).await;
                    if gate(&probe, &cancel).await {
                        let _ = sink.send_chunk(b"data: two\n\n".to_vec()).await;
                        let _ = sink.complete(Vec::new()).await;
                    }
                }
                Script::QuickSse => {
                    let _ = sink.send_meta(sse()).await;
                    let _ = sink.send_chunk(b"data: one\n\n".to_vec()).await;
                    let _ = sink.send_chunk(b"data: two\n\n".to_vec()).await;
                    let _ = sink.complete(Vec::new()).await;
                }
                Script::JsonStream => {
                    let _ = sink
                        .send_meta(entry("resp.content_type", "application/octet-stream"))
                        .await;
                    let _ = sink.send_chunk(b"[1,".to_vec()).await;
                    let _ = sink.send_chunk(b"2]".to_vec()).await;
                    let _ = sink.complete(Vec::new()).await;
                }
                Script::UnsendableSse => {
                    let _ = sink.send_meta(sse()).await;
                    let _ = sink.send_meta(entry("resp.header.X-Bad", "a\nb")).await;
                    let _ = sink.send_chunk(b"data: one\n\n".to_vec()).await;
                    cancel.cancelled().await;
                    probe.cancelled.notify_one();
                }
                Script::LeadingMetaHtml => {
                    let _ = sink
                        .send_meta(entry("resp.content_type", "text/html"))
                        .await;
                    let _ = sink.send_chunk(b"<p>".to_vec()).await;
                    let _ = sink.send_chunk(b"hi</p>".to_vec()).await;
                    let _ = sink.complete(Vec::new()).await;
                }
                Script::SseErrorBeforeBody => {
                    let _ = sink.send_meta(sse()).await;
                    let _ = sink
                        .error(WaferError::new(ErrorCode::NotFound, "no such chat"))
                        .await;
                }
                Script::SseThenError => {
                    let _ = sink.send_meta(sse()).await;
                    let _ = sink.send_chunk(b"data: one\n\n".to_vec()).await;
                    if gate(&probe, &cancel).await {
                        let _ = sink
                            .error(WaferError::new(
                                ErrorCode::Unavailable,
                                "upstream went away",
                            ))
                            .await;
                    }
                }
                Script::GatedDownload => {
                    let _ = sink.send_meta(entry("resp.stream", "1")).await;
                    let _ = sink.send_meta(loosened_framing()).await;
                    let _ = sink
                        .send_meta(entry("resp.content_type", "image/png"))
                        .await;
                    let _ = sink.send_meta(entry("resp.content_length", "6")).await;
                    let _ = sink.send_chunk(b"abc".to_vec()).await;
                    if gate(&probe, &cancel).await {
                        let _ = sink.send_chunk(b"def".to_vec()).await;
                        let _ = sink.complete(Vec::new()).await;
                    }
                }
            }
        })
    }
}

/// Wait for the test to release `probe.gate`; false (after reporting it)
/// when the consumer went away first.
async fn gate(probe: &Probe, cancel: &CancellationToken) -> bool {
    tokio::select! {
        () = probe.gate.notified() => true,
        () = cancel.cancelled() => {
            probe.cancelled.notify_one();
            false
        }
    }
}

/// Echoes its input bytes, as a collected response.
struct Echo;

#[async_trait::async_trait]
impl Block for Echo {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/echo", "0.0.1", "http-handler@v1", "echo fixture")
            .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, input: InputStream) -> OutputStream {
        OutputStream::from_result(input.collect_to_bytes().await)
    }
}

const FIXTURES: &[(&str, Script)] = &[
    ("test/sse", Script::GatedSse),
    ("test/sse-quick", Script::QuickSse),
    ("test/json-stream", Script::JsonStream),
    ("test/sse-unsendable", Script::UnsendableSse),
    ("test/sse-then-error", Script::SseThenError),
    ("test/download", Script::GatedDownload),
    ("test/html", Script::LeadingMetaHtml),
    ("test/sse-error-before-body", Script::SseErrorBeforeBody),
];

/// A started runtime with the security-headers middleware, the fixtures and
/// `flows` (as JSON documents).
async fn runtime(flows: &[serde_json::Value]) -> (Arc<Wafer>, Arc<Probe>) {
    let probe = Arc::new(Probe::default());
    let mut w = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer build is infallible");
    w.register_block(
        "wafer-run/security-headers",
        Arc::new(SecurityHeadersBlock::new()),
    )
    .expect("register security-headers");
    w.register_block("test/echo", Arc::new(Echo))
        .expect("register echo");
    for (name, script) in FIXTURES {
        w.register_block(
            *name,
            Arc::new(Producer {
                name,
                script: *script,
                probe: probe.clone(),
            }),
        )
        .expect("register fixture");
    }
    for flow in flows {
        let mut doc = flow.clone();
        doc["name"] = doc["id"].clone();
        doc["version"] = json!("0.0.1");
        let flow: WaferFlow = serde_json::from_value(doc).expect("valid flow JSON");
        w.add_flow(flow).expect("add flow");
    }
    let wafer = w.start().await.expect("start runtime");
    (wafer, probe)
}

/// Run `flow`; the runner itself must return promptly.
async fn run(wafer: &Wafer, flow: &str, input: &[u8]) -> OutputStream {
    tokio::time::timeout(
        PROMPTLY,
        wafer.run(
            flow,
            Message::new("http.request"),
            InputStream::from_bytes(input.to_vec()),
        ),
    )
    .await
    .unwrap_or_else(|_| panic!("flow '{flow}' did not return within {PROMPTLY:?}"))
}

/// The next event, which must come promptly.
async fn next_event(out: &mut OutputStream) -> StreamEvent {
    tokio::time::timeout(PROMPTLY, out.next())
        .await
        .expect("the next event did not come promptly")
        .expect("the stream ended early")
}

/// Read the leading `Meta` events and the first chunk, promptly.
async fn read_head(out: &mut OutputStream) -> (Vec<MetaEntry>, Vec<u8>) {
    let mut meta = Vec::new();
    loop {
        match next_event(out).await {
            StreamEvent::Meta(entry) => meta.push(entry),
            StreamEvent::Chunk(bytes) => return (meta, bytes),
            other => panic!("expected leading meta then a chunk, got {other:?}"),
        }
    }
}

/// Read the rest of the stream: body bytes and the terminal.
async fn read_rest(out: &mut OutputStream) -> (Vec<u8>, StreamEvent) {
    let mut body = Vec::new();
    loop {
        match next_event(out).await {
            StreamEvent::Chunk(bytes) => body.extend(bytes),
            StreamEvent::Meta(entry) => panic!("unexpected late meta {entry:?}"),
            terminal => return (body, terminal),
        }
    }
}

/// The value of `key` in `meta` (keys compared case-insensitively).
fn value<'a>(meta: &'a [MetaEntry], key: &str) -> Option<&'a str> {
    meta.iter()
        .rev()
        .find(|e| e.key.eq_ignore_ascii_case(key))
        .map(|e| e.value.as_str())
}

/// The stream has nothing ready: the producer is still gated.
fn assert_pending(out: &mut OutputStream) {
    use futures::FutureExt;
    assert!(
        out.next().now_or_never().is_none(),
        "the stream must still be waiting on its producer"
    );
}

fn assert_streamed_sse_head(meta: &[MetaEntry], first: &[u8]) {
    assert_eq!(
        value(meta, "resp.content_type"),
        Some("text/event-stream"),
        "{meta:?}"
    );
    let framing: Vec<&str> = meta
        .iter()
        .filter(|e| e.key.eq_ignore_ascii_case("resp.header.X-Frame-Options"))
        .map(|e| e.value.as_str())
        .collect();
    assert_eq!(
        framing,
        vec!["DENY"],
        "the middleware's headers lead the stream, laid over by the step's with \
         the flow's rules (a step cannot loosen X-Frame-Options): {meta:?}"
    );
    assert_eq!(first, b"data: one\n\n");
}

/// Fails on main: the flow collected the step, so `run` waited for the gate.
#[tokio::test]
async fn a_terminal_steps_sse_reaches_the_caller_before_it_ends() {
    let (wafer, probe) = runtime(&[json!({
        "id": "site",
        "steps": [
            { "id": "security-headers", "block": "wafer-run/security-headers" },
            { "id": "events", "block": "test/sse" },
        ],
    })])
    .await;
    let flow_ended = Arc::new(AtomicBool::new(false));
    let ended = flow_ended.clone();
    wafer
        .hooks
        .on_flow_end(move |_, _| ended.store(true, Ordering::SeqCst));

    let mut out = run(&wafer, "site", b"").await;
    let (meta, first) = read_head(&mut out).await;
    assert_streamed_sse_head(&meta, &first);
    assert_pending(&mut out);
    assert!(
        flow_ended.load(Ordering::SeqCst),
        "flow_end fires at time-to-headers for a streamed response"
    );

    probe.gate.notify_one();
    let (rest, terminal) = read_rest(&mut out).await;
    assert_eq!(rest, b"data: two\n\n");
    assert!(
        matches!(terminal, StreamEvent::Complete { .. }),
        "{terminal:?}"
    );
}

/// Fails on main (the head never arrives). Dropping the flow's output is
/// dropping the step's own stream: its producer is cancelled.
#[tokio::test]
async fn dropping_a_streamed_flow_output_cancels_the_step_producer() {
    let (wafer, probe) = runtime(&[json!({
        "id": "site",
        "steps": [ { "id": "events", "block": "test/sse" } ],
    })])
    .await;

    let mut out = run(&wafer, "site", b"").await;
    let (_, first) = read_head(&mut out).await;
    assert_eq!(first, b"data: one\n\n");
    drop(out);
    probe
        .assert_producer_cancelled("the flow output was dropped")
        .await;
}

/// Regression guard (passes on main): a streaming step with a step after it
/// is collected, and the next step gets its whole body.
#[tokio::test]
async fn a_non_terminal_streaming_step_is_still_collected() {
    let (wafer, _) = runtime(&[json!({
        "id": "site",
        "steps": [
            { "id": "events", "block": "test/sse-quick" },
            { "id": "after", "block": "test/echo" },
        ],
    })])
    .await;

    let mut out = run(&wafer, "site", b"").await;
    match next_event(&mut out).await {
        StreamEvent::Chunk(body) => assert_eq!(body, b"data: one\n\ndata: two\n\n"),
        other => panic!("the flow answers collected, got {other:?}"),
    }
    match next_event(&mut out).await {
        StreamEvent::Complete { meta } => assert_eq!(
            value(&meta, "resp.content_type"),
            Some("text/event-stream"),
            "the collected step's meta is on the flow message: {meta:?}"
        ),
        other => panic!("{other:?}"),
    }
}

/// Regression guard (passes on main): a terminal `each` step and a terminal
/// `parallel` step (and the steps of its branches) collect their streaming
/// outputs — their bodies are assembled from several of them.
#[tokio::test]
async fn each_and_parallel_steps_collect() {
    let (wafer, _) = runtime(&[
        json!({
            "id": "fan-out",
            "steps": [
                { "id": "items", "block": "test/json-stream", "each": "$.input.items" },
            ],
        }),
        json!({
            "id": "branches",
            "steps": [
                {
                    "id": "joined",
                    "block": "test/json-stream",
                    "parallel": [
                        { "steps": [ { "id": "b1", "block": "test/json-stream", "input": {} } ] },
                    ],
                },
            ],
        }),
    ])
    .await;

    for (flow, body) in [
        ("fan-out", b"[[1,2],[1,2]]".as_slice()),
        ("branches", b"[1,2]".as_slice()),
    ] {
        let mut out = run(&wafer, flow, br#"{"items": ["a", "b"]}"#).await;
        match next_event(&mut out).await {
            StreamEvent::Chunk(bytes) => assert_eq!(bytes, body, "flow '{flow}'"),
            other => panic!("flow '{flow}' answers collected, got {other:?}"),
        }
        assert!(
            matches!(next_event(&mut out).await, StreamEvent::Complete { .. }),
            "flow '{flow}'"
        );
    }
}

/// Fails on main (collection waits forever on a producer that never ends).
/// Unsendable leading meta on a streamed terminal fails the flow closed at
/// head time: a 500 carrying the middleware's headers, never the bad one,
/// and the producer is cancelled.
#[tokio::test]
async fn a_streamed_terminal_with_unsendable_leading_meta_fails_closed() {
    let (wafer, probe) = runtime(&[json!({
        "id": "site",
        "steps": [
            { "id": "security-headers", "block": "wafer-run/security-headers" },
            { "id": "events", "block": "test/sse-unsendable" },
        ],
    })])
    .await;

    let out = run(&wafer, "site", b"").await;
    let parts = tokio::time::timeout(PROMPTLY, collect_http_response(out))
        .await
        .expect("the flow's 500 renders promptly");
    assert_eq!(parts.status, 500, "{parts:?}");
    let header = |name: &str| {
        parts
            .headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(header("X-Frame-Options"), Some("DENY"), "{parts:?}");
    assert_eq!(header("X-Bad"), None, "{parts:?}");
    assert_ne!(header("Content-Type"), Some("text/event-stream"));
    probe
        .assert_producer_cancelled("the flow failed at head time")
        .await;
}

/// Fails on main. A step a `next` jumped to, with no `next` of its own,
/// ends the flow even though steps follow it: it is terminal and streams.
#[tokio::test]
async fn a_routed_terminal_step_streams() {
    let (wafer, probe) = runtime(&[json!({
        "id": "site",
        "steps": [
            {
                "id": "security-headers",
                "block": "wafer-run/security-headers",
                "next": [ { "step": "events" } ],
            },
            { "id": "skipped", "block": "test/echo" },
            { "id": "events", "block": "test/sse" },
            { "id": "after-events", "block": "test/echo" },
        ],
    })])
    .await;

    let mut out = run(&wafer, "site", b"").await;
    let (meta, first) = read_head(&mut out).await;
    assert_streamed_sse_head(&meta, &first);
    assert_pending(&mut out);
    probe.gate.notify_one();
    let (rest, terminal) = read_rest(&mut out).await;
    assert_eq!(rest, b"data: two\n\n");
    assert!(matches!(terminal, StreamEvent::Complete { .. }));
}

/// Regression guard (passes on main). A step with `next` entries is never
/// terminal, even when it is the last step and its conditional entry is not
/// taken: validation gives every `next` list a default entry that names a
/// step or a flow, so such a step always routes on — and its conditions are
/// evaluated against its own output, which must be collected first. Here the
/// last step's default entry jumps back to an earlier step, which gets the
/// whole body and is the one that ends the flow.
#[tokio::test]
async fn a_last_step_with_next_entries_collects() {
    let (wafer, probe) = runtime(&[json!({
        "id": "site",
        "steps": [
            {
                "id": "security-headers",
                "block": "wafer-run/security-headers",
                "next": [ { "step": "events" } ],
            },
            { "id": "finish", "block": "test/echo" },
            {
                "id": "events",
                "block": "test/sse",
                "input": {},
                "next": [
                    { "when": "$.input.go == true", "step": "security-headers" },
                    { "step": "finish" },
                ],
            },
        ],
    })])
    .await;

    let running = wafer.run(
        "site",
        Message::new("http.request"),
        InputStream::from_bytes(br#"{"go": false}"#.to_vec()),
    );
    tokio::pin!(running);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), &mut running)
            .await
            .is_err(),
        "the flow must still be collecting the gated step"
    );
    probe.gate.notify_one();
    let mut out = tokio::time::timeout(PROMPTLY, running)
        .await
        .expect("the flow ends once the step completes");
    match next_event(&mut out).await {
        StreamEvent::Chunk(body) => assert_eq!(
            body, b"data: one\n\ndata: two\n\n",
            "the step routed to gets the whole collected body"
        ),
        other => panic!("the flow answers collected, got {other:?}"),
    }
    match next_event(&mut out).await {
        StreamEvent::Complete { meta } => {
            assert_eq!(value(&meta, "resp.content_type"), Some("text/event-stream"));
            assert_eq!(value(&meta, "resp.header.X-Frame-Options"), Some("DENY"));
        }
        other => panic!("{other:?}"),
    }
}

/// Fails on main. A flow transfer runs the target as the chain's last flow,
/// so the target's terminal step streams, with the transferring flow's
/// middleware headers on it.
#[tokio::test]
async fn a_transfer_targets_terminal_step_streams() {
    let (wafer, probe) = runtime(&[
        json!({
            "id": "site",
            "steps": [
                {
                    "id": "security-headers",
                    "block": "wafer-run/security-headers",
                    "next": [ { "flow": "events" } ],
                },
            ],
        }),
        json!({
            "id": "events",
            "steps": [ { "id": "events", "block": "test/sse" } ],
        }),
    ])
    .await;

    let mut out = run(&wafer, "site", b"").await;
    let (meta, first) = read_head(&mut out).await;
    assert_streamed_sse_head(&meta, &first);
    assert_pending(&mut out);
    probe.gate.notify_one();
    let (rest, _) = read_rest(&mut out).await;
    assert_eq!(rest, b"data: two\n\n");
}

/// Fails on main. The `resp.stream` marker and the declared length are not
/// headers, but they survive the overlay onto the flow message and the
/// `resp.*` filter, so the HTTP codec opens the flow's output as the
/// streamed download it is — with the middleware's headers.
#[tokio::test]
async fn a_marker_declared_download_streams_through_the_flow() {
    let (wafer, probe) = runtime(&[json!({
        "id": "site",
        "steps": [
            { "id": "security-headers", "block": "wafer-run/security-headers" },
            { "id": "download", "block": "test/download" },
        ],
    })])
    .await;

    let out = run(&wafer, "site", b"").await;
    let opened = tokio::time::timeout(PROMPTLY, open_response(out))
        .await
        .expect("the codec opens the flow's output promptly");
    let OpenedResponse::Streamed(response) = opened else {
        panic!("a marker-declared download must open streamed");
    };
    assert_eq!(response.status, 200);
    assert_eq!(response.content_length, Some(6));
    let header = |name: &str| {
        response
            .headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };
    assert_eq!(header("Content-Type").as_deref(), Some("image/png"));
    let framing: Vec<_> = response
        .headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case("X-Frame-Options"))
        .map(|(_, v)| v.as_str())
        .collect();
    assert_eq!(
        framing,
        vec!["DENY"],
        "the step's looser value is refused: {:?}",
        response.headers
    );
    assert!(
        response
            .headers
            .iter()
            .all(|(n, _)| !n.to_ascii_lowercase().contains("stream")),
        "the marker is never a header: {:?}",
        response.headers
    );

    // The length-completing chunk is held to the terminal (wafer-block's
    // contract), so release the producer and read the body to its end.
    probe.gate.notify_one();
    let body: Vec<_> = tokio::time::timeout(PROMPTLY, response.body.collect::<Vec<_>>())
        .await
        .expect("the body ends promptly");
    let bytes: Vec<u8> = body
        .into_iter()
        .flat_map(|item| item.expect("a clean body"))
        .collect();
    assert_eq!(bytes, b"abcdef");
}

/// Fails on main. An `Error` the producer sends after its head reaches the
/// caller as a stream event after the chunks it already sent — not as a
/// flow error that replaces the response.
#[tokio::test]
async fn a_producer_error_after_the_head_reaches_the_caller_as_an_item() {
    let (wafer, probe) = runtime(&[json!({
        "id": "site",
        "steps": [
            { "id": "security-headers", "block": "wafer-run/security-headers" },
            { "id": "events", "block": "test/sse-then-error" },
        ],
    })])
    .await;

    let mut out = run(&wafer, "site", b"").await;
    let (meta, first) = read_head(&mut out).await;
    assert_streamed_sse_head(&meta, &first);
    probe.gate.notify_one();
    let (rest, terminal) = read_rest(&mut out).await;
    assert!(rest.is_empty());
    match terminal {
        StreamEvent::Error(err) => {
            assert_eq!(err.code, ErrorCode::Unavailable);
            assert_eq!(err.message, "upstream went away");
        }
        other => panic!("expected the producer's Error, got {other:?}"),
    }
}

/// Regression guard (passes on main). A terminal step's output that does not
/// stream is collected exactly as before: leading meta with no streaming
/// declaration, and a streaming declaration whose stream ends before any
/// body (here in an `Error`, which is then a flow error carrying the
/// middleware's headers, as for any step).
#[tokio::test]
async fn a_terminal_step_that_does_not_stream_is_collected() {
    let (wafer, _) = runtime(&[
        json!({
            "id": "html",
            "steps": [
                { "id": "security-headers", "block": "wafer-run/security-headers" },
                { "id": "page", "block": "test/html" },
            ],
        }),
        json!({
            "id": "failing",
            "steps": [
                { "id": "security-headers", "block": "wafer-run/security-headers" },
                { "id": "events", "block": "test/sse-error-before-body" },
            ],
        }),
    ])
    .await;

    let mut out = run(&wafer, "html", b"").await;
    match next_event(&mut out).await {
        StreamEvent::Chunk(body) => assert_eq!(body, b"<p>hi</p>"),
        other => panic!("the flow answers collected, got {other:?}"),
    }
    match next_event(&mut out).await {
        StreamEvent::Complete { meta } => {
            assert_eq!(value(&meta, "resp.content_type"), Some("text/html"));
            assert_eq!(value(&meta, "resp.header.X-Frame-Options"), Some("DENY"));
        }
        other => panic!("{other:?}"),
    }

    let mut out = run(&wafer, "failing", b"").await;
    match next_event(&mut out).await {
        StreamEvent::Error(err) => {
            assert_eq!(err.code, ErrorCode::NotFound);
            assert_eq!(
                value(&err.meta, "resp.header.X-Frame-Options"),
                Some("DENY"),
                "the flow error carries the middleware's headers: {err:?}"
            );
            assert_eq!(value(&err.meta, "resp.content_type"), None);
        }
        other => panic!("the flow fails with the step's error, got {other:?}"),
    }
}
