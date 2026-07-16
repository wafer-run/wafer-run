//! `wafer install --cache-only` orchestration.
//!
//! Flow:
//! 1. Resolve version — with `@ver` → `get_version`; without → `get_package`,
//!    filter yanked, pick highest semver.
//! 2. Emit a yanked-version warning if the caller asked for an explicit
//!    yanked version (still proceeds; reproducibility over warning).
//! 3. Cache-hit check: if the version dir exists AND the lockfile already
//!    has a matching entry (same sha256), skip the download entirely.
//! 4. Acquire the cache flock; re-check the cache under the lock (another
//!    process may have populated it while we waited); download, hash,
//!    verify, extract into a sibling temp dir, then `rename` into place.
//!    Release the lock.
//! 5. Update the lockfile with the new entry and write atomically.

use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use semver::Version;
use tar::{Archive, EntryType};

use crate::{
    block_name::parse_org_block,
    cache::CacheRoot,
    lockfile::{Lockfile, LockfilePackage, LockfileToml},
    registry_client::{self, Registry, VersionDetail, VersionSummary},
};

/// Result of a cache-only install. `from_cache=true` means no network was touched.
#[derive(Debug)]
pub struct InstallOutcome {
    pub org: String,
    pub block: String,
    pub version: String,
    #[expect(dead_code, reason = "captured for future verification logging")]
    pub sha256: String,
    pub from_cache: bool,
}

impl InstallOutcome {
    /// Outcome for a cache hit — no network was touched.
    fn cached(
        org: &str,
        block: &str,
        version: impl Into<String>,
        sha256: impl Into<String>,
    ) -> Self {
        Self {
            org: org.into(),
            block: block.into(),
            version: version.into(),
            sha256: sha256.into(),
            from_cache: true,
        }
    }

    /// Outcome for a fresh download + extract.
    fn fresh(
        org: &str,
        block: &str,
        version: impl Into<String>,
        sha256: impl Into<String>,
    ) -> Self {
        Self {
            org: org.into(),
            block: block.into(),
            version: version.into(),
            sha256: sha256.into(),
            from_cache: false,
        }
    }
}

/// The download critical section shared by the frozen and non-frozen
/// install paths: verify the tarball's sha256 against `expected_sha`,
/// extract into a sibling `.extract-{uuid}` temp dir, then atomically
/// promote it to `final_dir` via `fs::rename`. The caller must hold the
/// cache flock. `integrity_msg` renders the path-specific error for a sha
/// mismatch, receiving the actual sha.
fn verify_and_promote(
    bytes: &[u8],
    expected_sha: &str,
    final_dir: &Path,
    integrity_msg: impl FnOnce(&str) -> String,
) -> Result<()> {
    let actual = sha256_hex(bytes);
    if actual != expected_sha {
        bail!("{}", integrity_msg(&actual));
    }

    let parent = final_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("cache package path has no parent"))?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let tmp_dir = parent.join(format!(".extract-{}", uuid::Uuid::new_v4()));
    if let Err(e) = extract_tarball(bytes, &tmp_dir) {
        // Mid-stream failure (disk full, bad tarball, ...) may have left
        // a partially populated tmp dir — remove it before bailing.
        let _ = fs::remove_dir_all(&tmp_dir);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp_dir, final_dir) {
        // Clean up on failure; one common cause is a race where another
        // process completed the same install first.
        let _ = fs::remove_dir_all(&tmp_dir);
        return Err(e).with_context(|| {
            format!(
                "promote {} -> {} (concurrent install may have completed)",
                tmp_dir.display(),
                final_dir.display(),
            )
        });
    }
    Ok(())
}

/// Pick the highest non-yanked version from a list, or return `None` if
/// every version is yanked / fails to parse as semver.
pub(crate) fn pick_latest_non_yanked(versions: &[VersionSummary]) -> Option<&VersionSummary> {
    versions
        .iter()
        .filter(|v| v.yanked == 0)
        .filter_map(|v| Version::parse(&v.version).ok().map(|sv| (sv, v)))
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, v)| v)
}

