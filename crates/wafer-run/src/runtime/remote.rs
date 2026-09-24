//! Seal-time fetch of lockfile-pinned blocks (`wasm` feature).
//!
//! `wafer.lock` is the only thing that makes [`Wafer::seal`] download a
//! block. The lockfile loader registers every entry whose package is in the
//! local cache; an entry from a `registry+<url>` source whose cache directory
//! is missing is deferred to `seal()`, which downloads the package from that
//! registry the way `wafer install` does
//! (`{registry}/registry/download/{org}/{block}/{version}.wafer`), refuses it
//! unless the tarball hashes to the entry's `sha256` and its `.wasm` to the
//! entry's `wasm_sha256`, and only then compiles it. A block that is not
//! pinned is never fetched: a flow step, route or block config naming an
//! unregistered block is reported as not found.

use std::{io::Read, sync::Arc};

use futures::{StreamExt, TryStreamExt};
use wafer_block::{
    error::RuntimeError,
    lockfile::{
        is_valid_path_segment, sha256_hex, BoundedPackageStream, LockfilePackage,
        MAX_PACKAGE_BYTES, MAX_PACKAGE_ENTRIES, MAX_UNPACKED_BYTES, REGISTRY_SOURCE_PREFIX,
    },
};

use super::Wafer;

/// SEC-09: URL-level SSRF pre-check applied to every package download.
/// Catches non-http(s) schemes, `localhost`, and private/link-local IP
/// literals; hostnames that *resolve* to private IPs are caught by the
/// [`SsrfFilteringResolver`](wafer_net_security::SsrfFilteringResolver)
/// installed on the registry client (DNS rebinding, SEC-019).
#[cfg(not(feature = "allow-private-network"))]
fn ensure_url_allowed(url: &str, name: &str) -> Result<(), RuntimeError> {
    if wafer_net_security::is_blocked_url(url) {
        return Err(RuntimeError::Registry(format!(
            "refusing to fetch the package for {name}: {url} targets a private/internal address \
             (SEC-09; build with the `allow-private-network` feature for local registries)"
        )));
    }
    Ok(())
}

/// SSRF escape hatch: the `allow-private-network` build permits registries
/// on private addresses (local development / integration tests only — see
/// the feature docs in Cargo.toml).
#[cfg(feature = "allow-private-network")]
fn ensure_url_allowed(_url: &str, _name: &str) -> Result<(), RuntimeError> {
    Ok(())
}

/// PERF-04: bounded fan-out for package downloads during `seal()`. Small
/// enough to stay polite to the registry origin, large enough to overlap
/// network latency across independent packages.
const REMOTE_FETCH_CONCURRENCY: usize = 8;

