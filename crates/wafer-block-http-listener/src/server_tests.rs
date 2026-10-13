//! The listener driven over real sockets: `Init` + `bind` start the actual
//! server, and raw HTTP/1.1 bytes exercise it the way a client (or a slow,
//! hostile one) would.

use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::Notify,
};
use wafer_block::{
    http_codec::{META_HTTP_PATH, META_RESP_CONTENT_LENGTH, META_RESP_STREAM, STREAM_MARKER_VALUE},
    meta::{META_REQ_CLIENT_IP, META_RESP_CONTENT_TYPE},
    Block, ErrorCode, InputStream, LifecycleEvent, LifecycleType, Message, MetaEntry, OutputSink,
    OutputStream, WaferError,
};
use wafer_block_macro::wafer_async_trait;

use crate::{
    tests::{init_event, NoopCtx},
    HttpListenerBlock,
};

/// A response body far larger than loopback socket buffers, so a client
/// that does not read it leaves the server's write blocked.
const BIG_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// Chunks `/flood` offers its client: 64 MiB in all, far more than socket
/// and hyper buffers hold.
const FLOOD_CHUNKS: usize = 1_000;

/// Size of each `/flood` chunk.
const FLOOD_CHUNK_BYTES: usize = 64 * 1024;

/// What `/collect` read from its request body: the length of a whole body,
/// or the code of the stream's failure.
type Collected = Result<usize, wafer_block::ErrorCode>;

/// Runtime behind the test listener. By path: `/collect` records what it
/// read from the body (see [`Collected`]) and answers with the stream's error
/// if it failed, `/first-chunk` answers with the body's first chunk without
/// reading further, `/big` answers with
/// [`BIG_RESPONSE_BYTES`], `/slow` answers after 500 ms, `/hang` after a
/// minute, `/bad-headers` with a `Content-Security-Policy` no transport can
/// send beside a valid header, `/owned-headers` with transport-owned headers
/// and a lower-case `content-type`, `/xff` with the request's
/// `X-Forwarded-For` header as the message carries it, `/cut-off` with a
/// producer that sends part of a body and ends without a terminal event.
///
/// Streamed responses (leading meta declaring streaming), each from a
/// producer that signals [`TestRuntime::cancelled`] when its cancel token
/// fires: `/sse` sends one event, waits for [`TestRuntime::release`], then
/// sends a second and completes; `/sse-hang` sends one event and waits to be
/// cancelled; `/sse-error` sends one event, waits for `release`, then ends
/// with an `Error`; `/sse-bad-header` declares SSE beside a header no
/// transport can send and never ends; `/flood` offers [`FLOOD_CHUNKS`] chunks,
/// counting each one its sink accepted in [`TestRuntime::sent`];
/// `/length-exact`, `/length-overrun` and `/length-zero` declare a
/// `resp.content_length` of 5, 5 and 0 and send `abcde`, `abcde` + `fgh` and
/// `x`.
///
/// Streamed responses that read the request body inside the producer, each
/// recording what they read in [`TestRuntime::collected`]:
/// `/body-then-stream` reads the body first and then streams one event and
/// waits to be cancelled, whatever the body did; `/stream-then-body` streams
/// one event first, then reads the body and ends with its failure, if any.
///
/// Anything else answers with the client IP the listener put on the message.
/// Every path except `/collect` and `/first-chunk` reads the whole body first
/// and ignores how it ended.
#[derive(Default)]
struct TestRuntime {
    collected: Arc<parking_lot::Mutex<Vec<Collected>>>,
    /// Lets `/sse` and `/sse-error` go on past their first event.
    release: Arc<Notify>,
    /// Notified when a streaming producer's cancel token fires.
    cancelled: Arc<Notify>,
    /// Chunks `/flood` has handed to its sink.
    sent: Arc<AtomicUsize>,
}

fn meta(key: &str, value: &str) -> MetaEntry {
    MetaEntry {
        key: key.into(),
        value: value.into(),
    }
}

/// An SSE producer's head: the content type that declares it streams.
async fn send_sse_head(sink: &OutputSink) {
    sink.send_meta(meta(META_RESP_CONTENT_TYPE, "text/event-stream"))
        .await
        .expect("send the SSE content type");
}

/// A download's head: the streaming marker, a content type that would not
/// stream on its own, and the declared body length.
async fn send_length_head(sink: &OutputSink, length: &str) {
    for entry in [
        meta(META_RESP_STREAM, STREAM_MARKER_VALUE),
        meta(META_RESP_CONTENT_TYPE, "text/plain"),
        meta(META_RESP_CONTENT_LENGTH, length),
    ] {
        sink.send_meta(entry).await.expect("send the download head");
    }
}