/// Build a `LockfilePackage` entry from server metadata + a resolved registry URL.
pub(crate) fn lockfile_entry(
    registry: &Registry,
    org: &str,
    block: &str,
    version: &str,
    sha256: &str,
    wasm_sha256: &str,
) -> LockfilePackage {
    LockfilePackage {
        name: format!("{org}/{block}"),
        version: version.into(),
        sha256: sha256.into(),
        wasm_sha256: wasm_sha256.into(),
        source: format!("registry+{registry}"),
    }
}

/// Digest the single `.wasm` artifact in an extracted cache dir (SEC-05):
/// the installer records this in the lockfile so the runtime loader can
/// verify the cached file before compiling it. Requires exactly one `.wasm`
/// — the same structural invariant the runtime's cache validation enforces
/// (a package dir holds one block artifact).
fn wasm_artifact_digest(dir: &Path) -> Result<String> {
    let mut wasm: Option<PathBuf> = None;
    for entry in fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "wasm") {
            if wasm.is_some() {
                bail!(
                    "package dir {} contains more than one .wasm artifact",
                    dir.display()
                );
            }
            wasm = Some(path);
        }
    }
    let wasm =
        wasm.ok_or_else(|| anyhow::anyhow!("no .wasm artifact in package dir {}", dir.display()))?;
    let bytes = fs::read(&wasm).with_context(|| format!("read {}", wasm.display()))?;
    Ok(sha256_hex(&bytes))
}

/// Check whether the cache+lockfile pair already satisfies this version.
/// Returns `Some(sha256)` if satisfied and the download can be skipped.
///
/// Errors if `org`/`block`/`version` can't be turned into a cache path (see
/// `CacheRoot::package_dir`); callers are expected to have already validated
/// `version` as semver before reaching this point.
pub(crate) fn cache_hit(
    cache: &CacheRoot,
    lockfile: &Lockfile,
    org: &str,
    block: &str,
    version: &str,
) -> Result<Option<String>> {
    if !cache.is_populated(org, block, version)? {
        return Ok(None);
    }
    let name = format!("{org}/{block}");
    Ok(lockfile
        .packages
        .iter()
        .find(|p| p.name == name && p.version == version)
        .map(|p| p.sha256.clone()))
}

/// Extract a gzipped tar into `dest` (which must not exist yet; `fs::rename`
/// into the final cache path is the caller's job). Returns number of
/// regular files written.
pub(crate) fn extract_tarball(bytes: &[u8], dest: &Path) -> Result<usize> {
    fs::create_dir_all(dest).with_context(|| format!("create {}", dest.display()))?;
    let mut archive = Archive::new(GzDecoder::new(bytes));
    let mut count = 0usize;
    for entry in archive.entries().context("read tarball entries")? {
        let mut entry = entry.context("read tarball entry")?;
        let entry_path = entry.path().context("read entry path")?.into_owned();

        // Refuse path-traversal attempts (absolute paths or `..` components).
        if entry_path.is_absolute()
            || entry_path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            bail!("tarball contains unsafe path: {}", entry_path.display());
        }

        // Refuse symlink / hardlink entries. A symlink entry whose target
        // escapes `dest` (e.g. `foo -> ../../etc`) followed by a regular
        // file written through that symlink would bypass the path check
        // above. Block archives shouldn't contain links at all.
        let entry_type = entry.header().entry_type();
        if entry_type == EntryType::Symlink || entry_type == EntryType::Link {
            bail!(
                "tarball contains link entry (not allowed): {}",
                entry_path.display()
            );
        }

        let target = dest.join(&entry_path);
        if entry_type.is_dir() {
            fs::create_dir_all(&target).with_context(|| format!("mkdir {}", target.display()))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).context("read entry body")?;
        let mut f =
            File::create(&target).with_context(|| format!("create {}", target.display()))?;
        f.write_all(&buf)
            .with_context(|| format!("write {}", target.display()))?;
        count += 1;
    }
    Ok(count)
}

