//! Path B: load native WASM blocks from `wafer.lock` + the local cache
//! populated by `wafer install`.
//!
//! `WaferBuilder` drives this at build time: an explicit `.lockfile(path)`
//! calls [`Wafer::load_lockfile`], and the `Auto` lockfile source falls back
//! to [`Wafer::try_load_lockfile_cwd`] (`./wafer.lock`). The `impl Wafer`
//! methods here are `pub(crate)` so WaferBuilder can reach them without
//! exposing them to downstream users.
//!
//! Cache layout (per the `2026-04-24-wafer-search-info-install-design`
//! spec):
//!
//! ```text
//! ~/.wafer/cache/{org}/{block}/{version}/
//! ├── wafer.toml
//! ├── {name}.wasm      (exactly one)
//! └── README.md        (optional)
//! ```

use std::path::{Path, PathBuf};
#[cfg(feature = "wasmi")]
use std::sync::Arc;

use serde::Deserialize;
use wafer_block::{
    error::RuntimeError,
    lockfile::{Lockfile, LockfilePackage, SCHEMA_VERSION},
};

use crate::runtime::Wafer;
#[cfg(feature = "wasmi")]
use crate::wasm::WasmiBlock;

/// Structured lockfile-loader errors. Kept internal to this module; the
/// `impl Wafer` methods below convert to `RuntimeError::Lockfile(String)`
/// at the boundary so wafer-block stays free of wafer-run / toml / wasmi
/// deps.
#[derive(Debug, thiserror::Error)]
pub(crate) enum LockLoaderError {
    #[error("lockfile not found at {}", path.display())]
    LockfileMissing { path: PathBuf },

    #[error("parse {}: {source}", path.display())]
    LockfileParse {
        path: PathBuf,
        source: toml::de::Error,
    },

    #[error(
        "{name}@{version} cache at {}: {reason}. hint: run 'wafer install' to populate the cache",
        path.display()
    )]
    CacheMiss {
        name: String,
        version: String,
        path: PathBuf,
        reason: String,
    },

    // Constructed only when the wasmi feature is enabled (WasmiBlock::load_from_bytes).
    // Without the feature, the wasm-load path is compiled out and this variant is
    // dead — mirrors the WasmiFeatureDisabled variant below which is dead with the
    // feature on.
    #[cfg_attr(not(feature = "wasmi"), allow(dead_code))]
    #[error("{name}@{version}: wasm load failed: {source}")]
    WasmLoadFailed {
        name: String,
        version: String,
        source: RuntimeError,
    },

    // SEC-05: the cached .wasm bytes don't match the lockfile's wasm_sha256.
    // Dead without the wasmi feature (the verify+load path is compiled out),
    // same as WasmLoadFailed above.
    #[cfg_attr(not(feature = "wasmi"), allow(dead_code))]
    #[error(
        "{name}@{version}: cached artifact at {} failed integrity check — \
         wafer.lock pins wasm_sha256 {expected}, cached file hashes to {actual}. \
         The cache may be corrupt or tampered; run 'wafer install' to re-fetch.",
        path.display()
    )]
    IntegrityMismatch {
        name: String,
        version: String,
        path: PathBuf,
        expected: String,
        actual: String,
    },

    #[error("{name}@{version}: unsupported source '{source_value}'")]
    UnsupportedSource {
        name: String,
        version: String,
        source_value: String,
    },

    #[error(
        "invalid lockfile at {}: unsupported version {version} (expected {SCHEMA_VERSION})",
        path.display()
    )]
    UnsupportedVersion { path: PathBuf, version: u32 },

    // Only constructed when the `wasmi` feature is absent; suppress the dead-code
    // lint that fires in full builds where the cfg(not(feature="wasmi")) arm is
    // compiled out.
    #[cfg_attr(feature = "wasmi", allow(dead_code))]
    #[error(
        "{name}@{version}: wafer-run was compiled without the 'wasmi' feature; \
         WASM block loading from a lockfile requires the 'wasmi' feature to be enabled"
    )]
    WasmiFeatureDisabled { name: String, version: String },
}

impl From<LockLoaderError> for RuntimeError {
    fn from(e: LockLoaderError) -> Self {
        RuntimeError::Lockfile(e.to_string())
    }
}