#[wafer_async_trait]
impl wafer_block::Runtime for TestRuntime {
    async fn run(&self, _flow_id: &str, msg: Message, mut input: InputStream) -> OutputStream {
        use futures::StreamExt;

        match msg.get_meta(META_HTTP_PATH) {
            "/collect" => {
                let body = input.collect_to_bytes().await;
                self.collected
                    .lock()
                    .push(body.as_ref().map(Vec::len).map_err(|e| e.code));
                return match body {
                    Ok(bytes) => OutputStream::respond(format!("{} bytes", bytes.len()).into()),
                    Err(e) => OutputStream::error(e),
                };
            }
            "/body-then-stream" => {
                let (collected, cancelled) = (self.collected.clone(), self.cancelled.clone());
                return OutputStream::from_producer(|sink, cancel| async move {
                    let body = input.collect_to_bytes().await;
                    collected
                        .lock()
                        .push(body.as_ref().map(Vec::len).map_err(|e| e.code));
                    send_sse_head(&sink).await;
                    let _ = sink.send_chunk(b"data: one\n\n".to_vec()).await;
                    cancel.cancelled().await;
                    cancelled.notify_one();
                });
            }
            "/stream-then-body" => {
                let collected = self.collected.clone();
                return OutputStream::from_producer(|sink, _cancel| async move {
                    send_sse_head(&sink).await;
                    let _ = sink.send_chunk(b"data: one\n\n".to_vec()).await;
                    let body = input.collect_to_bytes().await;
                    collected
                        .lock()
                        .push(body.as_ref().map(Vec::len).map_err(|e| e.code));
                    let _ = match body {
                        Ok(_) => sink.complete(Vec::new()).await,
                        Err(e) => sink.error(e).await,
                    };
                });
            }
            "/first-chunk" => {
                return match input.next().await {
                    Some(Ok(chunk)) => OutputStream::respond(chunk),
                    other => OutputStream::respond(format!("{other:?}").into()),
                };
            }
            _ => {
                let _ = match input.collect_to_bytes().await {
                    Ok(bytes) => bytes,
                    Err(e) => return OutputStream::error(e),
                };
            }
        }
        match msg.get_meta(META_HTTP_PATH) {
            "/big" => OutputStream::respond(vec![b'x'; BIG_RESPONSE_BYTES]),
            "/slow" => {
                tokio::time::sleep(Duration::from_millis(500)).await;
                OutputStream::respond(b"slow done".to_vec())
            }
            "/bad-headers" => OutputStream::respond_with_meta(
                b"body".to_vec(),
                vec![
                    MetaEntry {
                        key: "resp.header.Content-Security-Policy".into(),
                        value: "script-src \u{2019}self\u{2019}".into(),
                    },
                    MetaEntry {
                        key: "resp.header.X-Good".into(),
                        value: "ok".into(),
                    },
                ],
            ),
            "/owned-headers" => OutputStream::respond_with_meta(
                b"body".to_vec(),
                vec![
                    MetaEntry {
                        key: "resp.header.Content-Length".into(),
                        value: "999".into(),
                    },
                    MetaEntry {
                        key: "resp.header.content-type".into(),
                        value: "text/plain".into(),
                    },
                    MetaEntry {
                        key: "resp.header.X-Good".into(),
                        value: "ok".into(),
                    },
                ],
            ),
            "/xff" => OutputStream::respond(msg.header("x-forwarded-for").as_bytes().to_vec()),
            "/cut-off" => OutputStream::from_producer(|sink, _cancel| async move {
                let _ = sink.send_chunk(b"partial body".to_vec()).await;
            }),
            "/hang" => {
                tokio::time::sleep(Duration::from_secs(60)).await;
                OutputStream::respond(b"hang done".to_vec())
            }
            "/sse" => {
                let release = self.release.clone();
                OutputStream::from_producer(|sink, _cancel| async move {
                    send_sse_head(&sink).await;
                    let _ = sink.send_chunk(b"data: one\n\n".to_vec()).await;
                    release.notified().await;
                    let _ = sink.send_chunk(b"data: two\n\n".to_vec()).await;
                    let _ = sink.complete(Vec::new()).await;
                })
            }
            "/sse-hang" => {
                let cancelled = self.cancelled.clone();
                OutputStream::from_producer(|sink, cancel| async move {
                    send_sse_head(&sink).await;
                    let _ = sink.send_chunk(b"data: one\n\n".to_vec()).await;
                    cancel.cancelled().await;
                    cancelled.notify_one();
                })
            }
            "/sse-error" => {
                let release = self.release.clone();
                OutputStream::from_producer(|sink, _cancel| async move {
                    send_sse_head(&sink).await;
                    let _ = sink.send_chunk(b"data: one\n\n".to_vec()).await;
                    release.notified().await;
                    let _ = sink
                        .error(WaferError::new(ErrorCode::Internal, "upstream failed"))
                        .await;
                })
            }
            "/sse-bad-header" => {
                let cancelled = self.cancelled.clone();
                OutputStream::from_producer(|sink, cancel| async move {
                    send_sse_head(&sink).await;
                    let _ = sink.send_meta(meta("resp.header.X-Bad", "a\nb")).await;
                    let _ = sink.send_chunk(b"data: one\n\n".to_vec()).await;
                    cancel.cancelled().await;
                    cancelled.notify_one();
                })
            }
            "/flood" => {
                let (sent, cancelled) = (self.sent.clone(), self.cancelled.clone());
                OutputStream::from_producer(|sink, cancel| async move {
                    sink.send_meta(meta(META_RESP_CONTENT_TYPE, "application/octet-stream"))
                        .await
                        .expect("send the content type");
                    for _ in 0..FLOOD_CHUNKS {
                        if sink
                            .send_chunk(vec![b'x'; FLOOD_CHUNK_BYTES])
                            .await
                            .is_err()
                        {
                            break;
                        }
                        sent.fetch_add(1, Ordering::SeqCst);
                    }
                    if cancel.is_cancelled() {
                        cancelled.notify_one();
                    } else {
                        let _ = sink.complete(Vec::new()).await;
                    }
                })
            }
            "/length-exact" => OutputStream::from_producer(|sink, _cancel| async move {
                send_length_head(&sink, "5").await;
                let _ = sink.send_chunk(b"abcde".to_vec()).await;
                let _ = sink.complete(Vec::new()).await;
            }),
            "/length-overrun" => OutputStream::from_producer(|sink, _cancel| async move {
                send_length_head(&sink, "5").await;
                let _ = sink.send_chunk(b"abcde".to_vec()).await;
                let _ = sink.send_chunk(b"fgh".to_vec()).await;
                let _ = sink.complete(Vec::new()).await;
            }),
            "/length-zero" => OutputStream::from_producer(|sink, _cancel| async move {
                send_length_head(&sink, "0").await;
                let _ = sink.send_chunk(b"x".to_vec()).await;
                let _ = sink.complete(Vec::new()).await;
            }),
            _ => OutputStream::respond(msg.get_meta(META_REQ_CLIENT_IP).as_bytes().to_vec()),
        }
    }

    async fn run_block(&self, _block_name: &str, msg: Message, input: InputStream) -> OutputStream {
        self.run("", msg, input).await
    }
}

/// A running listener on a loopback port. Dropping it drops the block, whose
/// shutdown sender going away stops the server.
struct Server {
    block: HttpListenerBlock,
    addr: SocketAddr,
    runtime: Arc<TestRuntime>,
}

