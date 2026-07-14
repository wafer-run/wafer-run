//! Cache layout + advisory flock for `~/.wafer/cache/`.
//!
//! Layout:
//!
//! ```text
//! ~/.wafer/
//! └── cache/
//!     ├── .lock                   advisory flock, zero-byte file
//!     └── {org}/{block}/{version}/
//!         ├── wafer.toml          verbatim from tarball
//!         ├── {name}.wasm         verbatim from tarball
//!         └── README.md           optional
//! ```
//!
//! `CacheRoot` centralises path construction and the flock dance. Reads
//! (cache-hit checks) don't lock; only download+extract does, for the
//! duration of that critical section.
//!
//! Default lock timeout is 60 seconds; override via `WAFER_INSTALL_LOCK_TIMEOUT_SECS`
//! (integration tests set `5`). Zero or negative values are treated as
//! "don't wait — fail immediately if the lock is held".

use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    thread::sleep,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use fs2::FileExt;

const LOCK_TIMEOUT_ENV: &str = "WAFER_INSTALL_LOCK_TIMEOUT_SECS";
const DEFAULT_LOCK_TIMEOUT_SECS: u64 = 60;
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// A cache path segment (org, block, or version) failed validation.
///
/// `org`/`block`/`version` all become raw path components under the cache
/// root (`{root}/{org}/{block}/{version}`). `org`/`block` are attacker-shaped
/// by whatever the caller passed to the CLI; `version` can be echoed
/// verbatim by the registry (see `install.rs`). None of the three may carry
/// `..`, be absolute, or embed a `/` — otherwise a hostile/compromised
/// registry response like `version = "../../.."` could steer
/// `fs::remove_dir_all`/`fs::rename` outside the cache root.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error(
        "invalid {field} {value:?}: must be a single path segment (no \"..\", \".\", leading \"/\", or embedded \"/\")"
    )]
    InvalidSegment { field: &'static str, value: String },
}

/// Reject anything that isn't a single safe path segment (no `..`, `.`,
/// absolute path, empty string, or embedded separator). Delegates to
/// [`wafer_block::lockfile::is_valid_path_segment`] — the same check the
/// runtime lockfile loader applies (SEC-05) — so the installer and loader
/// cannot drift.
fn validate_segment(field: &'static str, value: &str) -> Result<(), CacheError> {
    if wafer_block::lockfile::is_valid_path_segment(value) {
        Ok(())
    } else {
        Err(CacheError::InvalidSegment {
            field,
            value: value.to_string(),
        })
    }
}

/// Root of the on-disk cache; wraps an absolute path under the user's home.
#[derive(Debug, Clone)]
pub struct CacheRoot {
    root: PathBuf,
}

impl CacheRoot {
    /// Resolve the default cache root `~/.wafer/cache/`.
    /// Errors if the OS doesn't expose a home directory.
    pub fn default_location() -> Result<Self> {
        Ok(Self::at(crate::paths::wafer_home()?.join("cache")))
    }