// Lockfile schema: `wafer_block::lockfile` is the single source of truth for
// the on-disk contract, shared with the wafer-cli writer. This module only
// owns the TOML parse + version gate below.

// ---------------------------------------------------------------------------
// wafer.toml schema (only the bits we need for name cross-check)
// ---------------------------------------------------------------------------

// Both structs are only deserialized inside validate_cache, which is gated on
// the wasmi feature for non-test builds. Tests reference validate_cache
// directly so they keep these types alive in test builds.
#[cfg_attr(not(feature = "wasmi"), allow(dead_code))]
#[derive(Deserialize, Debug)]
struct WaferTomlForValidation {
    package: WaferPackage,
}

#[cfg_attr(not(feature = "wasmi"), allow(dead_code))]
#[derive(Deserialize, Debug)]
struct WaferPackage {
    org: String,
    name: String,
}

// ---------------------------------------------------------------------------
// Pure functions (testable without a Wafer instance)
// ---------------------------------------------------------------------------

/// Parse a `wafer.lock` from `path`. Returns `Ok(None)` on NotFound.
pub(crate) fn parse_lockfile(path: &Path) -> Result<Option<Lockfile>, LockLoaderError> {
    let body = match std::fs::read_to_string(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(LockLoaderError::CacheMiss {
                name: String::new(),
                version: String::new(),
                path: path.to_path_buf(),
                reason: format!("io: {e}"),
            });
        }
    };
    let parsed: Lockfile =
        toml::from_str(&body).map_err(|source| LockLoaderError::LockfileParse {
            path: path.to_path_buf(),
            source,
        })?;
    if parsed.version != SCHEMA_VERSION {
        return Err(LockLoaderError::UnsupportedVersion {
            path: path.to_path_buf(),
            version: parsed.version,
        });
    }
    Ok(Some(parsed))
}

pub(crate) fn validate_source(pkg: &LockfilePackage) -> Result<(), LockLoaderError> {
    if pkg.source.starts_with("registry+") || pkg.source.starts_with("path+") {
        Ok(())
    } else {
        Err(LockLoaderError::UnsupportedSource {
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            source_value: pkg.source.clone(),
        })
    }
}

// Only called from validate_cache (wasmi-gated in non-test builds). Tests reach
// it transitively, so the function stays compiled and merely marks as dead in
// the no-wasmi lib build.
#[cfg_attr(not(feature = "wasmi"), allow(dead_code))]
pub(crate) fn split_name(pkg: &LockfilePackage) -> Result<(String, String), LockLoaderError> {
    let (org, block) = pkg
        .name
        .split_once('/')
        .ok_or_else(|| LockLoaderError::CacheMiss {
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            path: PathBuf::new(),
            reason: format!("invalid package name '{}': expected 'org/block'", pkg.name),
        })?;
    // SEC-05: reject path-traversal in the org/block coordinates — a value
    // like `..` or an absolute component would escape / replace the cache root
    // once joined. Shared with the CLI installer via wafer-block.
    if !wafer_block::lockfile::is_valid_path_segment(org)
        || !wafer_block::lockfile::is_valid_path_segment(block)
    {
        return Err(LockLoaderError::CacheMiss {
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            path: PathBuf::new(),
            reason: format!("invalid package name '{}'", pkg.name),
        });
    }
    Ok((org.to_string(), block.to_string()))
}

// Only used by validate_cache; same wasmi-gating story as split_name above.
#[cfg_attr(not(feature = "wasmi"), allow(dead_code))]
pub(crate) fn locate_single_wasm(
    dir: &Path,
    pkg: &LockfilePackage,
) -> Result<PathBuf, LockLoaderError> {
    let mut wasm_files: Vec<PathBuf> = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|e| LockLoaderError::CacheMiss {
        name: pkg.name.clone(),
        version: pkg.version.clone(),
        path: dir.to_path_buf(),
        reason: format!("read_dir failed: {e}"),
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| LockLoaderError::CacheMiss {
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            path: dir.to_path_buf(),
            reason: format!("read_dir entry failed: {e}"),
        })?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "wasm") {
            wasm_files.push(path);
        }
    }
    match wasm_files.len() {
        1 => Ok(wasm_files.remove(0)),
        0 => Err(LockLoaderError::CacheMiss {
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            path: dir.to_path_buf(),
            reason: "no *.wasm file found".into(),
        }),
        n => Err(LockLoaderError::CacheMiss {
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            path: dir.to_path_buf(),
            reason: format!("expected exactly one *.wasm, found {n}"),
        }),
    }
}

