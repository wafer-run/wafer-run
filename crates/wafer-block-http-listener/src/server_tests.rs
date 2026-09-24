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
    LifecycleType, Message, OutputStream,
};
use wafer_block_macro::wafer_async_trait;

use crate::{
    tests::{init_event, NoopCtx},
    HttpListenerBlock,
};

/// A response body far larger than loopback socket buffers, so a client
/// that does not read it leaves the server's write blocked.
const BIG_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// Runtime behind the test listener. By path: `/big` answers with
/// [`BIG_RESPONSE_BYTES`], `/slow` answers after 500 ms, `/hang` after a
/// minute; anything else answers with the client IP the listener put on the
/// message.
struct TestRuntime;

#[wafer_async_trait]
impl wafer_block::Runtime for TestRuntime {
    async fn run(&self, _flow_id: &str, msg: Message, input: InputStream) -> OutputStream {
        input.collect_to_bytes().await;
        match msg.get_meta(META_HTTP_PATH) {
            "/big" => OutputStream::respond(vec![b'x'; BIG_RESPONSE_BYTES]),
            "/slow" => {
                tokio::time::sleep(Duration::from_millis(500)).await;
                OutputStream::respond(b"slow done".to_vec())
            }
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
        let runtime: Arc<dyn wafer_block::Runtime> = Arc::new(TestRuntime);
        block.bind(Box::new(runtime));

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
        Self { block, addr }
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