    /// Construct a `CacheRoot` for an explicit path (tests use `tempdir`).
    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }

    #[expect(dead_code, reason = "kept for future cache-introspection commands")]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Final directory for a specific package@version.
    ///
    /// `org`, `block`, and `version` are validated as single, normal path
    /// segments before being joined — see [`CacheError`]. `version` in
    /// particular may originate from the registry (untrusted network input);
    /// callers must not skip this by building the path any other way.
    pub fn package_dir(
        &self,
        org: &str,
        block: &str,
        version: &str,
    ) -> Result<PathBuf, CacheError> {
        validate_segment("org", org)?;
        validate_segment("block", block)?;
        validate_segment("version", version)?;
        Ok(self.root.join(org).join(block).join(version))
    }

    /// Path to the advisory lock file.
    pub fn lock_file(&self) -> PathBuf {
        self.root.join(".lock")
    }

    /// Ensure the root exists (`0755`). Idempotent.
    pub fn ensure_root(&self) -> Result<()> {
        fs::create_dir_all(&self.root)
            .with_context(|| format!("create {}", self.root.display()))?;
        Ok(())
    }

    /// Return true iff the final version dir exists. Does not hit the lock.
    pub fn is_populated(&self, org: &str, block: &str, version: &str) -> Result<bool, CacheError> {
        Ok(self.package_dir(org, block, version)?.is_dir())
    }

    /// Acquire the advisory flock, polling every 100 ms until acquired or
    /// timeout. Returns a `CacheLock` RAII guard; drop it to release.
    pub fn acquire_lock(&self) -> Result<CacheLock> {
        self.ensure_root()?;
        let path = self.lock_file();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;

        let timeout_secs = match std::env::var(LOCK_TIMEOUT_ENV) {
            Ok(s) => s
                .parse::<i64>()
                .with_context(|| format!("{LOCK_TIMEOUT_ENV} must be an integer, got {s:?}"))?,
            Err(_) => DEFAULT_LOCK_TIMEOUT_SECS as i64,
        };
        let deadline = if timeout_secs > 0 {
            Some(Instant::now() + Duration::from_secs(timeout_secs as u64))
        } else {
            None
        };

        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(CacheLock { file }),
                Err(e) => {
                    if let Some(deadline) = deadline {
                        if Instant::now() >= deadline {
                            bail!(
                                "timed out acquiring {} after {} s (another wafer install may be running)",
                                path.display(),
                                timeout_secs
                            );
                        }
                        sleep(LOCK_POLL_INTERVAL);
                    } else {
                        return Err(e).with_context(|| {
                            format!("lock {} (another wafer install holds it)", path.display())
                        });
                    }
                }
            }
        }
    }
}

/// RAII guard for the cache flock. Releases on drop.
#[derive(Debug)]
pub struct CacheLock {
    file: File,
}

