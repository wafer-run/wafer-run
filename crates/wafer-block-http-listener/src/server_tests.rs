//! The listener driven over real sockets: `Init` + `bind` start the actual
//! server, and raw HTTP/1.1 bytes exercise it the way a client (or a slow,
//! hostile one) would.

use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use wafer_block::{
    http_codec::META_HTTP_PATH, meta::META_REQ_CLIENT_IP, Block, InputStream, LifecycleEvent,
    LifecycleType, Message, MetaEntry, OutputStream,
};
use wafer_block_macro::wafer_async_trait;

use crate::{
    tests::{init_event, NoopCtx},
    HttpListenerBlock,
};

/// A response body far larger than loopback socket buffers, so a client
/// that does not read it leaves the server's write blocked.
const BIG_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

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
/// `X-Forwarded-For` header as the message carries it; anything else answers
/// with the client IP the listener put on the message. Every path except
/// `/collect` and `/first-chunk` reads the whole body first and ignores how
/// it ended.
#[derive(Default)]
struct TestRuntime {
    collected: parking_lot::Mutex<Vec<Collected>>,
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
            "/hang" => {
                tokio::time::sleep(Duration::from_secs(60)).await;
                OutputStream::respond(b"hang done".to_vec())
            }
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
/// with none of the response's headers, never the page without its CSP.
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
    assert!(!lower.contains("x-good"), "{response}");
    assert!(!lower.contains("content-security-policy"), "{response}");
    assert!(
        response.ends_with(r#"{"error":"Internal","message":"internal server error"}"#),
        "{response}"
    );
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