// Only called from load_lockfile_parsed inside #[cfg(feature = "wasmi")] in
// non-test builds. Test module references it unconditionally, so it stays
// compiled — but the lib build with the feature off needs to suppress the
// dead-code lint.
#[cfg_attr(not(feature = "wasmi"), allow(dead_code))]
pub(crate) fn validate_cache(
    cache_root: &Path,
    pkg: &LockfilePackage,
) -> Result<PathBuf, LockLoaderError> {
    let (org, block) = split_name(pkg)?;
    // SEC-05: the version is joined onto the cache root too, so it must be a
    // safe single component (registries echo it back — `version = "../../.."`
    // would escape the cache).
    if !wafer_block::lockfile::is_valid_path_segment(&pkg.version) {
        return Err(LockLoaderError::CacheMiss {
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            path: PathBuf::new(),
            reason: format!("invalid version segment '{}'", pkg.version),
        });
    }
    let dir = cache_root.join(&org).join(&block).join(&pkg.version);
    if !dir.is_dir() {
        return Err(LockLoaderError::CacheMiss {
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            path: dir,
            reason: "cache dir missing".into(),
        });
    }

    let wt_path = dir.join("wafer.toml");
    let wt_body = std::fs::read_to_string(&wt_path).map_err(|e| LockLoaderError::CacheMiss {
        name: pkg.name.clone(),
        version: pkg.version.clone(),
        path: wt_path.clone(),
        reason: format!("read wafer.toml: {e}"),
    })?;
    let wt: WaferTomlForValidation =
        toml::from_str(&wt_body).map_err(|e| LockLoaderError::CacheMiss {
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            path: wt_path.clone(),
            reason: format!("parse wafer.toml: {e}"),
        })?;

    let cached_name = format!("{}/{}", wt.package.org, wt.package.name);
    if cached_name != pkg.name {
        return Err(LockLoaderError::CacheMiss {
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            path: wt_path,
            reason: format!("manifest name mismatch: wafer.toml has '{cached_name}'"),
        });
    }

    locate_single_wasm(&dir, pkg)
}

pub(crate) fn default_cache_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".wafer").join("cache"))
}

// ---------------------------------------------------------------------------
// impl Wafer — the glue
// ---------------------------------------------------------------------------

impl Wafer {
    /// Attempt to load blocks from the default lockfile path (`./wafer.lock`).
    /// No-op if missing. `pub(crate)`, invoked by `WaferBuilder` for the
    /// `Auto` lockfile source.
    pub(crate) fn try_load_lockfile_cwd(&mut self) -> Result<usize, RuntimeError> {
        let path = PathBuf::from("wafer.lock");
        match parse_lockfile(&path).map_err(RuntimeError::from)? {
            Some(lf) => self.load_lockfile_parsed(&lf, &default_cache_root_or_err()?),
            None => Ok(0),
        }
    }

    /// Load blocks from an explicit lockfile path. Errors if missing.
    /// `pub(crate)`, invoked by `WaferBuilder` for an explicit `.lockfile(path)`.
    pub(crate) fn load_lockfile(&mut self, path: &Path) -> Result<usize, RuntimeError> {
        let lf = parse_lockfile(path)
            .map_err(RuntimeError::from)?
            .ok_or_else(|| {
                RuntimeError::from(LockLoaderError::LockfileMissing {
                    path: path.to_path_buf(),
                })
            })?;
        self.load_lockfile_parsed(&lf, &default_cache_root_or_err()?)
    }

    /// Test-friendly entry point that takes an explicit `cache_root`.
    #[cfg(test)]
    fn load_lockfile_with_cache(
        &mut self,
        path: &Path,
        cache_root: &Path,
    ) -> Result<usize, RuntimeError> {
        let lf = parse_lockfile(path)
            .map_err(RuntimeError::from)?
            .ok_or_else(|| {
                RuntimeError::from(LockLoaderError::LockfileMissing {
                    path: path.to_path_buf(),
                })
            })?;
        self.load_lockfile_parsed(&lf, cache_root)
    }