/// Compute sha256 of `bytes`, hex-encoded. Delegates to the shared
/// implementation in `wafer-block` so the installer's digests (tarball +
/// `wasm_sha256`) use exactly the encoding the runtime loader verifies
/// against — one format, no drift (SEC-05).
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    wafer_block::lockfile::sha256_hex(bytes)
}

/// Full cache-only install orchestration.
pub async fn install_cache_only(
    registry: &Registry,
    cache: &CacheRoot,
    lockfile_path: &Path,
    org: &str,
    block: &str,
    version_req: Option<&str>,
) -> Result<InstallOutcome> {
    // Pre-load lockfile for fast-path checks: if an explicit version was
    // requested and the lockfile already knows about it, we can skip both
    // the registry call and the flock entirely.
    let pre_lock_lf = Lockfile::load(lockfile_path)?.unwrap_or_else(Lockfile::new);

    // Step 1: resolve version. For explicit versions, try the lockfile first
    // to avoid a network call if we already have it cached.
    let (resolved_version, expected_sha, yanked_warning): (String, String, Option<String>) =
        match version_req {
            Some(ver) => {
                // Reject malformed version requests before `ver` can reach
                // any cache path construction — the fast-path
                // `is_populated` check below runs before any registry
                // round-trip, so this has to happen first, not just before
                // `final_dir` further down.
                Version::parse(ver).with_context(|| {
                    format!("invalid version {ver:?} for {org}/{block}: must be valid semver (e.g. 1.2.3)")
                })?;

                // Fast path: if the lockfile already has this version + the
                // cache is populated with it, skip the registry call entirely.
                let name = format!("{org}/{block}");
                if let Some(entry) = pre_lock_lf
                    .packages
                    .iter()
                    .find(|p| p.name == name && p.version == ver)
                {
                    if cache.is_populated(org, block, ver)? {
                        // Network-free: use the lockfile entry.
                        return Ok(InstallOutcome::cached(
                            org,
                            block,
                            entry.version.clone(),
                            entry.sha256.clone(),
                        ));
                    }
                }
                // Fallback: ask the registry.
                let vd: VersionDetail =
                    registry_client::get_version(registry, org, block, ver).await?;
                // The registry response is untrusted network input — a
                // hostile/compromised registry could echo back a
                // `version` field that differs from what we requested
                // (e.g. "../../.."). Require it to be valid semver before
                // it's ever used to build a cache path.
                Version::parse(&vd.version).with_context(|| {
                    format!(
                        "registry returned invalid version {:?} for {org}/{block}",
                        vd.version
                    )
                })?;
                let warn = if vd.yanked != 0 {
                    Some(format!("warning: {org}/{block}@{ver} was yanked"))
                } else {
                    None
                };
                (vd.version, vd.sha256, warn)
            }
            None => {
                let pd = registry_client::get_package(registry, org, block).await?;
                let pick = pick_latest_non_yanked(&pd.versions)
                    .ok_or_else(|| anyhow::anyhow!("no non-yanked versions of {org}/{block}"))?
                    .clone();
                (pick.version, pick.sha256, None)
            }
        };

    if let Some(w) = &yanked_warning {
        eprintln!("{w}");
    }

    // Step 2 (pre-lock): load the lockfile for the fast-path cache_hit check.
    // (Already loaded above for the explicit-version optimization.)

    // Step 3: pre-lock cache-hit shortcut.
    if let Some(cached_sha) = cache_hit(cache, &pre_lock_lf, org, block, &resolved_version)? {
        if cached_sha == expected_sha {
            return Ok(InstallOutcome::cached(
                org,
                block,
                resolved_version,
                expected_sha,
            ));
        }
    }

    // Step 4: acquire the flock.
    let guard = cache.acquire_lock()?;

    // Reload the lockfile under the lock — another installer may have
    // written a newer lockfile while we were waiting for the flock, and
    // we must not stomp their entry when we write below.
    let mut lf = Lockfile::load(lockfile_path)?.unwrap_or_else(Lockfile::new);

    let final_dir = cache.package_dir(org, block, &resolved_version)?;
    let bytes = if final_dir.is_dir() {
        // Another process may have populated while we waited for the lock.
        if lf.packages.iter().any(|p| {
            p.name == format!("{org}/{block}")
                && p.version == resolved_version
                && p.sha256 == expected_sha
        }) {
            // `guard` drops on return — RAII releases the flock.
            return Ok(InstallOutcome::cached(
                org,
                block,
                resolved_version,
                expected_sha,
            ));
        }
        // Cache dir exists but nothing vouches for it — stale, remove and
        // re-download.
        fs::remove_dir_all(&final_dir)
            .with_context(|| format!("remove stale {}", final_dir.display()))?;
        registry_client::download_tarball(registry, org, block, &resolved_version).await?
    } else {
        registry_client::download_tarball(registry, org, block, &resolved_version).await?
    };

    // Verify sha256, extract, and atomically promote into the cache.
    verify_and_promote(&bytes, &expected_sha, &final_dir, |_actual| {
        "integrity check failed: tarball sha256 did not match registry metadata — re-run, and report if it persists".to_string()
    })?;

    // SEC-05: record the digest of the extracted `.wasm` so the runtime
    // loader can verify the cached artifact before compiling it. Computed
    // from the freshly-promoted file (whose bytes came from the tarball we
    // just sha256-verified), so the chain is registry sha → tarball → wasm.
    let wasm_sha256 = wasm_artifact_digest(&final_dir)?;

    // Step 5: update lockfile. This must happen while we still hold the
    // flock, otherwise another installer could acquire the lock, write its
    // own entry, and our write below would silently overwrite it.
    lf.insert_or_replace(lockfile_entry(
        registry,
        org,
        block,
        &resolved_version,
        &expected_sha,
        &wasm_sha256,
    ));
    lf.write_atomic(lockfile_path)?;

    drop(guard);

    Ok(InstallOutcome::fresh(
        org,
        block,
        resolved_version,
        expected_sha,
    ))
}