/// Read a response body into memory, refusing more than `max` bytes. An
/// advertised `Content-Length` over `max` is refused before any body is
/// read; the body is then read chunk by chunk and refused as soon as it
/// passes `max`, so a missing or lying length cannot make it unbounded.
async fn read_body_capped(
    mut resp: reqwest::Response,
    max: usize,
    what: &str,
) -> Result<Vec<u8>, RuntimeError> {
    if let Some(len) = resp.content_length() {
        if len > max as u64 {
            return Err(RuntimeError::Registry(format!(
                "{what} exceeds the {max}-byte limit (Content-Length {len})"
            )));
        }
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| RuntimeError::Registry(format!("reading {what}: {e}")))?
    {
        if body.len() + chunk.len() > max {
            return Err(RuntimeError::Registry(format!(
                "{what} exceeds the {max}-byte limit"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// The URL `wafer install` downloads `pkg`'s tarball from: its
/// `registry+<url>` source joined with
/// `/registry/download/{org}/{block}/{version}.wafer`, each coordinate one
/// percent-encoded path segment.
fn package_download_url(pkg: &LockfilePackage) -> Result<String, RuntimeError> {
    let invalid =
        |reason: String| RuntimeError::Lockfile(format!("{}@{}: {reason}", pkg.name, pkg.version));
    let base = pkg
        .source
        .strip_prefix(REGISTRY_SOURCE_PREFIX)
        .ok_or_else(|| invalid(format!("source {:?} is not a registry", pkg.source)))?;
    let (org, block) = pkg
        .name
        .split_once('/')
        .ok_or_else(|| invalid("name is not {org}/{block}".to_string()))?;
    for segment in [org, block, pkg.version.as_str()] {
        if !is_valid_path_segment(segment) {
            return Err(invalid(format!("{segment:?} is not a valid path segment")));
        }
    }
    let mut url = url::Url::parse(base.trim_end_matches('/'))
        .map_err(|e| invalid(format!("registry {base:?} is not a URL: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid(format!("registry {base:?} is not an http(s) URL")));
    }
    url.path_segments_mut()
        .map_err(|()| invalid(format!("registry {base:?} cannot take a path")))?
        .pop_if_empty()
        .extend([
            "registry",
            "download",
            org,
            block,
            &format!("{}.wafer", pkg.version),
        ]);
    Ok(url.into())
}

/// Unpack the `.wasm` artifact from a package tarball in memory, under the
/// same bounds `wafer install` extracts with: a decompressed stream of at
/// most `MAX_DECOMPRESSED_BYTES` (which bounds header records and skipped
/// bodies too), at most [`MAX_PACKAGE_ENTRIES`] entries and
/// [`MAX_UNPACKED_BYTES`] of file content, regular files and directories
/// only. The package's top level must hold exactly one `.wasm` and exactly
/// one `wafer.toml` naming `pkg` — what the lockfile loader requires of a
/// cached package.
fn unpack_wasm(tarball: &[u8], pkg: &LockfilePackage) -> Result<Vec<u8>, RuntimeError> {
    let bad = |reason: String| {
        RuntimeError::Registry(format!("package {}@{}: {reason}", pkg.name, pkg.version))
    };
    let mut archive = tar::Archive::new(BoundedPackageStream::new(flate2::read::GzDecoder::new(
        tarball,
    )));
    let entries = archive
        .entries()
        .map_err(|e| bad(format!("reading the tarball: {e}")))?;
    let mut unpacked: u64 = 0;
    let mut wasm: Option<Vec<u8>> = None;
    let mut manifest: Option<Vec<u8>> = None;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_PACKAGE_ENTRIES {
            return Err(bad(format!(
                "more than {MAX_PACKAGE_ENTRIES} tarball entries"
            )));
        }
        let mut entry = entry.map_err(|e| bad(format!("reading a tarball entry: {e}")))?;
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|e| bad(format!("reading an entry path: {e}")))?
            .into_owned();
        if !entry_type.is_file() {
            return Err(bad(format!(
                "entry {} is not a regular file or directory",
                path.display()
            )));
        }
        let budget = MAX_UNPACKED_BYTES - unpacked;
        let mut body = Vec::new();
        (&mut entry)
            .take(budget + 1)
            .read_to_end(&mut body)
            .map_err(|e| bad(format!("reading {}: {e}", path.display())))?;
        if body.len() as u64 > budget {
            return Err(bad(format!(
                "unpacks to more than {MAX_UNPACKED_BYTES} bytes"
            )));
        }
        unpacked += body.len() as u64;

        let mut parts = path
            .components()
            .filter(|c| !matches!(c, std::path::Component::CurDir));
        let top_level = match (parts.next(), parts.next()) {
            (Some(std::path::Component::Normal(file)), None) => file.to_str(),
            _ => None,
        };
        match top_level {
            Some("wafer.toml") if manifest.is_some() => {
                return Err(bad("holds more than one wafer.toml".to_string()));
            }
            Some("wafer.toml") => manifest = Some(body),
            Some(file) if file.ends_with(".wasm") && wasm.is_some() => {
                return Err(bad("holds more than one .wasm artifact".to_string()));
            }
            Some(file) if file.ends_with(".wasm") => wasm = Some(body),
            _ => {}
        }
    }

    let manifest = manifest.ok_or_else(|| bad("has no wafer.toml".to_string()))?;
    let manifest =
        std::str::from_utf8(&manifest).map_err(|e| bad(format!("wafer.toml is not UTF-8: {e}")))?;
    let packaged = crate::registry_loader::packaged_name(manifest)
        .map_err(|e| bad(format!("parse wafer.toml: {e}")))?;
    if packaged != pkg.name {
        return Err(bad(format!("wafer.toml names {packaged:?}")));
    }
    wasm.ok_or_else(|| bad("has no .wasm artifact".to_string()))
}

/// Download `pkg`'s tarball and return its `.wasm` once both digests the
/// lockfile pins have matched: the tarball's against `sha256`, then the
/// unpacked artifact's against `wasm_sha256`. Pure network plus hashing —
/// no runtime state — so packages can be fetched concurrently.
async fn fetch_pinned_wasm(
    client: &reqwest::Client,
    pkg: &LockfilePackage,
) -> Result<Vec<u8>, RuntimeError> {
    let name = format!("{}@{}", pkg.name, pkg.version);
    let url = package_download_url(pkg)?;
    ensure_url_allowed(&url, &name)?;
    let resp = client
        .get(&url)
        .header(
            "User-Agent",
            concat!("wafer-run/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
        .map_err(|e| RuntimeError::Registry(format!("downloading {name} from {url}: {e}")))?;
    let status = resp.status().as_u16();
    if status != 200 {
        return Err(RuntimeError::Registry(format!(
            "downloading {name} from {url}: HTTP {status}"
        )));
    }
    let tarball = read_body_capped(resp, MAX_PACKAGE_BYTES, &format!("package {name}")).await?;

    let actual = sha256_hex(&tarball);
    if actual != pkg.sha256 {
        return Err(RuntimeError::Registry(format!(
            "{name}: integrity check failed — wafer.lock pins sha256 {}, the registry served \
             a tarball hashing to {actual}",
            pkg.sha256
        )));
    }
    let wasm = unpack_wasm(&tarball, pkg)?;
    let actual = sha256_hex(&wasm);
    if actual != pkg.wasm_sha256 {
        return Err(RuntimeError::Registry(format!(
            "{name}: integrity check failed — wafer.lock pins wasm_sha256 {}, the package's \
             .wasm hashes to {actual}",
            pkg.wasm_sha256
        )));
    }
    Ok(wasm)
}

impl Wafer {
    /// Build the short-lived HTTP client used for package downloads during
    /// one `seal()` pass.
    ///
    /// SSRF note: like the `wafer-run/network` client, the `SsrfFilteringResolver`
    /// DNS-rebind layer only runs on direct connections. With an
    /// `HTTP(S)_PROXY` / `ALL_PROXY` env var set, reqwest hands the hostname to
    /// the proxy and the resolver is bypassed (the `ensure_url_allowed`
    /// literal-URL gate still applies per fetch); a proxied deploy must enforce
    /// SSRF at the egress proxy.
    fn registry_http_client() -> Result<reqwest::Client, RuntimeError> {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            // SEC-09: do not follow redirects. The download URL is composed
            // from the lockfile's source; a redirect could bounce it to an
            // unintended (e.g. internal) destination. A registry that needs
            // a redirect must be pinned at its final URL.
            .redirect(reqwest::redirect::Policy::none())
            // SEC-09: drop DNS results pointing at private/loopback/link-local
            // IPs (DNS rebinding). URL-level checks happen per fetch in
            // `ensure_url_allowed`; this is the resolved-IP layer. Passthrough
            // under the `allow-private-network` build feature.
            .dns_resolver(Arc::new(wafer_net_security::SsrfFilteringResolver))
            .build()
            .map_err(|e| RuntimeError::Registry(format!("failed to create HTTP client: {e}")))
    }

    /// Download, verify and register the lockfile entries the lockfile
    /// loader deferred because their cache directory is missing.
    ///
    /// Each identity is checked before anything is fetched: one the
    /// embedder registered, or an operator alias, is refused rather than
    /// shadowed. Downloads run with bounded concurrency over immutable data
    /// (PERF-04); a failed download or a digest mismatch fails `seal()` with
    /// the cause. Registration is sequential and in lockfile order, through
    /// the same path as a cached entry.
    pub(crate) async fn fetch_deferred_lockfile_blocks(&mut self) -> Result<(), RuntimeError> {
        let deferred: Vec<LockfilePackage> = self
            .locked_blocks
            .iter()
            .filter(|locked| locked.deferred)
            .map(|locked| locked.package.clone())
            .collect();
        if deferred.is_empty() {
            return Ok(());
        }
        for pkg in &deferred {
            self.registration.check_downloadable(&pkg.name)?;
        }

        let client = Self::registry_http_client()?;
        let fetched: Vec<(LockfilePackage, Vec<u8>)> =
            futures::stream::iter(deferred.into_iter().map(|pkg| {
                let client = &client;
                async move {
                    let wasm = fetch_pinned_wasm(client, &pkg).await?;
                    Ok::<_, RuntimeError>((pkg, wasm))
                }
            }))
            .buffered(REMOTE_FETCH_CONCURRENCY)
            .try_collect()
            .await?;

        for (pkg, wasm) in fetched {
            self.register_locked_wasm(&pkg, &wasm)?;
            tracing::info!(
                block = %pkg.name,
                version = %pkg.version,
                "downloaded lockfile-pinned block from its registry"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(name: &str, version: &str, source: &str) -> LockfilePackage {
        LockfilePackage {
            name: name.into(),
            version: version.into(),
            sha256: String::new(),
            wasm_sha256: String::new(),
            source: source.into(),
            capabilities: None,
        }
    }

    /// The runtime downloads from the URL `wafer install` does, so one
    /// registry serves both.
    #[test]
    fn download_url_is_the_install_url() {
        for source in ["registry+https://wafer.run", "registry+https://wafer.run/"] {
            assert_eq!(
                package_download_url(&pkg("acme/widget", "1.2.0", source)).expect("url"),
                "https://wafer.run/registry/download/acme/widget/1.2.0.wafer"
            );
        }
        assert_eq!(
            package_download_url(&pkg("acme/widget", "1.2.0", "registry+https://x.test/base"))
                .expect("url"),
            "https://x.test/base/registry/download/acme/widget/1.2.0.wafer"
        );
    }

    #[test]
    fn download_url_refuses_what_is_not_a_registry_coordinate() {
        for (name, version, source) in [
            ("acme/widget", "1.2.0", "path+/srv/blocks"),
            ("acme/widget", "1.2.0", "registry+ftp://wafer.run"),
            ("acme/widget", "1.2.0", "registry+not a url"),
            ("acme", "1.2.0", "registry+https://wafer.run"),
            ("acme/..", "1.2.0", "registry+https://wafer.run"),
            ("acme/widget", "../../x", "registry+https://wafer.run"),
        ] {
            assert!(
                package_download_url(&pkg(name, version, source)).is_err(),
                "{name}@{version} from {source} must be refused"
            );
        }
    }

    const WAFER_TOML: &[u8] =
        b"[package]\norg = \"acme\"\nname = \"widget\"\nversion = \"1.2.0\"\nabi = 1\n";

    /// A gzipped tarball of `(path, type, body)` entries.
    fn tarball(entries: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        {
            let mut tb = tar::Builder::new(&mut gz);
            for (path, entry_type, body) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_path(path).unwrap();
                header.set_entry_type(*entry_type);
                header.set_size(body.len() as u64);
                if entry_type.is_symlink() {
                    header.set_link_name("../../escape").unwrap();
                }
                header.set_cksum();
                tb.append(&header, *body).unwrap();
            }
            tb.finish().unwrap();
        }
        gz.finish().unwrap()
    }

    fn widget() -> LockfilePackage {
        pkg("acme/widget", "1.2.0", "registry+https://wafer.run")
    }

    #[test]
    fn unpack_returns_the_packaged_wasm() {
        let bytes = tarball(&[
            ("./wafer.toml", tar::EntryType::Regular, WAFER_TOML),
            ("widget.wasm", tar::EntryType::Regular, b"\0asm"),
            ("docs/README.md", tar::EntryType::Regular, b"hi"),
        ]);
        assert_eq!(unpack_wasm(&bytes, &widget()).expect("unpacks"), b"\0asm");
    }

    /// What the cache loader would refuse is refused before compiling: a
    /// link entry, a second `.wasm`, no `.wasm`, a second `wafer.toml`
    /// (which would decide the name check by entry order), a `wafer.toml` naming
    /// another block, and more entries than the bound.
    #[test]
    fn unpack_refuses_what_the_cache_loader_would() {
        let regular = tar::EntryType::Regular;
        let many: Vec<(String, tar::EntryType, &[u8])> = (0..=MAX_PACKAGE_ENTRIES)
            .map(|i| (format!("f{i}"), regular, &b""[..]))
            .collect();
        let many: Vec<(&str, tar::EntryType, &[u8])> =
            many.iter().map(|(p, t, b)| (p.as_str(), *t, *b)).collect();
        for (case, bytes, expected) in [
            (
                "symlink",
                tarball(&[
                    ("wafer.toml", regular, WAFER_TOML),
                    ("widget.wasm", tar::EntryType::Symlink, b""),
                ]),
                "not a regular file",
            ),
            (
                "two wasm",
                tarball(&[
                    ("wafer.toml", regular, WAFER_TOML),
                    ("a.wasm", regular, b"\0asm"),
                    ("b.wasm", regular, b"\0asm"),
                ]),
                "more than one .wasm",
            ),
            (
                "no wasm",
                tarball(&[("wafer.toml", regular, WAFER_TOML)]),
                "no .wasm",
            ),
            (
                "two wafer.toml",
                tarball(&[
                    ("wafer.toml", regular, WAFER_TOML),
                    ("./wafer.toml", regular, WAFER_TOML),
                    ("widget.wasm", regular, b"\0asm"),
                ]),
                "more than one wafer.toml",
            ),
            (
                "foreign name",
                tarball(&[
                    (
                        "wafer.toml",
                        regular,
                        b"[package]\norg = \"a\"\nname = \"victim\"\n",
                    ),
                    ("widget.wasm", regular, b"\0asm"),
                ]),
                "wafer.toml names \"a/victim\"",
            ),
            ("too many entries", tarball(&many), "tarball entries"),
        ] {
            let err = unpack_wasm(&bytes, &widget()).expect_err(case).to_string();
            assert!(err.contains(expected), "{case}: {err}");
        }
    }

    /// What a tar reader consumes without surfacing it as file content is
    /// bounded by the decompressed stream: a GNU long-name record, which the
    /// reader buffers whole, and a directory entry declaring a huge body,
    /// which it reads through to skip — each a small gzip of zeros followed
    /// by an otherwise valid package.
    #[test]
    fn unpack_bounds_header_records_and_skipped_bodies() {
        use wafer_block::lockfile::MAX_DECOMPRESSED_BYTES;

        fn build(entry_type: tar::EntryType, path: &str) -> Vec<u8> {
            let size = MAX_DECOMPRESSED_BYTES + 1024 * 1024;
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            {
                let mut tb = tar::Builder::new(&mut gz);
                let mut h = tar::Header::new_gnu();
                h.set_path(path).unwrap();
                h.set_entry_type(entry_type);
                h.set_size(size);
                h.set_cksum();
                tb.append(&h, std::io::repeat(0).take(size)).unwrap();
                for (file, body) in [("wafer.toml", WAFER_TOML), ("widget.wasm", b"\0asm")] {
                    let mut f = tar::Header::new_gnu();
                    f.set_path(file).unwrap();
                    f.set_size(body.len() as u64);
                    f.set_cksum();
                    tb.append(&f, body).unwrap();
                }
                tb.finish().unwrap();
            }
            gz.finish().unwrap()
        }
        for (case, bytes) in [
            (
                "long name",
                build(tar::EntryType::GNULongName, "././@LongLink"),
            ),
            ("huge directory", build(tar::EntryType::Directory, "dir/")),
        ] {
            assert!(
                bytes.len() < MAX_PACKAGE_BYTES,
                "{case}: the bomb downloads"
            );
            let err = unpack_wasm(&bytes, &widget()).expect_err(case).to_string();
            assert!(err.contains("decompresses to more than"), "{case}: {err}");
        }
    }

    /// A hostile registry host must be stopped before any connection (SEC-09).
    #[cfg(not(feature = "allow-private-network"))]
    #[test]
    fn ensure_url_allowed_blocks_internal_targets() {
        for url in [
            "http://169.254.169.254/latest/meta-data/", // cloud metadata
            "http://127.0.0.1:8080/registry/download/a/b/1.0.0.wafer",
            "http://localhost/registry/download/a/b/1.0.0.wafer",
            "http://10.0.0.7/registry/download/a/b/1.0.0.wafer",
            "file:///etc/passwd",
        ] {
            let err = ensure_url_allowed(url, "acme/widget@1.0.0").expect_err("must be refused");
            let msg = err.to_string();
            assert!(
                msg.contains("SEC-09") && msg.contains(url),
                "error must cite the policy and echo the URL: {msg}"
            );
        }
    }

    #[cfg(not(feature = "allow-private-network"))]
    #[test]
    fn ensure_url_allowed_passes_public_targets() {
        ensure_url_allowed(
            "https://wafer.run/registry/download/acme/widget/1.0.0.wafer",
            "acme/widget@1.0.0",
        )
        .expect("public URL must pass");
    }
}