    // Without the wasmi feature the for-loop body unconditionally returns on
    // the first iteration (line 343 `return Err`); clippy correctly flags it
    // as `never_loop`, and `count` is never mutated since `count += 1` only
    // lives inside the wasmi-gated arm. Silencing both lints + the unused
    // `cache_root` param under this feature config keeps the two branches
    // structurally parallel without splitting the function in two.
    #[cfg_attr(
        not(feature = "wasmi"),
        allow(unused_variables, unused_mut, clippy::never_loop)
    )]
    fn load_lockfile_parsed(
        &mut self,
        lf: &Lockfile,
        cache_root: &Path,
    ) -> Result<usize, RuntimeError> {
        let mut count = 0usize;
        for pkg in &lf.packages {
            validate_source(pkg).map_err(RuntimeError::from)?;

            // Without wasmi there is no engine to run WASM blocks; surface a
            // clear error rather than silently skipping the entry.
            #[cfg(not(feature = "wasmi"))]
            return Err(RuntimeError::from(LockLoaderError::WasmiFeatureDisabled {
                name: pkg.name.clone(),
                version: pkg.version.clone(),
            }));

            #[cfg(feature = "wasmi")]
            {
                let wasm_path = validate_cache(cache_root, pkg).map_err(RuntimeError::from)?;
                let wasm_bytes = std::fs::read(&wasm_path).map_err(|e| {
                    RuntimeError::from(LockLoaderError::CacheMiss {
                        name: pkg.name.clone(),
                        version: pkg.version.clone(),
                        path: wasm_path.clone(),
                        reason: format!("read wasm bytes: {e}"),
                    })
                })?;
                // SEC-05: verify the cached artifact against the lockfile's
                // recorded digest BEFORE compiling it. The cache dir is
                // mutable on-disk state; the lockfile is the reviewed source
                // of truth. A mismatch (corruption, tampering, or a lockfile
                // steered at an unintended-but-existing dir) is refused, not
                // loaded. An empty pin can never match a real digest, so a
                // hand-blanked wasm_sha256 fails closed here too.
                let actual = wafer_block::lockfile::sha256_hex(&wasm_bytes);
                if actual != pkg.wasm_sha256 {
                    return Err(RuntimeError::from(LockLoaderError::IntegrityMismatch {
                        name: pkg.name.clone(),
                        version: pkg.version.clone(),
                        path: wasm_path,
                        expected: pkg.wasm_sha256.clone(),
                        actual,
                    }));
                }
                // Honour the builder's `fuel_per_call` / `max_wasm_memory_pages`
                // selection for blocks auto-loaded from the lockfile.
                let block = WasmiBlock::load_from_bytes_with_limits(
                    &wasm_bytes,
                    self.wasm.resource_limits(),
                )
                .map_err(|source| {
                    RuntimeError::from(LockLoaderError::WasmLoadFailed {
                        name: pkg.name.clone(),
                        version: pkg.version.clone(),
                        source,
                    })
                })?;
                self.register_block(pkg.name.clone(), Arc::new(block))?;
                tracing::debug!(
                    name = %pkg.name,
                    source = %format!("lockfile:{}", pkg.source),
                    "auto-registered block"
                );
                count += 1;
            }
        }
        Ok(count)
    }
}