impl Server {
    /// Start a listener with `extra` merged into its Init config and wait
    /// until it accepts connections.
    async fn start(extra: serde_json::Value) -> Self {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("reserve a loopback port")
            .port();
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let mut config = serde_json::json!({ "listen": addr.to_string(), "flow": "echo" });
        for (key, value) in extra.as_object().expect("extra config is an object") {
            config[key] = value.clone();
        }
        let block = HttpListenerBlock::new();
        block
            .lifecycle(&NoopCtx, init_event(&config))
            .await
            .expect("test config passes Init");
        let runtime = Arc::new(TestRuntime::default());
        let dyn_runtime: Arc<dyn wafer_block::Runtime> = runtime.clone();
        block.bind(Box::new(dyn_runtime));

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            // The probe connection closes at once, which frees its slot; the
            // short sleep after lets the server notice before a test that
            // counts connections starts.
            if TcpStream::connect(addr).await.is_ok() {
                tokio::time::sleep(Duration::from_millis(100)).await;
                break;
            }
            assert!(
                Instant::now() < deadline,
                "listener never came up on {addr}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Self {
            block,
            addr,
            runtime,
        }
    }

    /// Wait until `/collect` has recorded `n` bodies, then return them.
    async fn collected(&self, n: usize) -> Vec<Collected> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let seen = self.runtime.collected.lock().clone();
            if seen.len() >= n {
                return seen;
            }
            assert!(
                Instant::now() < deadline,
                "only {} of {n} bodies reached the block: {seen:?}",
                seen.len()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Stop the listener through its lifecycle, as the runtime does.
    async fn stop(&self) {
        self.block
            .lifecycle(
                &NoopCtx,
                LifecycleEvent {
                    event_type: LifecycleType::Stop,
                    data: Vec::new(),
                },
            )
            .await
            .expect("Stop succeeds");
    }

    async fn connect(&self) -> TcpStream {
        TcpStream::connect(self.addr)
            .await
            .expect("connect to the listener")
    }
}

/// Read until the server closes the connection, or fail after `within`.
async fn read_until_closed(
    stream: &mut (impl AsyncRead + Unpin),
    within: Duration,
) -> Option<String> {
    let mut buf = Vec::new();
    match tokio::time::timeout(within, stream.read_to_end(&mut buf)).await {
        // A reset counts as closed too; keep what arrived.
        Ok(_) => Some(String::from_utf8_lossy(&buf).into_owned()),
        Err(_) => None,
    }
}

/// Read until `needle` has arrived, or fail after `within` (or on close).
async fn read_until(
    stream: &mut (impl AsyncRead + Unpin),
    needle: &str,
    within: Duration,
) -> String {
    let deadline = Instant::now() + within;
    let mut got = Vec::new();
    let mut buf = vec![0u8; 16 * 1024];
    while !String::from_utf8_lossy(&got).contains(needle) {
        let left = deadline.saturating_duration_since(Instant::now());
        let n = tokio::time::timeout(left, stream.read(&mut buf))
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "{needle:?} did not arrive within {within:?}; got {:?}",
                    String::from_utf8_lossy(&got)
                )
            })
            .expect("read");
        assert!(
            n > 0,
            "closed before {needle:?} arrived: {:?}",
            String::from_utf8_lossy(&got)
        );
        got.extend_from_slice(&buf[..n]);
    }
    String::from_utf8_lossy(&got).into_owned()
}

/// Wait for a streaming producer to see its cancel token fire.
async fn producer_cancelled(server: &Server, within: Duration, why: &str) {
    tokio::time::timeout(within, server.runtime.cancelled.notified())
        .await
        .unwrap_or_else(|_| panic!("the producer was not cancelled within {within:?}: {why}"));
}

/// A trusted proxy that appends its hop as a separate `X-Forwarded-For`
/// line (HAProxy `option forwardfor`): the client-written first line must not
/// become the client IP.
#[tokio::test]
async fn multi_line_xff_from_trusted_proxy_resolves_the_real_client() {
    let server = Server::start(serde_json::json!({ "trusted_proxies": "127.0.0.1" })).await;
    let mut stream = server.connect().await;
    stream
        .write_all(
            b"GET / HTTP/1.1\r\nHost: t\r\n\
              X-Forwarded-For: 203.0.113.99\r\n\
              X-Forwarded-For: 198.51.100.7\r\n\
              Connection: close\r\n\r\n",
        )
        .await
        .unwrap();
    let response = read_until_closed(&mut stream, Duration::from_secs(5))
        .await
        .expect("response arrives");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(
        response.ends_with("\r\n\r\n198.51.100.7"),
        "client IP must come from the proxy-appended line: {response}"
    );
}

/// Every `X-Forwarded-For` line reaches the message's header, joined in
/// wire order — not only the last one.
#[tokio::test]
async fn repeated_request_header_lines_reach_the_message_joined() {
    let server = Server::start(serde_json::json!({})).await;
    let mut stream = server.connect().await;
    stream
        .write_all(
            b"GET /xff HTTP/1.1\r\nHost: t\r\n\
              X-Forwarded-For: 203.0.113.99\r\n\
              X-Forwarded-For: 198.51.100.7\r\n\
              Connection: close\r\n\r\n",
        )
        .await
        .unwrap();
    let response = read_until_closed(&mut stream, Duration::from_secs(5))
        .await
        .expect("response arrives");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(
        response.ends_with("\r\n\r\n203.0.113.99, 198.51.100.7"),
        "{response}"
    );
}

