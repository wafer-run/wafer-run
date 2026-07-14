//! Shared `wafer.lock` schema types.
//!
//! Single source of truth for the lockfile's on-disk contract, consumed by
//! both the writer (`wafer-cli`'s `wafer install`) and the reader
//! (`wafer-run`'s registry loader). This module holds only format-agnostic
//! serde types plus the schema-version constant — it deliberately has **no
//! TOML dependency** (wafer-block must stay wasm32-clean), so each consumer
//! owns its own TOML parsing/serialization and file IO.
//!
//! # On-disk contract (v1)
//!
//! `wafer.lock` is TOML:
//!
//! ```toml
//! version = 1
//!
//! [[package]]
//! name = "acme/widget"
//! version = "0.3.1"
//! sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
//! source = "registry+https://wafer.run"
//! ```
//!
//! - The top-level `version` field is the schema version and is **required**
//!   (no serde default); parsers must reject any value other than
//!   [`SCHEMA_VERSION`].
//! - Each package is a `[[package]]` array-of-tables entry (serde rename of
//!   the `packages` field). A lockfile with no packages is valid.
//! - `[[package]]` entries are stored sorted by `name`.
//!   [`Lockfile::insert_or_replace`] preserves that invariant so serializers
//!   emit deterministic output regardless of insertion order.
//! - `source` follows the Cargo convention `registry+<base-url>` (or
//!   `path+<dir>` for local sources) — forward-compatible with future
//!   multi-registry support.

use serde::{Deserialize, Serialize};

/// Current `wafer.lock` schema version. Parsers reject any other value.
pub const SCHEMA_VERSION: u32 = 1;

/// One `[[package]]` entry in `wafer.lock`.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct LockfilePackage {
    /// Block name in `{org}/{block}` form, e.g. `acme/widget`.
    pub name: String,
    /// Exact resolved package version, e.g. `0.3.1`.
    pub version: String,
    /// Hex-encoded sha256 of the package tarball, used for integrity checks
    /// during install and cache resolution.
    pub sha256: String,
    /// Provenance in Cargo convention: `registry+<base-url>` or `path+<dir>`.
    pub source: String,
}

/// In-memory form of a `wafer.lock` file.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Lockfile {
    /// Schema version; must equal [`SCHEMA_VERSION`]. Required on parse —
    /// a lockfile without it is rejected.
    pub version: u32,
    /// Package entries, kept sorted by `name` (the `[[package]]` sections).
    #[serde(default, rename = "package")]
    pub packages: Vec<LockfilePackage>,
}

impl Lockfile {
    /// Empty current-schema lockfile (for when no `wafer.lock` exists on
    /// disk yet).
    pub fn new() -> Self {
        Self {
            version: SCHEMA_VERSION,
            packages: Vec::new(),
        }
    }

    /// Insert or replace a package entry (keyed by `name`), keeping
    /// `packages` sorted by name.
    pub fn insert_or_replace(&mut self, pkg: LockfilePackage) {
        if let Some(existing) = self.packages.iter_mut().find(|p| p.name == pkg.name) {
            *existing = pkg;
        } else {
            self.packages.push(pkg);
        }
        self.packages.sort_by(|a, b| a.name.cmp(&b.name));
    }
}

impl Default for Lockfile {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether `s` is a safe single path component — exactly one
/// `std::path::Component::Normal` equal to `s`: no `.`/`..`, no path
/// separators, not absolute, not empty.
///
/// Shared by the CLI installer and the runtime lockfile loader (SEC-05) so
/// both reject path-traversal in package coordinates (`org`, `block`,
/// `version`) identically — a value like `..`, `../evil`, `/etc`, or
/// `a/b` must never be joined onto the cache root, where `..` could escape it
/// and an absolute component could replace the whole path.
pub fn is_valid_path_segment(s: &str) -> bool {
    use std::path::{Component, Path};
    let mut components = Path::new(s).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(seg)), None) if seg.to_str() == Some(s)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_path_segment_accepts_normal_rejects_traversal() {
        for ok in ["widget", "acme", "0.1.0", "my-block_2"] {
            assert!(is_valid_path_segment(ok), "{ok} should be valid");
        }
        for bad in ["", ".", "..", "../evil", "a/b", "/etc", "./x", "../../.."] {
            assert!(!is_valid_path_segment(bad), "{bad:?} should be rejected");
        }
    }

    fn pkg(name: &str, version: &str) -> LockfilePackage {
        LockfilePackage {
            name: name.into(),
            version: version.into(),
            sha256: "a".repeat(64),
            source: "registry+https://wafer.run".into(),
        }
    }

    #[test]
    fn empty_new_has_current_schema_version() {
        let lf = Lockfile::new();
        assert_eq!(lf.version, SCHEMA_VERSION);
        assert!(lf.packages.is_empty());
    }

    #[test]
    fn insert_or_replace_inserts_in_sorted_order() {
        let mut lf = Lockfile::new();
        lf.insert_or_replace(pkg("zeta/z", "0.1.0"));
        lf.insert_or_replace(pkg("acme/widget", "0.3.1"));
        lf.insert_or_replace(pkg("mid/m", "1.0.0"));
        let names: Vec<_> = lf.packages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["acme/widget", "mid/m", "zeta/z"]);
    }

    #[test]
    fn insert_or_replace_replaces_existing() {
        let mut lf = Lockfile::new();
        lf.insert_or_replace(pkg("a/b", "0.1.0"));
        lf.insert_or_replace(pkg("a/b", "0.2.0"));
        assert_eq!(lf.packages.len(), 1);
        assert_eq!(lf.packages[0].version, "0.2.0");
    }
}