impl Drop for CacheLock {
    fn drop(&mut self) {
        // Best-effort; if unlock fails the OS will clean up on process exit.
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex, OnceLock},
        thread,
        time::Instant,
    };

    use tempfile::tempdir;

    use super::*;

    /// Serializes tests that mutate the process-wide
    /// `WAFER_INSTALL_LOCK_TIMEOUT_SECS` env var so parallel test threads
    /// don't race each other.
    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        static M: OnceLock<Mutex<()>> = OnceLock::new();
        M.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn package_dir_composes_segments() {
        let tmp = tempdir().unwrap();
        let c = CacheRoot::at(tmp.path().to_path_buf());
        let p = c.package_dir("acme", "widget", "0.3.1").unwrap();
        assert!(p.ends_with("acme/widget/0.3.1"));
    }

    #[test]
    fn package_dir_rejects_traversal_in_version() {
        // A compromised/hostile registry echoing version="../../.." must not
        // be able to build a cache path that escapes the cache root and
        // gets fed to remove_dir_all.
        let tmp = tempdir().unwrap();
        let cache = CacheRoot::at(tmp.path().to_path_buf());
        assert!(cache.package_dir("acme", "widget", "../../..").is_err());
        assert!(cache.package_dir("acme", "widget", "/etc").is_err());
    }

    #[test]
    fn package_dir_rejects_traversal_in_org_or_block() {
        let tmp = tempdir().unwrap();
        let cache = CacheRoot::at(tmp.path().to_path_buf());
        assert!(cache.package_dir("..", "widget", "1.0.0").is_err());
        assert!(cache.package_dir("acme", "..", "1.0.0").is_err());
        assert!(cache.package_dir("a/b", "widget", "1.0.0").is_err());
        assert!(cache.package_dir("acme", "a/b", "1.0.0").is_err());
    }

    #[test]
    fn package_dir_rejects_dot_and_embedded_separator() {
        let tmp = tempdir().unwrap();
        let cache = CacheRoot::at(tmp.path().to_path_buf());
        assert!(cache.package_dir("acme", "widget", ".").is_err());
        assert!(cache
            .package_dir("acme", "widget", "1.0.0/../../x")
            .is_err());
    }

    #[test]
    fn is_populated_rejects_invalid_version() {
        let tmp = tempdir().unwrap();
        let cache = CacheRoot::at(tmp.path().to_path_buf());
        assert!(cache.is_populated("acme", "widget", "../../etc").is_err());
    }

    #[test]
    fn ensure_root_creates_the_directory() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("cache");
        let c = CacheRoot::at(root.clone());
        assert!(!root.exists());
        c.ensure_root().unwrap();
        assert!(root.is_dir());
    }

    #[test]
    fn is_populated_sees_the_version_dir() {
        let tmp = tempdir().unwrap();
        let c = CacheRoot::at(tmp.path().to_path_buf());
        assert!(!c.is_populated("a", "b", "1.0.0").unwrap());
        fs::create_dir_all(c.package_dir("a", "b", "1.0.0").unwrap()).unwrap();
        assert!(c.is_populated("a", "b", "1.0.0").unwrap());
    }

    #[test]
    fn acquire_lock_then_release_allows_next_acquire() {
        let _guard = env_guard();
        let tmp = tempdir().unwrap();
        let c = CacheRoot::at(tmp.path().to_path_buf());
        {
            let _g = c.acquire_lock().unwrap();
        }
        // Released on drop; next acquire is fine.
        let _g2 = c.acquire_lock().unwrap();
    }

    #[test]
    fn concurrent_acquire_blocks_until_first_releases() {
        let _guard = env_guard();
        let tmp = tempdir().unwrap();
        let c = Arc::new(CacheRoot::at(tmp.path().to_path_buf()));

        // Short test timeout via env var.
        std::env::set_var(LOCK_TIMEOUT_ENV, "5");

        let c_first = Arc::clone(&c);
        let first = thread::spawn(move || {
            let g = c_first.acquire_lock().unwrap();
            thread::sleep(Duration::from_millis(300));
            drop(g);
        });

        // Give thread 1 time to grab the lock.
        thread::sleep(Duration::from_millis(50));

        let t0 = Instant::now();
        let _g2 = c.acquire_lock().unwrap();
        let waited = t0.elapsed();
        first.join().unwrap();

        assert!(
            waited >= Duration::from_millis(150),
            "expected to block, waited only {waited:?}"
        );

        std::env::remove_var(LOCK_TIMEOUT_ENV);
    }

    #[test]
    fn malformed_timeout_env_produces_clear_error() {
        let _guard = env_guard();
        let tmp = tempdir().unwrap();
        let c = CacheRoot::at(tmp.path().to_path_buf());
        std::env::set_var(LOCK_TIMEOUT_ENV, "5s");
        let err = c.acquire_lock().unwrap_err().to_string();
        std::env::remove_var(LOCK_TIMEOUT_ENV);
        assert!(
            err.contains("must be an integer") && err.contains("\"5s\""),
            "{err}"
        );
    }

    #[test]
    fn lock_timeout_surfaces_as_error() {
        let _guard = env_guard();
        // NOTE: process-level flocks are per-file-descriptor on Linux but
        // per-process-open on some OSes. Rather than mix threads (which
        // share the fd table on Linux) we simulate contention by opening a
        // SECOND file handle from within the same process — each handle
        // has its own lock state.
        let tmp = tempdir().unwrap();
        let c = CacheRoot::at(tmp.path().to_path_buf());
        c.ensure_root().unwrap();

        // Acquire the lock on a separate file handle held for the whole test.
        let holder_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(c.lock_file())
            .unwrap();
        holder_file.try_lock_exclusive().unwrap();

        // Try to acquire with a 1-second timeout via env var.
        std::env::set_var(LOCK_TIMEOUT_ENV, "1");
        let err = c.acquire_lock().unwrap_err().to_string();
        std::env::remove_var(LOCK_TIMEOUT_ENV);

        assert!(err.contains("timed out"), "{err}");

        // Clean up: release the holder lock explicitly.
        let _ = holder_file.unlock();
    }
}
