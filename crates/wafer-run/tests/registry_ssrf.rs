//! SEC-09 e2e: SSRF filtering on seal-time package downloads.
//!
//! `seal()` downloads a `wafer.lock` entry whose cache is missing from the
//! registry its `source` names. In default builds that fetch refuses
//! private/internal addresses: the composed download URL is pre-checked with
//! `wafer_net_security::is_blocked_url` and DNS results are filtered by
//! `SsrfFilteringResolver` (rebinding). The `allow-private-network` build
//! feature is the compile-time escape hatch for local registries — under it
//! the same wiremock registry serves the package end-to-end.

#![cfg(feature = "wasm")]

mod pinned_registry;

use pinned_registry::{build_with_lock, lock_entry, package, requests, serve};
use serial_test::serial;
use wiremock::MockServer;

const ECHO_WASM: &[u8] = include_bytes!("../testdata/echo_block.wasm");

/// A registry serving `example/echo@1.0.0` (the echo-block fixture, which
/// reports itself as `example/echo`) and the lockfile entry pinning it.
async fn echo_registry(registry: &str, server: &MockServer) -> String {
    let tarball = package("example/echo", "1.0.0", ECHO_WASM);
    serve(server, "example/echo", "1.0.0", tarball.clone()).await;
    lock_entry("example/echo", "1.0.0", &tarball, ECHO_WASM, registry)
}

/// Default build: a registry on a private/loopback address is refused, by
/// IP literal and by `localhost`, and the server is never contacted.
#[cfg(not(feature = "allow-private-network"))]
#[tokio::test]
#[serial]
async fn registry_on_private_address_is_refused() {
    let server = MockServer::start().await;
    let localhost = server.uri().replace("127.0.0.1", "localhost");
    for registry in [server.uri(), localhost] {
        let home = tempfile::tempdir().expect("tempdir");
        let entry = echo_registry(&registry, &server).await;
        let mut wafer = build_with_lock(home.path(), &[entry]).expect("build defers the entry");
        let err = wafer
            .seal()
            .await
            .expect_err("a private registry must be refused in a default build");
        let msg = err.to_string();
        assert!(
            msg.contains("SEC-09") && msg.contains("private/internal"),
            "seal error must cite the SSRF policy: {msg}"
        );
    }
    assert_eq!(
        requests(&server).await,
        0,
        "the private registry must never be contacted"
    );
}

/// A lockfile source that is not an absolute http(s) URL fails `seal()`
/// naming the entry, in every build.
#[tokio::test]
#[serial]
async fn a_registry_source_that_is_not_a_url_fails_seal() {
    let home = tempfile::tempdir().expect("tempdir");
    let tarball = package("example/echo", "1.0.0", ECHO_WASM);
    let entry = lock_entry("example/echo", "1.0.0", &tarball, ECHO_WASM, "not a url");
    let mut wafer = build_with_lock(home.path(), &[entry]).expect("build defers the entry");
    let msg = wafer.seal().await.expect_err("seal must fail").to_string();
    assert!(
        msg.contains("example/echo@1.0.0") && msg.contains("not a URL"),
        "the error names the entry and the cause: {msg}"
    );
}

/// `allow-private-network` build: the same local wiremock registry serves
/// the package end-to-end — seal downloads it, verifies both digests and
/// registers the block under its lockfile name.
#[cfg(feature = "allow-private-network")]
#[tokio::test]
#[serial]
async fn local_registry_works_under_escape_hatch() {
    let server = MockServer::start().await;
    let home = tempfile::tempdir().expect("tempdir");
    let entry = echo_registry(&server.uri(), &server).await;
    let mut wafer = build_with_lock(home.path(), &[entry]).expect("build defers the entry");
    assert!(
        !wafer.has_block("example/echo"),
        "an uncached entry is not registered at build"
    );

    wafer
        .seal()
        .await
        .expect("seal must fetch the block from the local registry");
    assert!(
        wafer.has_block("example/echo"),
        "downloaded block must be registered; got {:?}",
        wafer.block_names()
    );
    assert_eq!(requests(&server).await, 1);
}
