//! Path B: load native WASM blocks from `wafer.lock` + the local cache
//! populated by `wafer install`.
//!
//! PR β ships the primitives and their unit tests. `Wafer::new()` does
//! NOT call into this module yet — that wiring lands in PR γ via
//! `WaferBuilder`. The `impl Wafer` methods here are `pub(crate)` so
//! WaferBuilder can reach them without exposing them to downstream users.
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
use wafer_block::error::RuntimeError;

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

    #[error("{name}@{version}: unsupported source '{source_value}'")]
    UnsupportedSource {
        name: String,
        version: String,
        source_value: String,
    },

    #[error("invalid lockfile at {}: unsupported version {version} (expected 1)", path.display())]
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

// ---------------------------------------------------------------------------
// Lockfile schema (mirrored locally — wafer-run can't depend on wafer-cli)
// ---------------------------------------------------------------------------

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct Lockfile {
    pub(crate) version: u32,
    #[serde(default, rename = "package")]
    pub(crate) packages: Vec<LockfilePackage>,
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct LockfilePackage {
    pub(crate) name: String,
    pub(crate) version: String,
    #[expect(
        dead_code,
        reason = "verified during cache resolution; not consumed by this struct's own callers"
    )]
    pub(crate) sha256: String,
    pub(crate) source: String,
}

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
    if parsed.version != 1 {
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
    if org.is_empty() || block.is_empty() || block.contains('/') {
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
    /// No-op if missing. `pub(crate)` — wired into WaferBuilder in PR γ.
    pub(crate) fn try_load_lockfile_cwd(&mut self) -> Result<usize, RuntimeError> {
        let path = PathBuf::from("wafer.lock");
        match parse_lockfile(&path).map_err(RuntimeError::from)? {
            Some(lf) => self.load_lockfile_parsed(&lf, &default_cache_root_or_err()?),
            None => Ok(0),
        }
    }

    /// Load blocks from an explicit lockfile path. Errors if missing.
    /// `pub(crate)` — wired into WaferBuilder in PR γ.
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
                let block = WasmiBlock::load_from_bytes(&wasm_bytes).map_err(|source| {
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

    #[test]
    fn parse_lockfile_valid_v1() {
        let body = r#"version = 1

[[package]]
name = "acme/widget"
version = "0.3.1"
sha256 = "abc"
source = "registry+https://wafer.run"
"#;
        let (p, _tmp) = mk_lockfile(body);
        let lf = parse_lockfile(&p).unwrap().unwrap();
        assert_eq!(lf.version, 1);
        assert_eq!(lf.packages.len(), 1);
        assert_eq!(lf.packages[0].name, "acme/widget");
    }

    #[test]
    fn parse_lockfile_rejects_v2() {
        let body = "version = 2\n";
        let (p, _tmp) = mk_lockfile(body);
        let err = parse_lockfile(&p).unwrap_err();
        assert!(matches!(
            err,
            LockLoaderError::UnsupportedVersion { version: 2, .. }
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
        let lock_body = r#"version = 1

[[package]]
name = "acme/widget"
version = "0.1.0"
sha256 = "abc"
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
        let n = w.load_lockfile_with_cache(&lock_path, tmp.path()).unwrap();
        assert_eq!(n, 1);
    }

    #[cfg(feature = "wasmi")]
    #[test]
    fn load_lockfile_duplicate_name_errors() {
        let tmp = tempdir().unwrap();
        let lock_body = r#"version = 1

[[package]]
name = "acme/widget"
version = "0.1.0"
sha256 = "abc"
source = "registry+https://wafer.run"

[[package]]
name = "acme/widget"
version = "0.2.0"
sha256 = "def"
source = "registry+https://wafer.run"
"#;
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