/// Frozen variant: download + verify against a caller-supplied sha256
/// (the lockfile's pinned sha). No registry metadata fetch, no lockfile
/// mutation. Returns cached+no-op if the cache already has a matching
/// dir and `expected_sha` matches what's provided.
///
/// Used by `install_from_manifest(frozen=true)` to preserve reproducibility:
/// if the registry silently swapped the tarball under the same version,
/// the sha check fails and we bail loudly.
pub async fn install_cache_only_frozen(
    registry: &Registry,
    cache: &CacheRoot,
    org: &str,
    block: &str,
    version: &str,
    expected_sha: &str,
) -> Result<InstallOutcome> {
    // `version` comes straight from wafer.lock — a file that could have
    // been hand-edited or, historically, populated from an unvalidated
    // registry response. Require it to be valid semver before it's used to
    // build any cache path (fast path below, and `final_dir` further down).
    Version::parse(version).with_context(|| {
        format!(
            "wafer.lock has invalid version {version:?} for {org}/{block}: must be valid semver"
        )
    })?;

    // Fast path: if the cache is populated, trust the lockfile sha as the
    // proof of provenance (that's what --frozen means: lockfile is truth).
    if cache.is_populated(org, block, version)? {
        return Ok(InstallOutcome::cached(org, block, version, expected_sha));
    }

    // Slow path: download, hash, verify, extract under the flock.
    let _guard = cache.acquire_lock()?;

    // Re-check under the lock — another installer may have populated.
    let final_dir = cache.package_dir(org, block, version)?;
    if final_dir.is_dir() {
        return Ok(InstallOutcome::cached(org, block, version, expected_sha));
    }

    let bytes = registry_client::download_tarball(registry, org, block, version).await?;

    // Verify sha256, extract, and atomically promote into the cache.
    verify_and_promote(&bytes, expected_sha, &final_dir, |actual| {
        format!(
            "integrity check failed: {org}/{block}@{version} — wafer.lock pins sha256 {expected_sha}, but the registry served a tarball with sha256 {actual}. --frozen refuses to install a version the lockfile doesn't pin."
        )
    })?;

    Ok(InstallOutcome::fresh(org, block, version, expected_sha))
}

