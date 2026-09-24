//! The listener driven over real sockets: `Init` + `bind` start the actual
//! server, and raw HTTP/1.1 bytes exercise it the way a client (or a slow,
//! hostile one) would.

use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use wafer_block::{meta::META_REQ_CLIENT_IP, Block, InputStream, Message, OutputStream};
use wafer_block_macro::wafer_async_trait;

use crate::{
    tests::{init_event, NoopCtx},
    HttpListenerBlock,
};

/// Runtime that answers every dispatch with the client IP the listener put
/// on the message.
struct EchoClientIp;

#[wafer_async_trait]
impl wafer_block::Runtime for EchoClientIp {
    async fn run(&self, _flow_id: &str, msg: Message, input: InputStream) -> OutputStream {
        input.collect_to_bytes().await;
        OutputStream::respond(msg.get_meta(META_REQ_CLIENT_IP).as_bytes().to_vec())
    }

    async fn run_block(&self, _block_name: &str, msg: Message, input: InputStream) -> OutputStream {
        self.run("", msg, input).await
    }
}

/// A running listener on a loopback port. Dropping it drops the block, whose
/// shutdown sender going away stops the server.
struct Server {
    _block: HttpListenerBlock,
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
        let runtime: Arc<dyn wafer_block::Runtime> = Arc::new(EchoClientIp);
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
        Self {
            _block: block,
            addr,
        }
    }

    async fn connect(&self) -> TcpStream {
        TcpStream::connect(self.addr)
            .await
            .expect("connect to the listener")
    }
}

/// Read until the server closes the connection, or fail after `within`.
async fn read_until_closed(stream: &mut TcpStream, within: Duration) -> Option<String> {
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