fn default_cache_root_or_err() -> Result<PathBuf, RuntimeError> {
    default_cache_root()
        .ok_or_else(|| RuntimeError::Lockfile("cannot resolve default cache root (no HOME)".into()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    const VALID_WAFER_TOML: &str =
        "[package]\norg = \"acme\"\nname = \"widget\"\nversion = \"0.1.0\"\nabi = 1\n";

    /// Minimal valid wasm module bytes ("\0asm" magic + version 1).
    const MINIMAL_WASM: &[u8] = &[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

    fn mk_lockfile(body: &str) -> (PathBuf, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let p = dir.path().join("wafer.lock");
        fs::write(&p, body).unwrap();
        (p, dir)
    }

    fn mk_pkg(name: &str, version: &str, source: &str) -> LockfilePackage {
        LockfilePackage {
            name: name.into(),
            version: version.into(),
            sha256: "a".repeat(64),
            // Default to the digest of the wasm `seed_cache` writes, so any
            // test that seeds the cache and loads passes the SEC-05 integrity
            // check; tests exercising a mismatch override this field.
            wasm_sha256: wafer_block::lockfile::sha256_hex(MINIMAL_WASM),
            source: source.into(),
        }
    }

    fn seed_cache(root: &Path, org: &str, name: &str, version: &str) {
        let dir = root.join(org).join(name).join(version);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("wafer.toml"),
            format!(
                "[package]\norg = \"{org}\"\nname = \"{name}\"\nversion = \"{version}\"\nabi = 1\n"
            ),
        )
        .unwrap();
        fs::write(dir.join(format!("{name}.wasm")), MINIMAL_WASM).unwrap();
    }

    // SEC-05: a lockfile whose coordinates contain path-traversal must be
    // rejected before those values are joined onto the cache root.
    #[test]
    fn split_name_rejects_traversal_segments() {
        assert!(
            split_name(&mk_pkg("../evil", "0.1.0", "registry+https://wafer.run")).is_err(),
            "org '..' must be rejected"
        );
        assert!(
            split_name(&mk_pkg("acme/..", "0.1.0", "registry+https://wafer.run")).is_err(),
            "block '..' must be rejected"
        );
    }

    #[test]
    fn validate_cache_rejects_traversal_version() {
        let tmp = tempdir().unwrap();
        let pkg = mk_pkg("acme/widget", "../../../etc", "registry+https://wafer.run");
        match validate_cache(tmp.path(), &pkg) {
            Err(LockLoaderError::CacheMiss { reason, .. }) => assert!(
                reason.contains("invalid version segment"),
                "version must be rejected at validation, not by an incidental missing dir; got: {reason}"
            ),
            other => panic!("expected version-segment rejection, got {other:?}"),
        }
    }

    #[test]
    fn parse_lockfile_valid_v2() {
        let body = r#"version = 2

[[package]]
name = "acme/widget"
version = "0.3.1"
sha256 = "abc"
wasm_sha256 = "def"
source = "registry+https://wafer.run"
"#;
        let (p, _tmp) = mk_lockfile(body);
        let lf = parse_lockfile(&p).unwrap().unwrap();
        assert_eq!(lf.version, 2);
        assert_eq!(lf.packages.len(), 1);
        assert_eq!(lf.packages[0].name, "acme/widget");
        assert_eq!(lf.packages[0].wasm_sha256, "def");
    }

    #[test]
    fn parse_lockfile_rejects_wrong_version() {
        // v1 is the now-superseded schema; the loader must refuse it (the
        // package is otherwise well-formed so the version check — not a
        // missing-field error — is what fires).
        let body = r#"version = 1

[[package]]
name = "acme/widget"
version = "0.3.1"
sha256 = "abc"
wasm_sha256 = "def"
source = "registry+https://wafer.run"
"#;
        let (p, _tmp) = mk_lockfile(body);
        let err = parse_lockfile(&p).unwrap_err();
        assert!(matches!(
            err,
            LockLoaderError::UnsupportedVersion { version: 1, .. }
        ));
    }

    #[test]
    fn parse_lockfile_missing_returns_ok_none() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("wafer.lock");
        assert!(parse_lockfile(&p).unwrap().is_none());
    }

    #[test]
    fn validate_source_accepts_registry_and_path() {
        let p1 = mk_pkg("a/b", "1.0.0", "registry+https://wafer.run");
        let p2 = mk_pkg("a/b", "1.0.0", "path+./local");
        validate_source(&p1).unwrap();
        validate_source(&p2).unwrap();
    }

    #[test]
    fn validate_source_rejects_git_plus() {
        let p = mk_pkg("acme/widget", "0.1.0", "git+https://example.com");
        let err = validate_source(&p).unwrap_err();
        assert!(matches!(err, LockLoaderError::UnsupportedSource { .. }));
    }

    #[test]
    fn validate_cache_missing_dir() {
        let tmp = tempdir().unwrap();
        let pkg = mk_pkg("acme/widget", "0.1.0", "registry+https://wafer.run");
        let err = validate_cache(tmp.path(), &pkg).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("acme/widget"), "{msg}");
        assert!(msg.contains("cache dir missing"), "{msg}");
    }

    #[test]
    fn validate_cache_manifest_name_mismatch() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("acme").join("widget").join("0.1.0");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("wafer.toml"),
            "[package]\norg = \"wrong\"\nname = \"widget\"\nversion = \"0.1.0\"\nabi = 1\n",
        )
        .unwrap();
        fs::write(dir.join("widget.wasm"), MINIMAL_WASM).unwrap();

        let pkg = mk_pkg("acme/widget", "0.1.0", "registry+https://wafer.run");
        let err = validate_cache(tmp.path(), &pkg).unwrap_err();
        assert!(err.to_string().contains("manifest name mismatch"), "{err}");
    }

    #[test]
    fn validate_cache_zero_wasm_files() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("acme").join("widget").join("0.1.0");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("wafer.toml"), VALID_WAFER_TOML).unwrap();

        let pkg = mk_pkg("acme/widget", "0.1.0", "registry+https://wafer.run");
        let err = validate_cache(tmp.path(), &pkg).unwrap_err();
        assert!(err.to_string().contains("no *.wasm file found"), "{err}");
    }

    #[test]
    fn validate_cache_multiple_wasm_files() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("acme").join("widget").join("0.1.0");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("wafer.toml"), VALID_WAFER_TOML).unwrap();
        fs::write(dir.join("a.wasm"), MINIMAL_WASM).unwrap();
        fs::write(dir.join("b.wasm"), MINIMAL_WASM).unwrap();

        let pkg = mk_pkg("acme/widget", "0.1.0", "registry+https://wafer.run");
        let err = validate_cache(tmp.path(), &pkg).unwrap_err();
        assert!(
            err.to_string()
                .contains("expected exactly one *.wasm, found 2"),
            "{err}"
        );
    }

    #[cfg(feature = "wasmi")]
    #[test]
    fn load_lockfile_happy_path_registers_block() {
        let tmp = tempdir().unwrap();
        let wasm_sha = wafer_block::lockfile::sha256_hex(MINIMAL_WASM);
        let lock_body = format!(
            r#"version = 2

[[package]]
name = "acme/widget"
version = "0.1.0"
sha256 = "abc"
wasm_sha256 = "{wasm_sha}"
source = "registry+https://wafer.run"
"#
        );
        let lock_path = tmp.path().join("wafer.lock");
        fs::write(&lock_path, lock_body).unwrap();
        seed_cache(tmp.path(), "acme", "widget", "0.1.0");

        let mut w = Wafer::builder()
            .disable_inventory()
            .disable_lockfile()
            .build()
            .expect("empty wafer build is infallible");
        let n = w.load_lockfile_with_cache(&lock_path, tmp.path()).unwrap();
        assert_eq!(n, 1);
    }

    #[cfg(feature = "wasmi")]
    #[test]
    fn load_lockfile_rejects_tampered_wasm() {
        // SEC-05: the cache holds a .wasm whose bytes don't match the
        // lockfile's wasm_sha256 (corruption / tampering / a lockfile steered
        // at an unintended dir). The loader must refuse it, not compile it.
        let tmp = tempdir().unwrap();
        // Pin a digest of DIFFERENT bytes than seed_cache writes.
        let wrong_sha = wafer_block::lockfile::sha256_hex(b"not the real artifact");
        let lock_body = format!(
            r#"version = 2

[[package]]
name = "acme/widget"
version = "0.1.0"
sha256 = "abc"
wasm_sha256 = "{wrong_sha}"
source = "registry+https://wafer.run"
"#
        );
        let lock_path = tmp.path().join("wafer.lock");
        fs::write(&lock_path, lock_body).unwrap();
        seed_cache(tmp.path(), "acme", "widget", "0.1.0");

        let mut w = Wafer::builder()
            .disable_inventory()
            .disable_lockfile()
            .build()
            .expect("empty wafer build is infallible");
        let err = w
            .load_lockfile_with_cache(&lock_path, tmp.path())
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("integrity check"), "{msg}");
        assert!(msg.contains("acme/widget"), "{msg}");
    }

    #[cfg(feature = "wasmi")]
    #[test]
    fn load_lockfile_rejects_empty_wasm_digest() {
        // A hand-blanked wasm_sha256 must fail closed — an empty pin can
        // never equal a real digest, so the artifact is refused.
        let tmp = tempdir().unwrap();
        let lock_body = r#"version = 2

[[package]]
name = "acme/widget"
version = "0.1.0"
sha256 = "abc"
wasm_sha256 = ""
source = "registry+https://wafer.run"
"#;
        let lock_path = tmp.path().join("wafer.lock");
        fs::write(&lock_path, lock_body).unwrap();
        seed_cache(tmp.path(), "acme", "widget", "0.1.0");

        let mut w = Wafer::builder()
            .disable_inventory()
            .disable_lockfile()
            .build()
            .expect("empty wafer build is infallible");
        let err = w
            .load_lockfile_with_cache(&lock_path, tmp.path())
            .unwrap_err();
        assert!(err.to_string().contains("integrity check"), "{err}");
    }

    #[cfg(feature = "wasmi")]
    #[test]
    fn load_lockfile_duplicate_name_errors() {
        let tmp = tempdir().unwrap();
        let wasm_sha = wafer_block::lockfile::sha256_hex(MINIMAL_WASM);
        let lock_body = format!(
            r#"version = 2

[[package]]
name = "acme/widget"
version = "0.1.0"
sha256 = "abc"
wasm_sha256 = "{wasm_sha}"
source = "registry+https://wafer.run"

[[package]]
name = "acme/widget"
version = "0.2.0"
sha256 = "def"
wasm_sha256 = "{wasm_sha}"
source = "registry+https://wafer.run"
"#
        );
        let lock_path = tmp.path().join("wafer.lock");
        fs::write(&lock_path, lock_body).unwrap();
        seed_cache(tmp.path(), "acme", "widget", "0.1.0");
        seed_cache(tmp.path(), "acme", "widget", "0.2.0");

        let mut w = Wafer::builder()
            .disable_inventory()
            .disable_lockfile()
            .build()
            .expect("empty wafer build is infallible");
        let err = w
            .load_lockfile_with_cache(&lock_path, tmp.path())
            .unwrap_err();
        assert!(err.to_string().contains("already registered"), "{err}");
    }

    #[cfg(feature = "wasmi")]
    #[test]
    fn load_lockfile_cache_missing_surfaces_cache_miss() {
        let tmp = tempdir().unwrap();
        let lock_body = r#"version = 2

[[package]]
name = "acme/widget"
version = "0.1.0"
sha256 = "abc"
wasm_sha256 = "def"
source = "registry+https://wafer.run"
"#;
        let lock_path = tmp.path().join("wafer.lock");
        fs::write(&lock_path, lock_body).unwrap();

        let mut w = Wafer::builder()
            .disable_inventory()
            .disable_lockfile()
            .build()
            .expect("empty wafer build is infallible");
        let err = w
            .load_lockfile_with_cache(&lock_path, tmp.path())
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("acme/widget"), "{msg}");
        assert!(msg.contains("cache dir missing"), "{msg}");
    }

    /// Without the `wasmi` feature, loading a lockfile with WASM entries must
    /// fail with a clear `WasmiFeatureDisabled` error rather than a compiler
    /// error or silent no-op.
    #[cfg(not(feature = "wasmi"))]
    #[test]
    fn load_lockfile_without_wasmi_errors_clearly() {
        let tmp = tempdir().unwrap();
        let lock_body = r#"version = 1

[[package]]
name = "acme/widget"
version = "0.1.0"
sha256 = "abc"
source = "registry+https://wafer.run"
"#;
        let lock_path = tmp.path().join("wafer.lock");
        fs::write(&lock_path, lock_body).unwrap();

        let mut w = Wafer::builder()
            .disable_inventory()
            .disable_lockfile()
            .build()
            .expect("empty wafer build is infallible");
        let err = w
            .load_lockfile_with_cache(&lock_path, tmp.path())
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("wasmi"),
            "expected wasmi feature error, got: {msg}"
        );
        assert!(
            msg.contains("acme/widget"),
            "expected package name in error, got: {msg}"
        );
    }
}