/// A header no transport can send fails the response closed: a uniform 500
/// keeping the response's other headers, never the page without its CSP.
#[tokio::test]
async fn an_unsendable_response_header_fails_closed_as_a_500() {
    let server = Server::start(serde_json::json!({})).await;
    let mut stream = server.connect().await;
    stream
        .write_all(b"GET /bad-headers HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let response = read_until_closed(&mut stream, Duration::from_secs(5))
        .await
        .expect("response arrives");
    let lower = response.to_ascii_lowercase();
    assert!(response.starts_with("HTTP/1.1 500"), "{response}");
    assert!(lower.contains("\r\nx-good: ok\r\n"), "{response}");
    assert!(!lower.contains("content-security-policy"), "{response}");
    assert!(
        response.ends_with(r#"{"error":"Internal","message":"internal server error"}"#),
        "{response}"
    );
}

/// A body whose producer ends without a terminal event is a 500, never a
/// 200 carrying the truncated prefix as if it were the whole body.
#[tokio::test]
async fn a_body_cut_off_by_its_producer_is_a_500() {
    let server = Server::start(serde_json::json!({})).await;
    let mut stream = server.connect().await;
    stream
        .write_all(b"GET /cut-off HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let response = read_until_closed(&mut stream, Duration::from_secs(5))
        .await
        .expect("response arrives");
    assert!(response.starts_with("HTTP/1.1 500"), "{response}");
    assert!(!response.contains("partial body"), "{response}");
}

/// Transport-owned headers are dropped and the response stands; a
/// lower-case `content-type` header is the one Content-Type, and the body is
/// framed by the transport, not by the block's `Content-Length`.
#[tokio::test]
async fn transport_owned_response_headers_are_dropped() {
    let server = Server::start(serde_json::json!({})).await;
    let mut stream = server.connect().await;
    stream
        .write_all(b"GET /owned-headers HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let response = read_until_closed(&mut stream, Duration::from_secs(5))
        .await
        .expect("response arrives");
    let lower = response.to_ascii_lowercase();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(lower.contains("\r\nx-good: ok\r\n"), "{response}");
    assert!(lower.contains("\r\ncontent-length: 4\r\n"), "{response}");
    assert!(!lower.contains("999"), "{response}");
    assert_eq!(lower.matches("\r\ncontent-type:").count(), 1, "{response}");
    assert!(
        lower.contains("\r\ncontent-type: text/plain\r\n"),
        "{response}"
    );
    assert!(response.ends_with("\r\n\r\nbody"), "{response}");
}

/// A request repeating a single-valued header is refused with 400, not
/// joined into one ambiguous value (RFC 9110 §5.3, RFC 9112 §6.3). hyper
/// itself refuses differing `Content-Length` lines and folds identical ones
/// into one, which RFC 9112 §6.3 permits; `Host`, `Authorization` and
/// `Content-Type` reach the listener, which refuses them.
#[tokio::test]
async fn a_repeated_single_valued_request_header_is_a_400() {
    let server = Server::start(serde_json::json!({})).await;
    for repeated in [
        "Host: t\r\nHost: u\r\n",
        "Host: t\r\nAuthorization: Bearer a\r\nauthorization: Bearer b\r\n",
        "Host: t\r\nContent-Type: text/plain\r\nContent-Type: application/json\r\n",
        "Host: t\r\nContent-Length: 0\r\nContent-Length: 5\r\n",
    ] {
        let mut stream = server.connect().await;
        stream
            .write_all(format!("GET / HTTP/1.1\r\n{repeated}Connection: close\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let response = read_until_closed(&mut stream, Duration::from_secs(5))
            .await
            .expect("response arrives");
        assert!(
            response.starts_with("HTTP/1.1 400"),
            "{repeated:?} must be refused: {response}"
        );
    }
}

/// Slowloris: a client that starts a request head and never finishes it is
/// disconnected once `header_read_timeout_secs` runs out.
#[tokio::test]
async fn stalled_request_head_is_closed_after_header_read_timeout() {
    let server = Server::start(serde_json::json!({ "header_read_timeout_secs": 1 })).await;
    let mut stream = server.connect().await;
    let started = Instant::now();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: t\r\nX-Slow: ")
        .await
        .unwrap();
    read_until_closed(&mut stream, Duration::from_secs(5))
        .await
        .expect("the server must close a connection whose head stalls");
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "closed before the timeout ran out: {:?}",
        started.elapsed()
    );
}

/// A client that sends its head, promises a 1000-byte body and stalls gets a
/// 408 and a closed connection once `body_read_timeout_secs` runs out.
#[tokio::test]
async fn stalled_request_body_gets_408_after_body_read_timeout() {
    let server = Server::start(serde_json::json!({ "body_read_timeout_secs": 1 })).await;
    let mut stream = server.connect().await;
    let started = Instant::now();
    stream
        .write_all(b"POST / HTTP/1.1\r\nHost: t\r\nContent-Length: 1000\r\n\r\nabc")
        .await
        .unwrap();
    let response = read_until_closed(&mut stream, Duration::from_secs(5))
        .await
        .expect("the server must answer and close a connection whose body stalls");
    assert!(response.starts_with("HTTP/1.1 408"), "{response}");
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "answered before the timeout ran out: {:?}",
        started.elapsed()
    );
}

/// The block reads a stalled body as a timeout failure, and the client gets
/// 408 even though the block answered with its own error.
#[tokio::test]
async fn stalled_request_body_reaches_the_block_as_a_timeout() {
    let server = Server::start(serde_json::json!({ "body_read_timeout_secs": 1 })).await;
    let mut stream = server.connect().await;
    stream
        .write_all(b"POST /collect HTTP/1.1\r\nHost: t\r\nContent-Length: 1000\r\n\r\nabc")
        .await
        .unwrap();
    let response = read_until_closed(&mut stream, Duration::from_secs(5))
        .await
        .expect("the server answers a stalled body");
    assert!(response.starts_with("HTTP/1.1 408"), "{response}");
    assert_eq!(
        server.collected(1).await,
        vec![Err(wafer_block::ErrorCode::DeadlineExceeded)]
    );
}

/// The body streams: the block reads the first chunk while the client is
/// still sending the rest, and can answer before the body is complete.
#[tokio::test]
async fn request_body_reaches_the_block_before_it_is_complete() {
    let server = Server::start(serde_json::json!({})).await;
    let mut stream = server.connect().await;
    stream
        .write_all(b"POST /first-chunk HTTP/1.1\r\nHost: t\r\nContent-Length: 1000\r\n\r\nfirst")
        .await
        .unwrap();
    let mut buf = vec![0u8; 4096];
    let mut got = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !got.ends_with(b"first") {
        let left = deadline.saturating_duration_since(Instant::now());
        let n = tokio::time::timeout(left, stream.read(&mut buf))
            .await
            .expect("the block answers from the first chunk, before the body is complete")
            .expect("read");
        assert!(n > 0, "closed early: {}", String::from_utf8_lossy(&got));
        got.extend_from_slice(&buf[..n]);
    }
    assert!(
        got.starts_with(b"HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&got)
    );
}

/// A chunked body that grows past `max_body_bytes` reaches the block as a
/// failure, not as its first `max_body_bytes`, and the client gets 413.
#[tokio::test]
async fn chunked_body_over_the_cap_reaches_the_block_as_a_failure() {
    let server = Server::start(serde_json::json!({ "max_body_bytes": 8 })).await;
    let mut stream = server.connect().await;
    stream
        .write_all(
            b"POST /collect HTTP/1.1\r\nHost: t\r\nTransfer-Encoding: chunked\r\n\r\n\
              5\r\nhello\r\n5\r\nworld\r\n0\r\n\r\n",
        )
        .await
        .unwrap();
    let response = read_until_closed(&mut stream, Duration::from_secs(5))
        .await
        .expect("response arrives");
    assert!(response.starts_with("HTTP/1.1 413"), "{response}");
    assert_eq!(
        server.collected(1).await,
        vec![Err(wafer_block::ErrorCode::ResourceExhausted)]
    );
}

/// A `Content-Length` over the cap is refused before dispatch.
#[tokio::test]
async fn announced_body_over_the_cap_is_refused_before_dispatch() {
    let server = Server::start(serde_json::json!({ "max_body_bytes": 8 })).await;
    let mut stream = server.connect().await;
    stream
        .write_all(b"POST /collect HTTP/1.1\r\nHost: t\r\nContent-Length: 9\r\n\r\n")
        .await
        .unwrap();
    let response = read_until_closed(&mut stream, Duration::from_secs(5))
        .await
        .expect("response arrives");
    assert!(response.starts_with("HTTP/1.1 413"), "{response}");
    assert!(server.runtime.collected.lock().is_empty());
}

/// A client that disconnects mid-body: the block reads a failure, never the
/// bytes that arrived as if they were the whole body.
#[tokio::test]
async fn dropped_connection_mid_body_reaches_the_block_as_a_failure() {
    let server = Server::start(serde_json::json!({})).await;
    let mut stream = server.connect().await;
    stream
        .write_all(b"POST /collect HTTP/1.1\r\nHost: t\r\nContent-Length: 1000\r\n\r\nabc")
        .await
        .unwrap();
    // Let the block start reading, then go away.
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(stream);
    assert_eq!(
        server.collected(1).await,
        vec![Err(wafer_block::ErrorCode::InvalidArgument)]
    );
}

/// A whole body arrives whole, however it is framed.
#[tokio::test]
async fn whole_bodies_reach_the_block_whole() {
    let server = Server::start(serde_json::json!({})).await;
    for request in [
        &b"POST /collect HTTP/1.1\r\nHost: t\r\nContent-Length: 10\r\nConnection: close\r\n\r\n\
           helloworld"[..],
        &b"POST /collect HTTP/1.1\r\nHost: t\r\nTransfer-Encoding: chunked\r\n\
           Connection: close\r\n\r\n5\r\nhello\r\n5\r\nworld\r\n0\r\n\r\n"[..],
    ] {
        let mut stream = server.connect().await;
        stream.write_all(request).await.unwrap();
        let response = read_until_closed(&mut stream, Duration::from_secs(5))
            .await
            .expect("response arrives");
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with("10 bytes"), "{response}");
    }
    assert_eq!(server.collected(2).await, vec![Ok(10), Ok(10)]);
}

/// At `max_connections` open connections the listener serves nobody else
/// until one closes.
#[tokio::test]
async fn max_connections_holds_further_clients_until_a_slot_frees() {
    let server = Server::start(serde_json::json!({ "max_connections": 1 })).await;
    // Takes the only slot and sends nothing.
    let idle = server.connect().await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut waiting = server.connect().await;
    waiting
        .write_all(b"GET / HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut probe = [0u8; 1];
    assert!(
        tokio::time::timeout(Duration::from_millis(800), waiting.read(&mut probe))
            .await
            .is_err(),
        "a second connection was served while the cap of 1 was in use"
    );

    drop(idle);
    // The timed-out read was cancelled before any byte arrived, so this
    // reads the whole response.
    let response = read_until_closed(&mut waiting, Duration::from_secs(5))
        .await
        .expect("the waiting client is served once the slot frees");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
}

/// A request head dripped one byte at a time still has to be complete by the
/// header deadline: progress does not extend it.
#[tokio::test]
async fn dripped_request_head_is_closed_at_the_header_deadline() {
    let server = Server::start(serde_json::json!({ "header_read_timeout_secs": 1 })).await;
    let (mut reader, mut writer) = server.connect().await.into_split();
    let started = Instant::now();
    writer
        .write_all(b"GET / HTTP/1.1\r\nHost: t\r\nX-Drip: ")
        .await
        .unwrap();
    let drip = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if writer.write_all(b"a").await.is_err() {
                break;
            }
        }
    });
    read_until_closed(&mut reader, Duration::from_secs(5))
        .await
        .expect("the server must close a connection whose head never completes");
    drip.abort();
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "closed before the timeout ran out: {:?}",
        started.elapsed()
    );
}

/// An idle keep-alive connection is closed once `header_read_timeout_secs`
/// passes without the next request.
#[tokio::test]
async fn idle_keep_alive_connection_is_closed_by_the_header_timeout() {
    let server = Server::start(serde_json::json!({ "header_read_timeout_secs": 1 })).await;
    let mut stream = server.connect().await;
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: t\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    let mut chunk = [0u8; 1024];
    while !response.ends_with(b"127.0.0.1") {
        let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk))
            .await
            .expect("first response arrives")
            .unwrap();
        assert!(n > 0, "closed before the first response completed");
        response.extend_from_slice(&chunk[..n]);
    }
    let idle_since = Instant::now();
    let rest = read_until_closed(&mut stream, Duration::from_secs(5))
        .await
        .expect("the server must close an idle keep-alive connection");
    assert!(
        rest.is_empty(),
        "unexpected bytes after the response: {rest}"
    );
    assert!(
        idle_since.elapsed() >= Duration::from_millis(900),
        "closed before the timeout ran out: {:?}",
        idle_since.elapsed()
    );
}

