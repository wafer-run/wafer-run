//! `wafer.lock` parser + serializer.
//!
//! Format (v1):
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
//! - Top-level `version = 1` is the schema version; unknown values are
//!   rejected on parse.
//! - `[[package]]` entries are stored sorted by `name`. `insert_or_replace`
//!   + `to_toml_string` preserve that invariant so any consumer can rely on
//!   deterministic output regardless of insertion order.
//! - `source` follows the Cargo convention `registry+<base-url>` — forward-
//!   compatible with future multi-registry support.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

const SCHEMA_VERSION: u32 = 1;

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct LockfilePackage {
    pub name: String,
    pub version: String,
    pub sha256: String,
    pub source: String,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Lockfile {
    pub version: u32,
    #[serde(default, rename = "package")]
    pub packages: Vec<LockfilePackage>,
}

impl Lockfile {
    /// Empty v1 lockfile (for when no `wafer.lock` exists on disk yet).
    pub fn new() -> Self {
        Self {
            version: SCHEMA_VERSION,
            packages: Vec::new(),
        }
    }

    /// Read + parse `wafer.lock` from `path`. Missing file yields `Ok(None)`
    /// so callers can distinguish "no lockfile yet" from "parse error".
    pub fn load(path: &Path) -> Result<Option<Self>> {
        match fs::read_to_string(path) {
            Ok(body) => {
                let parsed: Lockfile =
                    toml::from_str(&body).with_context(|| format!("parse {}", path.display()))?;
                if parsed.version != SCHEMA_VERSION {
                    bail!(
                        "{}: unsupported lockfile version {} (expected {SCHEMA_VERSION})",
                        path.display(),
                        parsed.version
                    );
                }
                Ok(Some(parsed))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    /// Insert or replace a package entry, keeping `packages` sorted by name.
    pub fn insert_or_replace(&mut self, pkg: LockfilePackage) {
        if let Some(existing) = self.packages.iter_mut().find(|p| p.name == pkg.name) {
            *existing = pkg;
        } else {
            self.packages.push(pkg);
        }
        self.packages.sort_by(|a, b| a.name.cmp(&b.name));
    }

    /// Serialize to canonical TOML text. Packages emitted in sorted order,
    /// trailing newline.
    pub fn to_toml_string(&self) -> Result<String> {
        let mut ordered = self.clone();
        ordered.packages.sort_by(|a, b| a.name.cmp(&b.name));
        let mut out = toml::to_string(&ordered).context("serialise lockfile")?;
        if !out.ends_with('\n') {
            out.push('\n');
        }
        Ok(out)
    }

    /// Atomically (w.r.t. concurrent readers on the same filesystem) write the
    /// lockfile to `path` via a temp file + rename. Creates parent directories
    /// as needed.
    ///
    /// Not crash-atomic — a power loss between the temp write and the rename
    /// can leave the temp file behind, and the Linux kernel does not fsync the
    /// parent directory on rename. For a lockfile this is acceptable: the
    /// next `wafer install` re-derives the entry.
    pub fn write_atomic(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
        }
        let body = self.to_toml_string()?;
        let tmp = tmp_sibling(path)?;
        fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
        fs::rename(&tmp, path)
            .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
        Ok(())
    }
}

/// Produce a sibling temp-file path, e.g. `wafer.lock` → `wafer.lock.tmp-<uuid>`.
/// Handles trailing slashes and produces an error for invalid paths.
fn tmp_sibling(path: &Path) -> Result<PathBuf> {
    let file_name = path.file_name().ok_or_else(|| {
        anyhow::anyhow!("invalid lockfile path (no file name): {}", path.display())
    })?;
    let mut name_os = file_name.to_os_string();
    name_os.push(format!(".tmp-{}", uuid::Uuid::new_v4()));
    Ok(path.with_file_name(name_os))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn pkg(name: &str, version: &str) -> LockfilePackage {
        LockfilePackage {
            name: name.into(),
            version: version.into(),
            sha256: "a".repeat(64),
            source: "registry+https://wafer.run".into(),
        }
    }

    #[test]
    fn empty_new_has_version_1() {
        let lf = Lockfile::new();
        assert_eq!(lf.version, 1);
        assert!(lf.packages.is_empty());
    }

    #[test]
    fn load_missing_file_returns_none() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("wafer.lock");
        assert!(Lockfile::load(&path).unwrap().is_none());
    }

    #[test]
    fn load_parses_valid_v1() {
        let body = r#"version = 1

[[package]]
name = "acme/widget"
version = "0.3.1"
sha256 = "abc"
source = "registry+https://wafer.run"
"#;
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("wafer.lock");
        fs::write(&path, body).unwrap();
        let lf = Lockfile::load(&path).unwrap().unwrap();
        assert_eq!(lf.packages.len(), 1);
        assert_eq!(lf.packages[0].name, "acme/widget");
    }

    #[test]
    fn load_rejects_unknown_version() {
        let body = "version = 999\n";
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("wafer.lock");
        fs::write(&path, body).unwrap();
        let err = Lockfile::load(&path).unwrap_err().to_string();
        assert!(err.contains("unsupported lockfile version 999"), "{err}");
    }

    #[test]
    fn load_propagates_parse_error_with_path() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("wafer.lock");
        fs::write(&path, "this is not toml = = =").unwrap();
        let err = format!("{:#}", Lockfile::load(&path).unwrap_err());
        assert!(err.contains("wafer.lock"), "{err}");
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

    #[test]
    fn to_toml_string_is_sorted_with_trailing_newline() {
        let mut lf = Lockfile::new();
        lf.insert_or_replace(pkg("b/b", "0.1.0"));
        lf.insert_or_replace(pkg("a/a", "0.1.0"));
        let s = lf.to_toml_string().unwrap();
        assert!(s.ends_with('\n'));
        let ia = s.find("\"a/a\"").unwrap();
        let ib = s.find("\"b/b\"").unwrap();
        assert!(ia < ib, "not sorted: {s}");
    }

    #[test]
    fn write_atomic_produces_readable_file() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("nested/wafer.lock");
        let mut lf = Lockfile::new();
        lf.insert_or_replace(pkg("acme/widget", "0.3.1"));
        lf.write_atomic(&path).unwrap();
        let reloaded = Lockfile::load(&path).unwrap().unwrap();
        assert_eq!(reloaded, lf);
    }

    #[test]
    fn roundtrip_is_idempotent() {
        let mut lf = Lockfile::new();
        lf.insert_or_replace(pkg("a/a", "0.1.0"));
        lf.insert_or_replace(pkg("b/b", "0.2.0"));
        let s1 = lf.to_toml_string().unwrap();
        let parsed: Lockfile = toml::from_str(&s1).unwrap();
        let s2 = parsed.to_toml_string().unwrap();
        assert_eq!(s1, s2);
    }

    #[test]
    fn load_rejects_missing_version_field() {
        let body =
            "[[package]]\nname = \"a/b\"\nversion = \"0.1.0\"\nsha256 = \"abc\"\nsource = \"x\"\n";
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("wafer.lock");
        fs::write(&path, body).unwrap();
        let err = format!("{:#}", Lockfile::load(&path).unwrap_err());
        // With Default gone, serde reports the missing field directly.
        assert!(
            err.contains("missing field") && err.contains("version"),
            "expected missing-version diagnostic, got: {err}"
        );
    }
}
