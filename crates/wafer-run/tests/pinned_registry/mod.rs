//! Shared fixtures for the seal-time lockfile-pinned download e2es
//! (`registry_ssrf.rs`, `remote_integrity.rs`): package tarballs in the
//! layout `wafer install` extracts, `wafer.lock` entries pinning them, a
//! wiremock registry serving them at the install URL, and a runtime built
//! from the lockfile against an empty cache.
//!
//! The cache root is `$HOME/.wafer/cache`, read once while the runtime is
//! built; every test using [`build_with_lock`] is `#[serial]` because it
//! points `HOME` at a temp dir for that call.

#![allow(dead_code, reason = "each test binary uses a different subset")]

use std::path::Path;

use flate2::{write::GzEncoder, Compression};
use wafer_block::{lockfile::sha256_hex, BlockInfo};
use wafer_run::{RuntimeError, Wafer};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

/// A WASM module whose `__wafer_info` reports `info`.
pub fn guest(info: &BlockInfo) -> Vec<u8> {
    let json = serde_json::to_string(info).expect("BlockInfo serializes");
    let packed = (64u64 << 32) | json.len() as u64;
    let escaped = json.replace('\\', "\\\\").replace('"', "\\\"");
    wat::parse_str(format!(
        r#"(module
            (memory (export "memory") 1)
            (data (i32.const 64) "{escaped}")
            (func (export "__wafer_info") (result i64) (i64.const {packed})))"#
    ))
    .expect("WAT parses")
}

/// The `wafer.toml` a package for `name` (`{org}/{block}`) carries.
pub fn wafer_toml(name: &str, version: &str) -> Vec<u8> {
    let (org, block) = name.split_once('/').expect("{org}/{block}");
    format!("[package]\norg = \"{org}\"\nname = \"{block}\"\nversion = \"{version}\"\nabi = 1\n")
        .into_bytes()
}

/// A gzipped tarball holding `files` (path, bytes) as regular files.
pub fn tarball(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut gz = GzEncoder::new(Vec::new(), Compression::fast());
    {
        let mut tb = tar::Builder::new(&mut gz);
        for (file, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(file).expect("tar path");
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tb.append(&header, *bytes).expect("tar append");
        }
        tb.finish().expect("tar finish");
    }
    gz.finish().expect("gzip finish")
}

/// The package `wafer install` would download for `name@version`: its
/// `wafer.toml` and `block.wasm`.
pub fn package(name: &str, version: &str, wasm: &[u8]) -> Vec<u8> {
    tarball(&[
        ("wafer.toml", &wafer_toml(name, version)),
        ("block.wasm", wasm),
    ])
}

/// A `[[package]]` entry pinning `tarball` and `wasm` for `name@version`,
/// fetched from `registry`.
pub fn lock_entry(
    name: &str,
    version: &str,
    tarball: &[u8],
    wasm: &[u8],
    registry: &str,
) -> String {
    format!(
        "[[package]]\nname = \"{name}\"\nversion = \"{version}\"\nsha256 = \"{}\"\n\
         wasm_sha256 = \"{}\"\nsource = \"registry+{registry}\"\n",
        sha256_hex(tarball),
        sha256_hex(wasm)
    )
}

/// Serve `bytes` at the install URL of `name@version` on `server`.
pub async fn serve(server: &MockServer, name: &str, version: &str, bytes: Vec<u8>) {
    Mock::given(method("GET"))
        .and(path(format!("/registry/download/{name}/{version}.wafer")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
        .mount(server)
        .await;
}

/// Build a runtime (no inventory blocks) from a `wafer.lock` holding
/// `entries`, with `$HOME` — and so the package cache — at `home`.
pub fn build_with_lock(home: &Path, entries: &[String]) -> Result<Wafer, RuntimeError> {
    let lock = home.join("wafer.lock");
    std::fs::write(&lock, format!("version = 2\n\n{}", entries.join("\n")))
        .expect("write wafer.lock");
    let prior = std::env::var_os("HOME");
    std::env::set_var("HOME", home);
    let built = Wafer::builder().disable_inventory().lockfile(&lock).build();
    match prior {
        Some(h) => std::env::set_var("HOME", h),
        None => std::env::remove_var("HOME"),
    }
    built
}

/// Seed the package cache under `home` with `wasm` for `name@version`, as
/// `wafer install` leaves it.
pub fn seed_cache(home: &Path, name: &str, version: &str, wasm: &[u8]) {
    let dir = home.join(".wafer/cache").join(name).join(version);
    std::fs::create_dir_all(&dir).expect("create cache dir");
    std::fs::write(dir.join("wafer.toml"), wafer_toml(name, version)).expect("write wafer.toml");
    std::fs::write(dir.join("block.wasm"), wasm).expect("write wasm");
}

/// How many requests `server` has received.
pub async fn requests(server: &MockServer) -> usize {
    server.received_requests().await.expect("recorded").len()
}