/// Full install: `install_cache_only` + mutate `wafer.toml`'s
/// `[dependencies]` to pin the resolved version.
pub async fn install_full(
    registry: &Registry,
    cache: &CacheRoot,
    lockfile_path: &std::path::Path,
    wafer_toml_path: &std::path::Path,
    org: &str,
    block: &str,
    version_req: Option<&str>,
) -> Result<InstallOutcome> {
    let outcome =
        install_cache_only(registry, cache, lockfile_path, org, block, version_req).await?;

    // Mutate wafer.toml to pin the resolved version.
    let mut wt = crate::wafer_toml::WaferToml::read(wafer_toml_path)?;
    let name = format!("{org}/{block}");
    wt.insert_or_replace_dependency(&name, &outcome.version);
    wt.write_atomic(wafer_toml_path)?;

    Ok(outcome)
}

/// Argument-less install. Reads `[dependencies]` from `wafer.toml`, optionally
/// enforces strict sync via `frozen`, installs each entry into the cache,
/// and (when not frozen) rewrites `wafer.lock` from the manifest (pruning
/// orphans).
pub async fn install_from_manifest(
    registry: &Registry,
    cache: &CacheRoot,
    wafer_toml_path: &std::path::Path,
    lockfile_path: &std::path::Path,
    frozen: bool,
) -> Result<Vec<InstallOutcome>> {
    let wt = crate::wafer_toml::WaferToml::read(wafer_toml_path)?;
    let deps = wt.dependencies();
    if deps.is_empty() {
        println!("no dependencies");
        return Ok(Vec::new());
    }

    if frozen {
        // Load lockfile; missing → error.
        let lf = crate::lockfile::Lockfile::load(lockfile_path)?.ok_or_else(|| {
            anyhow::anyhow!("wafer.lock not found — --frozen requires an existing lockfile")
        })?;
        // Drift → error with hint.
        if let Err(e) = crate::sync_check::check(&wt, &lf) {
            anyhow::bail!("{e}\nhint: run 'wafer install' without --frozen to update wafer.lock");
        }
        // All good — install each lockfile entry. install_cache_only is
        // idempotent when the sha matches and the cache is populated
        // (reproducibility preserved; lockfile bytes unchanged if nothing
        // really needs to be fetched).
        let mut out = Vec::with_capacity(lf.packages.len());
        for pkg in &lf.packages {
            let (org, block) = parse_org_block(&pkg.name)?;
            let outcome =
                install_cache_only_frozen(registry, cache, &org, &block, &pkg.version, &pkg.sha256)
                    .await?;
            out.push(outcome);
        }
        return Ok(out);
    }

    // Non-frozen: install each [dependencies] entry. install_cache_only
    // updates the lockfile as it goes. After we're done, prune orphans.
    let mut out = Vec::with_capacity(deps.len());
    for (name, version) in &deps {
        let (org, block) = parse_org_block(name)?;
        let outcome =
            install_cache_only(registry, cache, lockfile_path, &org, &block, Some(version)).await?;
        out.push(outcome);
    }

    // Prune lockfile orphans — wafer.toml is the source of truth.
    let kept: std::collections::BTreeSet<String> = deps.iter().map(|(n, _)| n.clone()).collect();
    let mut lf = crate::lockfile::Lockfile::load(lockfile_path)?
        .unwrap_or_else(crate::lockfile::Lockfile::new);
    let before = lf.packages.len();
    lf.packages.retain(|p| kept.contains(&p.name));
    if lf.packages.len() != before {
        lf.write_atomic(lockfile_path)?;
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vs(version: &str, yanked: i64, sha: &str) -> VersionSummary {
        VersionSummary {
            version: version.into(),
            abi: 1,
            sha256: sha.into(),
            size_bytes: 1,
            license: None,
            yanked,
            published_at: 0,
        }
    }

    #[test]
    fn pick_latest_ignores_yanked() {
        let vs_ = vec![
            vs("0.4.0", 1, "yy"),
            vs("0.3.1", 0, "aa"),
            vs("0.3.0", 0, "bb"),
        ];
        let chosen = pick_latest_non_yanked(&vs_).unwrap();
        assert_eq!(chosen.version, "0.3.1");
    }

    #[test]
    fn pick_latest_orders_by_semver_not_lex() {
        let vs_ = vec![
            vs("0.10.0", 0, "a"),
            vs("0.9.0", 0, "b"),
            vs("0.2.0", 0, "c"),
        ];
        assert_eq!(pick_latest_non_yanked(&vs_).unwrap().version, "0.10.0");
    }

    #[test]
    fn pick_latest_returns_none_if_all_yanked() {
        let vs_ = vec![vs("0.1.0", 1, "a"), vs("0.2.0", 1, "b")];
        assert!(pick_latest_non_yanked(&vs_).is_none());
    }

    #[test]
    fn pick_latest_returns_none_if_empty() {
        assert!(pick_latest_non_yanked(&[]).is_none());
    }

    #[test]
    fn lockfile_entry_formats_source() {
        let e = lockfile_entry(
            &Registry::new("https://wafer.run/"),
            "acme",
            "widget",
            "0.3.1",
            "abc",
            "def",
        );
        assert_eq!(e.name, "acme/widget");
        assert_eq!(e.source, "registry+https://wafer.run");
        assert_eq!(e.wasm_sha256, "def");
    }

    #[test]
    fn sha256_hex_matches_known_value() {
        // SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn extract_tarball_writes_files_and_rejects_traversal() {
        use std::io::Cursor;

        use flate2::{write::GzEncoder, Compression};
        use tempfile::tempdir;

        // Happy archive.
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut tb = tar::Builder::new(&mut gz);
            let mut h = tar::Header::new_gnu();
            h.set_path("wafer.toml").unwrap();
            h.set_size(4);
            h.set_cksum();
            tb.append(&h, Cursor::new(b"a=1\n")).unwrap();
            let mut h2 = tar::Header::new_gnu();
            h2.set_path("w.wasm").unwrap();
            h2.set_size(4);
            h2.set_cksum();
            tb.append(&h2, Cursor::new(b"\0asm")).unwrap();
            tb.finish().unwrap();
        }
        let bytes = gz.finish().unwrap();
        let tmp = tempdir().unwrap();
        let dest = tmp.path().join("out");
        let n = extract_tarball(&bytes, &dest).unwrap();
        assert_eq!(n, 2);
        assert!(dest.join("wafer.toml").is_file());
        assert!(dest.join("w.wasm").is_file());

        // Traversal archive. `set_path` refuses `..`, so poke the name
        // bytes directly to construct a malicious header.
        let mut gz2 = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut tb = tar::Builder::new(&mut gz2);
            let mut h = tar::Header::new_old();
            {
                let name = &mut h.as_old_mut().name;
                let bytes = b"../escape";
                name[..bytes.len()].copy_from_slice(bytes);
            }
            h.set_entry_type(tar::EntryType::Regular);
            h.set_mode(0o644);
            h.set_size(0);
            h.set_cksum();
            tb.append(&h, Cursor::new(b"")).unwrap();
            tb.finish().unwrap();
        }
        let bad = gz2.finish().unwrap();
        let dest2 = tmp.path().join("out2");
        let err = extract_tarball(&bad, &dest2).unwrap_err().to_string();
        assert!(err.contains("unsafe path"), "{err}");
    }

    #[test]
    fn extract_tarball_rejects_symlink_and_hardlink_entries() {
        use flate2::{write::GzEncoder, Compression};
        use tempfile::tempdir;

        fn build(entry_type: tar::EntryType) -> Vec<u8> {
            let mut gz = GzEncoder::new(Vec::new(), Compression::default());
            {
                let mut tb = tar::Builder::new(&mut gz);
                let mut h = tar::Header::new_gnu();
                h.set_path("link").unwrap();
                h.set_size(0);
                h.set_entry_type(entry_type);
                h.set_link_name("../../escape").unwrap();
                h.set_cksum();
                tb.append(&h, std::io::empty()).unwrap();
                tb.finish().unwrap();
            }
            gz.finish().unwrap()
        }

        let tmp = tempdir().unwrap();

        let sym_bytes = build(tar::EntryType::Symlink);
        let err = extract_tarball(&sym_bytes, &tmp.path().join("sym"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("link entry"), "{err}");

        let hard_bytes = build(tar::EntryType::Link);
        let err = extract_tarball(&hard_bytes, &tmp.path().join("hard"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("link entry"), "{err}");
    }

    #[test]
    fn cache_hit_returns_none_when_dir_exists_but_no_lockfile_entry() {
        use tempfile::tempdir;
        let tmp = tempdir().unwrap();
        let cache = CacheRoot::at(tmp.path().to_path_buf());
        fs::create_dir_all(cache.package_dir("a", "b", "1.0.0").unwrap()).unwrap();
        let lf = Lockfile::new();
        // This is the "stale dir" case: dir present, no lockfile entry.
        assert!(cache_hit(&cache, &lf, "a", "b", "1.0.0").unwrap().is_none());
    }

    #[test]
    fn cache_hit_requires_both_dir_and_lockfile_entry() {
        use tempfile::tempdir;
        let tmp = tempdir().unwrap();
        let cache = CacheRoot::at(tmp.path().to_path_buf());
        let mut lf = Lockfile::new();

        // Neither dir nor entry → miss.
        assert!(cache_hit(&cache, &lf, "a", "b", "1.0.0").unwrap().is_none());

        // Dir only → miss.
        fs::create_dir_all(cache.package_dir("a", "b", "1.0.0").unwrap()).unwrap();
        assert!(cache_hit(&cache, &lf, "a", "b", "1.0.0").unwrap().is_none());

        // Both → hit.
        lf.insert_or_replace(LockfilePackage {
            name: "a/b".into(),
            version: "1.0.0".into(),
            sha256: "zzz".into(),
            wasm_sha256: "www".into(),
            source: "registry+https://x".into(),
        });
        assert_eq!(
            cache_hit(&cache, &lf, "a", "b", "1.0.0").unwrap(),
            Some("zzz".into())
        );
    }

    #[test]
    fn cache_hit_rejects_traversal_version() {
        // Same threat model as `package_dir` — a hostile registry response
        // resolved into `cache_hit`'s `version` argument must error, not
        // silently treat it as a cache miss and fall through to the network.
        use tempfile::tempdir;
        let tmp = tempdir().unwrap();
        let cache = CacheRoot::at(tmp.path().to_path_buf());
        let lf = Lockfile::new();
        assert!(cache_hit(&cache, &lf, "a", "b", "../../etc").is_err());
    }

    #[tokio::test]
    async fn install_cache_only_rejects_non_semver_explicit_version() {
        use tempfile::tempdir;
        let tmp = tempdir().unwrap();
        let cache = CacheRoot::at(tmp.path().join("cache"));
        let lockfile_path = tmp.path().join("wafer.lock");
        let registry = Registry::new("https://example.invalid/");

        let err = install_cache_only(
            &registry,
            &cache,
            &lockfile_path,
            "acme",
            "widget",
            Some("../../../etc/passwd"),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("invalid version"), "{err}");
        // No path should have been created outside the cache dir.
        assert!(!tmp.path().join("etc").exists());
    }

    #[tokio::test]
    async fn install_cache_only_frozen_rejects_non_semver_version() {
        use tempfile::tempdir;
        let tmp = tempdir().unwrap();
        let cache = CacheRoot::at(tmp.path().join("cache"));
        let registry = Registry::new("https://example.invalid/");

        let err = install_cache_only_frozen(
            &registry,
            &cache,
            "acme",
            "widget",
            "../../../etc/passwd",
            "deadbeef",
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("invalid version"), "{err}");
        assert!(!tmp.path().join("etc").exists());
    }
}
