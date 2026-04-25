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
    path::Path,
};

use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use semver::Version;
use sha2::{Digest, Sha256};
use tar::{Archive, EntryType};

use crate::{
    cache::CacheRoot,
    lockfile::{Lockfile, LockfilePackage},
    registry_client::{self, VersionDetail, VersionSummary},
};

/// Result of a cache-only install. `from_cache=true` means no network was touched.
#[derive(Debug)]
pub struct InstallOutcome {
    pub org: String,
    pub block: String,
    pub version: String,
    #[allow(dead_code)]
    pub sha256: String,
    pub from_cache: bool,
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
    registry: &str,
    org: &str,
    block: &str,
    version: &str,
    sha256: &str,
) -> LockfilePackage {
    LockfilePackage {
        name: format!("{org}/{block}"),
        version: version.into(),
        sha256: sha256.into(),
        source: format!("registry+{}", registry.trim_end_matches('/')),
    }
}

/// Check whether the cache+lockfile pair already satisfies this version.
/// Returns `Some(sha256)` if satisfied and the download can be skipped.
pub(crate) fn cache_hit(
    cache: &CacheRoot,
    lockfile: &Lockfile,
    org: &str,
    block: &str,
    version: &str,
) -> Option<String> {
    if !cache.is_populated(org, block, version) {
        return None;
    }
    let name = format!("{org}/{block}");
    lockfile
        .packages
        .iter()
        .find(|p| p.name == name && p.version == version)
        .map(|p| p.sha256.clone())
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

/// Compute sha256 of `bytes`, hex-encoded.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Full cache-only install orchestration.
pub async fn install_cache_only(
    registry: &str,
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
                // Fast path: if the lockfile already has this version + the
                // cache is populated with it, skip the registry call entirely.
                let name = format!("{org}/{block}");
                if let Some(entry) = pre_lock_lf
                    .packages
                    .iter()
                    .find(|p| p.name == name && p.version == ver)
                {
                    if cache.is_populated(org, block, ver) {
                        // Network-free: use the lockfile entry.
                        return Ok(InstallOutcome {
                            org: org.into(),
                            block: block.into(),
                            version: entry.version.clone(),
                            sha256: entry.sha256.clone(),
                            from_cache: true,
                        });
                    }
                }
                // Fallback: ask the registry.
                let vd: VersionDetail =
                    registry_client::get_version(registry, org, block, ver).await?;
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
    if let Some(cached_sha) = cache_hit(cache, &pre_lock_lf, org, block, &resolved_version) {
        if cached_sha == expected_sha {
            return Ok(InstallOutcome {
                org: org.into(),
                block: block.into(),
                version: resolved_version,
                sha256: expected_sha,
                from_cache: true,
            });
        }
    }

    // Step 4: acquire the flock.
    let guard = cache.acquire_lock()?;

    // Reload the lockfile under the lock — another installer may have
    // written a newer lockfile while we were waiting for the flock, and
    // we must not stomp their entry when we write below.
    let mut lf = Lockfile::load(lockfile_path)?.unwrap_or_else(Lockfile::new);

    let final_dir = cache.package_dir(org, block, &resolved_version);
    let bytes = if final_dir.is_dir() {
        // Another process may have populated while we waited for the lock.
        if lf.packages.iter().any(|p| {
            p.name == format!("{org}/{block}")
                && p.version == resolved_version
                && p.sha256 == expected_sha
        }) {
            // `guard` drops on return — RAII releases the flock.
            return Ok(InstallOutcome {
                org: org.into(),
                block: block.into(),
                version: resolved_version,
                sha256: expected_sha,
                from_cache: true,
            });
        }
        // Cache dir exists but nothing vouches for it — stale, remove and
        // re-download.
        fs::remove_dir_all(&final_dir)
            .with_context(|| format!("remove stale {}", final_dir.display()))?;
        registry_client::download_tarball(registry, org, block, &resolved_version).await?
    } else {
        registry_client::download_tarball(registry, org, block, &resolved_version).await?
    };

    // Verify sha256.
    let actual = sha256_hex(&bytes);
    if actual != expected_sha {
        bail!(
            "integrity check failed: tarball sha256 did not match registry metadata — re-run, and report if it persists"
        );
    }

    // Extract into a sibling temp dir, then atomic rename.
    let parent = final_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("cache package path has no parent"))?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let tmp_name = format!(".extract-{}", uuid::Uuid::new_v4());
    let tmp_dir = parent.join(&tmp_name);
    if let Err(e) = extract_tarball(&bytes, &tmp_dir) {
        // Mid-stream failure (disk full, bad tarball, ...) may have left
        // a partially populated tmp dir — remove it before bailing.
        let _ = fs::remove_dir_all(&tmp_dir);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp_dir, &final_dir) {
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

    // Step 5: update lockfile. This must happen while we still hold the
    // flock, otherwise another installer could acquire the lock, write its
    // own entry, and our write below would silently overwrite it.
    lf.insert_or_replace(lockfile_entry(
        registry,
        org,
        block,
        &resolved_version,
        &expected_sha,
    ));
    lf.write_atomic(lockfile_path)?;

    drop(guard);

    Ok(InstallOutcome {
        org: org.into(),
        block: block.into(),
        version: resolved_version,
        sha256: expected_sha,
        from_cache: false,
    })
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
        let e = lockfile_entry("https://wafer.run/", "acme", "widget", "0.3.1", "abc");
        assert_eq!(e.name, "acme/widget");
        assert_eq!(e.source, "registry+https://wafer.run");
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
        fs::create_dir_all(cache.package_dir("a", "b", "1.0.0")).unwrap();
        let lf = Lockfile::new();
        // This is the "stale dir" case: dir present, no lockfile entry.
        assert!(cache_hit(&cache, &lf, "a", "b", "1.0.0").is_none());
    }

    #[test]
    fn cache_hit_requires_both_dir_and_lockfile_entry() {
        use tempfile::tempdir;
        let tmp = tempdir().unwrap();
        let cache = CacheRoot::at(tmp.path().to_path_buf());
        let mut lf = Lockfile::new();

        // Neither dir nor entry → miss.
        assert!(cache_hit(&cache, &lf, "a", "b", "1.0.0").is_none());

        // Dir only → miss.
        fs::create_dir_all(cache.package_dir("a", "b", "1.0.0")).unwrap();
        assert!(cache_hit(&cache, &lf, "a", "b", "1.0.0").is_none());

        // Both → hit.
        lf.insert_or_replace(LockfilePackage {
            name: "a/b".into(),
            version: "1.0.0".into(),
            sha256: "zzz".into(),
            source: "registry+https://x".into(),
        });
        assert_eq!(
            cache_hit(&cache, &lf, "a", "b", "1.0.0"),
            Some("zzz".into())
        );
    }
}
