//! `HttpNetworkService` timeouts against a live server that trickles its body.
//!
//! A streaming download is bounded by how long the server goes quiet
//! (`read_timeout`), not by a total; the buffered path keeps its total
//! (`request_timeout`). Runs under `allow-private-network` so the loopback
//! server is dialable (see `redirect_ssrf.rs`).
#![cfg(feature = "allow-private-network")]

use std::{collections::HashMap, time::Duration};

use futures::StreamExt;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use wafer_block::StreamEvent;
use wafer_block_network::service::{
    HttpNetworkLimits, HttpNetworkService, NetworkService, Request, DEFAULT_MAX_RESPONSE_BYTES,
};

/// Serve one HTTP/1.1 response per connection: the head at once, then
/// `chunks` one-byte chunks `gap` apart, then the terminating chunk.
async fn trickle_server(chunks: usize, gap: Duration) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let head = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
                if sock.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                for _ in 0..chunks {
                    tokio::time::sleep(gap).await;
                    if sock.write_all(b"1\r\nx\r\n").await.is_err() {
                        return;
                    }
                }
                let _ = sock.write_all(b"0\r\n\r\n").await;
            });
        }
    });
    format!("http://{addr}/")
}

fn get(url: &str) -> Request {
    Request {
        method: "GET".into(),
        url: url.into(),
        headers: HashMap::new(),
        body: None,
    }
}

/// Total 1 s, idle 1 s; the body takes ~2 s at one chunk per 250 ms.
fn limits() -> HttpNetworkLimits {
    HttpNetworkLimits {
        max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        connect_timeout: Duration::from_secs(1),
        read_timeout: Duration::from_secs(1),
        request_timeout: Duration::from_secs(1),
    }
}

/// A streaming body that keeps arriving outlives `request_timeout` and
/// completes: only `read_timeout` (never reached here) bounds a stream.
#[tokio::test]
async fn streaming_body_that_keeps_arriving_outlives_the_total_timeout() {
    let url = trickle_server(8, Duration::from_millis(250)).await;
    let svc = HttpNetworkService::new(limits());
    let (head, body) = svc
        .do_request_streaming(&get(&url))
        .await
        .expect("response head");
    assert_eq!(head.status_code, 200);
    let events: Vec<StreamEvent> = body.collect().await;
    let bytes: usize = events
        .iter()
        .map(|e| match e {
            StreamEvent::Chunk(c) => c.len(),
            _ => 0,
        })
        .sum();
    assert!(
        matches!(events.last(), Some(StreamEvent::Complete { .. })),
        "a progressing stream must complete, got: {events:?}"
    );
    assert_eq!(bytes, 8);
}

/// A stream whose server goes quiet for longer than `read_timeout` ends with
/// an `Error` terminal, not a hang and not a clean `Complete`.
#[tokio::test]
async fn streaming_body_that_stalls_fails_on_the_idle_timeout() {
    let url = trickle_server(2, Duration::from_secs(3)).await;
    let svc = HttpNetworkService::new(limits());
    let started = std::time::Instant::now();
    let (_, body) = svc
        .do_request_streaming(&get(&url))
        .await
        .expect("response head");
    let events: Vec<StreamEvent> = body.collect().await;
    assert!(
        matches!(events.last(), Some(StreamEvent::Error(_))),
        "a stalled stream must end in Error, got: {events:?}"
    );
    assert!(
        started.elapsed() < Duration::from_millis(2500),
        "the idle timeout must fire before the server's 3 s gap ends"
    );
}

/// The buffered path keeps its total cap: the same trickling body that
/// streams to completion above fails `do_request` after `request_timeout`.
#[tokio::test]
async fn buffered_request_is_bounded_by_the_total_timeout() {
    let url = trickle_server(8, Duration::from_millis(250)).await;
    let svc = HttpNetworkService::new(limits());
    let started = std::time::Instant::now();
    svc.do_request(&get(&url))
        .await
        .expect_err("a 2 s body must exceed the 1 s total timeout");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(900) && elapsed < Duration::from_millis(1800),
        "the failure must be the 1 s total timeout, took {elapsed:?}"
    );
}