/// A client that requests a large response and never reads it is dropped
/// once the write makes no progress for `write_timeout_secs`, freeing its
/// connection slot.
#[tokio::test]
async fn client_that_stops_reading_is_dropped_after_write_timeout() {
    let server = Server::start(serde_json::json!({
        "write_timeout_secs": 1,
        "max_connections": 1,
    }))
    .await;
    let mut stalled = server.connect().await;
    stalled
        .write_all(b"GET /big HTTP/1.1\r\nHost: t\r\n\r\n")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // The only slot is held by `stalled` until its write stalls out.
    let mut next = server.connect().await;
    next.write_all(b"GET / HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let response = read_until_closed(&mut next, Duration::from_secs(10))
        .await
        .expect("the stalled reader's slot must be freed by the write timeout");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");

    // The stalled connection is gone: draining it ends instead of hanging.
    let partial = read_until_closed(&mut stalled, Duration::from_secs(10))
        .await
        .expect("the stalled reader's connection must be closed");
    assert!(
        partial.len() < BIG_RESPONSE_BYTES,
        "the whole response was delivered, so the write never stalled"
    );
}

/// `Stop` lets an in-flight request finish and returns only once it has.
#[tokio::test]
async fn stop_drains_an_in_flight_request() {
    let server = Server::start(serde_json::json!({})).await;
    let mut stream = server.connect().await;
    stream
        .write_all(b"GET /slow HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let stopping = Instant::now();
    server.stop().await;
    assert!(
        stopping.elapsed() >= Duration::from_millis(300),
        "Stop returned before the in-flight request finished: {:?}",
        stopping.elapsed()
    );
    let response = read_until_closed(&mut stream, Duration::from_secs(1))
        .await
        .expect("the drained connection is closed");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("slow done"), "{response}");
}

/// A request still running when `shutdown_grace_secs` runs out is aborted,
/// and `Stop` returns.
#[tokio::test]
async fn stop_aborts_connections_left_after_the_grace_period() {
    let server = Server::start(serde_json::json!({ "shutdown_grace_secs": 1 })).await;
    let mut stream = server.connect().await;
    stream
        .write_all(b"GET /hang HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let stopping = Instant::now();
    tokio::time::timeout(Duration::from_secs(5), server.stop())
        .await
        .expect("Stop must return once the grace period runs out");
    assert!(
        stopping.elapsed() >= Duration::from_millis(900),
        "Stop returned before the grace period: {:?}",
        stopping.elapsed()
    );
    let response = read_until_closed(&mut stream, Duration::from_secs(1))
        .await
        .expect("the aborted connection is closed");
    assert!(response.is_empty(), "{response}");
}

// ---------------------------------------------------------------------------
// Streamed responses
// ---------------------------------------------------------------------------

/// An SSE response reaches the client as its producer sends it: the head and
/// the first event arrive while the producer is still waiting to send the
/// second, and the body ends with a clean chunked terminator once it
/// completes.
#[tokio::test]
async fn sse_first_event_arrives_before_the_stream_ends() {
    let server = Server::start(serde_json::json!({})).await;
    let mut stream = server.connect().await;
    stream
        .write_all(b"GET /sse HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let head = read_until(&mut stream, "data: one", Duration::from_secs(2)).await;
    let lower = head.to_ascii_lowercase();
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(
        lower.contains("\r\ncontent-type: text/event-stream\r\n"),
        "{head}"
    );
    assert!(
        lower.contains("\r\ntransfer-encoding: chunked\r\n"),
        "{head}"
    );
    assert!(!lower.contains("content-length"), "{head}");
    assert!(!head.contains("data: two"), "{head}");

    server.runtime.release.notify_one();
    let rest = read_until_closed(&mut stream, Duration::from_secs(5))
        .await
        .expect("the stream ends once its producer completes");
    assert!(rest.contains("data: two"), "{rest}");
    assert!(rest.ends_with("\r\n0\r\n\r\n"), "{rest}");
}

/// A client that goes away in the middle of a stream cancels its producer,
/// even one that is idle between events.
#[tokio::test]
async fn client_disconnect_mid_stream_cancels_the_producer() {
    let server = Server::start(serde_json::json!({})).await;
    let mut stream = server.connect().await;
    stream
        .write_all(b"GET /sse-hang HTTP/1.1\r\nHost: t\r\n\r\n")
        .await
        .unwrap();
    read_until(&mut stream, "data: one", Duration::from_secs(2)).await;
    drop(stream);
    producer_cancelled(
        &server,
        Duration::from_secs(2),
        "the client disconnected mid-stream",
    )
    .await;
}

/// A client that stops reading holds the producer back: the producer blocks
/// once the bounded output channel and the socket buffers are full, instead
/// of being drained into memory. Once the client goes away it is cancelled.
#[tokio::test]
async fn a_slow_reader_backpressures_the_producer() {
    let server = Server::start(serde_json::json!({})).await;
    let mut stream = server.connect().await;
    stream
        .write_all(b"GET /flood HTTP/1.1\r\nHost: t\r\n\r\n")
        .await
        .unwrap();
    read_until(&mut stream, "\r\n\r\n", Duration::from_secs(2)).await;

    // Stop reading and let the buffers between the producer and the client
    // fill.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let settled = server.runtime.sent.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let later = server.runtime.sent.load(Ordering::SeqCst);
    assert_eq!(
        settled, later,
        "the producer kept sending to a client that reads nothing"
    );
    assert!(
        later < FLOOD_CHUNKS / 4,
        "{later} of {FLOOD_CHUNKS} chunks were accepted from a producer whose client reads nothing"
    );

    drop(stream);
    producer_cancelled(
        &server,
        Duration::from_secs(5),
        "the client of a stalled stream went away",
    )
    .await;
}

/// A producer error after the head aborts the response: the connection is
/// closed without the chunked terminator, so the client can tell the body is
/// truncated, never a clean end on a kept-alive connection.
#[tokio::test]
async fn an_error_after_the_head_aborts_the_body() {
    let server = Server::start(serde_json::json!({})).await;
    let mut stream = server.connect().await;
    // Keep-alive: a clean end would leave the connection open.
    stream
        .write_all(b"GET /sse-error HTTP/1.1\r\nHost: t\r\n\r\n")
        .await
        .unwrap();
    let head = read_until(&mut stream, "data: one", Duration::from_secs(2)).await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");

    server.runtime.release.notify_one();
    let rest = read_until_closed(&mut stream, Duration::from_secs(5))
        .await
        .expect("the server must close a connection whose streamed body failed");
    let whole = format!("{head}{rest}");
    assert!(!whole.contains("\r\n0\r\n\r\n"), "{whole}");
}

/// A body that runs past its declared length on a chunk boundary is aborted,
/// never sent as a whole response holding the first 5 bytes on a kept-alive
/// connection.
#[tokio::test]
async fn a_body_overrunning_its_declared_length_on_a_chunk_boundary_is_aborted() {
    let server = Server::start(serde_json::json!({})).await;
    let mut stream = server.connect().await;
    // Keep-alive: a clean end would leave the connection open.
    stream
        .write_all(b"GET /length-overrun HTTP/1.1\r\nHost: t\r\n\r\n")
        .await
        .unwrap();
    let response = read_until_closed(&mut stream, Duration::from_secs(5))
        .await
        .expect("the server must close a connection whose body overran its length");
    assert!(!response.contains("abcde"), "{response}");
}

/// A body of exactly its declared length is sent with that `Content-Length`
/// (not chunked) and ends cleanly: the connection serves the next request.
/// (A buffering listener passes this too; it is the control for the overrun
/// test above, proving the length check aborts only a body that is wrong.)
#[tokio::test]
async fn a_body_of_its_declared_length_is_sent_with_its_content_length() {
    let server = Server::start(serde_json::json!({})).await;
    let mut stream = server.connect().await;
    stream
        .write_all(b"GET /length-exact HTTP/1.1\r\nHost: t\r\n\r\n")
        .await
        .unwrap();
    let response = read_until(&mut stream, "\r\n\r\nabcde", Duration::from_secs(2)).await;
    let lower = response.to_ascii_lowercase();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(lower.contains("\r\ncontent-length: 5\r\n"), "{response}");
    assert!(
        lower.contains("\r\ncontent-type: text/plain\r\n"),
        "{response}"
    );
    assert!(!lower.contains("transfer-encoding"), "{response}");
    assert!(response.ends_with("\r\n\r\nabcde"), "{response}");

    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let next = read_until_closed(&mut stream, Duration::from_secs(5))
        .await
        .expect("the kept-alive connection answers the next request");
    assert!(next.starts_with("HTTP/1.1 200"), "{next}");
    assert!(next.ends_with("127.0.0.1"), "{next}");
}

/// A streamed response whose head cannot be sent is a uniform 500 before
/// any of it goes out, and its producer is cancelled: an SSE stream beside an
/// unsendable header (whose producer never ends, so collecting it first would
/// never answer), and a body under a declared length of 0.
#[tokio::test]
async fn a_streamed_response_refused_at_its_head_is_a_500() {
    let server = Server::start(serde_json::json!({})).await;
    for path in ["/sse-bad-header", "/length-zero"] {
        let mut stream = server.connect().await;
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n").as_bytes(),
            )
            .await
            .unwrap();
        let response = read_until_closed(&mut stream, Duration::from_secs(5))
            .await
            .unwrap_or_else(|| panic!("{path} must be answered"));
        let lower = response.to_ascii_lowercase();
        assert!(response.starts_with("HTTP/1.1 500"), "{path}: {response}");
        assert!(
            lower.contains("\r\ncache-control: no-store\r\n"),
            "{path}: {response}"
        );
        assert!(!lower.contains("x-bad"), "{path}: {response}");
        assert!(!response.contains("data: one"), "{path}: {response}");
    }
    producer_cancelled(
        &server,
        Duration::from_secs(2),
        "the SSE stream was refused at its head",
    )
    .await;
}

/// `HEAD` on a streamed route answers the head without a body and cancels
/// the producer; a declared length is the `Content-Length` it reports.
#[tokio::test]
async fn head_on_a_streamed_route_sends_the_head_and_cancels_the_producer() {
    let server = Server::start(serde_json::json!({})).await;
    let mut stream = server.connect().await;
    stream
        .write_all(b"HEAD /sse-hang HTTP/1.1\r\nHost: t\r\n\r\n")
        .await
        .unwrap();
    let head = read_until(&mut stream, "\r\n\r\n", Duration::from_secs(2)).await;
    let lower = head.to_ascii_lowercase();
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(
        lower.contains("\r\ncontent-type: text/event-stream\r\n"),
        "{head}"
    );
    assert!(
        head.ends_with("\r\n\r\n"),
        "a HEAD answer has no body: {head}"
    );
    producer_cancelled(
        &server,
        Duration::from_secs(2),
        "a HEAD answer sends no body",
    )
    .await;

    stream
        .write_all(b"HEAD /length-exact HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let head = read_until_closed(&mut stream, Duration::from_secs(5))
        .await
        .expect("the HEAD answer arrives and the connection closes");
    let lower = head.to_ascii_lowercase();
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(lower.contains("\r\ncontent-length: 5\r\n"), "{head}");
    assert!(
        head.ends_with("\r\n\r\n"),
        "a HEAD answer has no body: {head}"
    );
}

/// A client that stops reading a stream is dropped once its write makes no
/// progress for `write_timeout_secs`, freeing its connection slot and
/// cancelling the producer (a buffering listener frees the slot too, but only
/// after the producer has run to its end).
#[tokio::test]
async fn a_stream_whose_client_stops_reading_is_dropped_after_write_timeout() {
    let server = Server::start(serde_json::json!({
        "write_timeout_secs": 1,
        "max_connections": 1,
    }))
    .await;
    let mut stalled = server.connect().await;
    stalled
        .write_all(b"GET /flood HTTP/1.1\r\nHost: t\r\n\r\n")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut next = server.connect().await;
    next.write_all(b"GET / HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let response = read_until_closed(&mut next, Duration::from_secs(10))
        .await
        .expect("the stalled reader's slot must be freed by the write timeout");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    producer_cancelled(
        &server,
        Duration::from_secs(5),
        "the stalled reader's connection was dropped",
    )
    .await;
    drop(stalled);
}

/// `Stop` gives an open stream the grace period, then cuts it off and
/// cancels its producer.
#[tokio::test]
async fn stop_aborts_an_open_stream_after_the_grace_period() {
    let server = Server::start(serde_json::json!({ "shutdown_grace_secs": 1 })).await;
    let mut stream = server.connect().await;
    stream
        .write_all(b"GET /sse-hang HTTP/1.1\r\nHost: t\r\n\r\n")
        .await
        .unwrap();
    read_until(&mut stream, "data: one", Duration::from_secs(2)).await;

    let stopping = Instant::now();
    tokio::time::timeout(Duration::from_secs(5), server.stop())
        .await
        .expect("Stop must return once the grace period runs out");
    assert!(
        stopping.elapsed() >= Duration::from_millis(900),
        "Stop returned before the grace period: {:?}",
        stopping.elapsed()
    );
    producer_cancelled(&server, Duration::from_secs(2), "the listener stopped").await;
    let rest = read_until_closed(&mut stream, Duration::from_secs(2))
        .await
        .expect("the aborted stream's connection is closed");
    assert!(!rest.contains("\r\n0\r\n\r\n"), "{rest}");
}

/// How a read to the end of a connection ended.
#[derive(Debug, PartialEq, Eq)]
enum ConnectionEnd {
    /// A clean close (FIN): to a close-delimited body, its whole end.
    Closed,
    /// A read error — a reset.
    Reset(std::io::ErrorKind),
}

/// Read until the connection ends, returning what arrived and how it ended,
/// or fail after `within`.
async fn read_to_end_of_connection(
    stream: &mut (impl AsyncRead + Unpin),
    within: Duration,
) -> (String, ConnectionEnd) {
    let mut got = Vec::new();
    let mut buf = vec![0u8; 16 * 1024];
    let end = tokio::time::timeout(within, async {
        loop {
            match stream.read(&mut buf).await {
                Ok(0) => return ConnectionEnd::Closed,
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(e) => return ConnectionEnd::Reset(e.kind()),
            }
        }
    })
    .await
    .expect("the connection ends");
    (String::from_utf8_lossy(&got).into_owned(), end)
}

/// HTTP/1.0 has no chunked encoding, so a streamed body of unknown length is
/// close-delimited: a clean close is its end. A producer error after the
/// head must therefore reset the connection, so the client (or a proxy that
/// speaks HTTP/1.0 upstream, as nginx does by default) sees an error, never
/// a whole response.
#[tokio::test]
async fn an_error_after_the_head_resets_an_http10_connection() {
    let server = Server::start(serde_json::json!({})).await;
    let mut stream = server.connect().await;
    stream
        .write_all(b"GET /sse-error HTTP/1.0\r\nHost: t\r\n\r\n")
        .await
        .unwrap();
    let head = read_until(&mut stream, "data: one", Duration::from_secs(2)).await;
    let lower = head.to_ascii_lowercase();
    assert!(head.starts_with("HTTP/1.0 200"), "{head}");
    assert!(!lower.contains("transfer-encoding"), "{head}");
    assert!(!lower.contains("content-length"), "{head}");

    server.runtime.release.notify_one();
    let (rest, end) = read_to_end_of_connection(&mut stream, Duration::from_secs(5)).await;
    assert_eq!(
        end,
        ConnectionEnd::Reset(std::io::ErrorKind::ConnectionReset),
        "an aborted close-delimited body must end in a reset, not a clean close: {head}{rest}"
    );
}

/// Control for the reset: an HTTP/1.0 stream that completes ends in a clean
/// close after its whole body.
#[tokio::test]
async fn a_complete_http10_stream_ends_in_a_clean_close() {
    let server = Server::start(serde_json::json!({})).await;
    let mut stream = server.connect().await;
    stream
        .write_all(b"GET /sse HTTP/1.0\r\nHost: t\r\n\r\n")
        .await
        .unwrap();
    let head = read_until(&mut stream, "data: one", Duration::from_secs(2)).await;
    assert!(head.starts_with("HTTP/1.0 200"), "{head}");

    server.runtime.release.notify_one();
    let (rest, end) = read_to_end_of_connection(&mut stream, Duration::from_secs(5)).await;
    assert_eq!(end, ConnectionEnd::Closed, "{head}{rest}");
    assert!(rest.ends_with("data: two\n\n"), "{head}{rest}");
}

/// A request-body failure before a streamed response's head replaces the
/// response (here 413) and cancels its producer.
#[tokio::test]
async fn a_body_failure_before_a_streamed_head_is_answered_and_cancels_the_producer() {
    let server = Server::start(serde_json::json!({ "max_body_bytes": 8 })).await;
    let mut stream = server.connect().await;
    stream
        .write_all(
            b"POST /body-then-stream HTTP/1.1\r\nHost: t\r\nTransfer-Encoding: chunked\r\n\r\n\
              5\r\nhello\r\n5\r\nworld\r\n0\r\n\r\n",
        )
        .await
        .unwrap();
    let response = read_until_closed(&mut stream, Duration::from_secs(5))
        .await
        .expect("response arrives");
    assert!(response.starts_with("HTTP/1.1 413"), "{response}");
    assert!(!response.contains("data: one"), "{response}");
    assert_eq!(
        server.collected(1).await,
        vec![Err(wafer_block::ErrorCode::ResourceExhausted)]
    );
    producer_cancelled(
        &server,
        Duration::from_secs(2),
        "the body failed before the head",
    )
    .await;
}

/// A request-body failure after a streamed head cannot change the committed
/// status: the client keeps its 200, the block reads the failure, and the
/// body aborts instead of ending cleanly.
#[tokio::test]
async fn a_body_failure_after_a_streamed_head_leaves_the_status_committed() {
    let server = Server::start(serde_json::json!({ "max_body_bytes": 8 })).await;
    let mut stream = server.connect().await;
    stream
        .write_all(
            b"POST /stream-then-body HTTP/1.1\r\nHost: t\r\nTransfer-Encoding: chunked\r\n\r\n",
        )
        .await
        .unwrap();
    let head = read_until(&mut stream, "data: one", Duration::from_secs(2)).await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");

    stream
        .write_all(b"5\r\nhello\r\n5\r\nworld\r\n0\r\n\r\n")
        .await
        .unwrap();
    assert_eq!(
        server.collected(1).await,
        vec![Err(wafer_block::ErrorCode::ResourceExhausted)]
    );
    let (rest, end) = read_to_end_of_connection(&mut stream, Duration::from_secs(5)).await;
    assert!(!rest.contains("413"), "{head}{rest}");
    assert!(!rest.contains("\r\n0\r\n\r\n"), "{head}{rest}");
    assert_eq!(
        end,
        ConnectionEnd::Reset(std::io::ErrorKind::ConnectionReset),
        "{head}{rest}"
    );
}
