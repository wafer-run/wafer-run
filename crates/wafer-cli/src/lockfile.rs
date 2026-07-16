//! `wafer.lock` TOML parsing/serialization + atomic file IO.
//!
//! The schema types ([`Lockfile`], [`LockfilePackage`], the schema-version
//! constant, and the sorted-by-name invariant) live in
//! [`wafer_block::lockfile`] — shared with wafer-run's registry loader so
//! the on-disk contract is defined exactly once. This module owns the CLI
//! side: TOML (de)serialization, the schema-version gate on load, and the
//! atomic write. See the shared module's docs for the format specification.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use wafer_block::lockfile::SCHEMA_VERSION;
pub use wafer_block::lockfile::{Lockfile, LockfilePackage};

/// TOML parsing/serialization + atomic file IO for [`Lockfile`].
///
/// Extension trait because the schema type is defined in
/// `wafer_block::lockfile` (shared with wafer-run); the TOML + filesystem
/// side is CLI-owned and must not leak into the wasm32-clean types crate.
pub trait LockfileToml: Sized {
    /// Read + parse `wafer.lock` from `path`. Missing file yields `Ok(None)`
    /// so callers can distinguish "no lockfile yet" from "parse error".
    fn load(path: &Path) -> Result<Option<Self>>;

    /// Serialize to canonical TOML text. Packages emitted in sorted order,
    /// trailing newline.
    fn to_toml_string(&self) -> Result<String>;

    /// Atomically (w.r.t. concurrent readers on the same filesystem) write the
    /// lockfile to `path` via a temp file + rename. Creates parent directories
    /// as needed.
    ///
    /// Not crash-atomic — a power loss between the temp write and the rename
    /// can leave the temp file behind, and the Linux kernel does not fsync the
    /// parent directory on rename. For a lockfile this is acceptable: the
    /// next `wafer install` re-derives the entry.
    fn write_atomic(&self, path: &Path) -> Result<()>;
}

impl LockfileToml for Lockfile {
    fn load(path: &Path) -> Result<Option<Self>> {
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

    fn to_toml_string(&self) -> Result<String> {
        let mut ordered = self.clone();
        ordered.packages.sort_by(|a, b| a.name.cmp(&b.name));
        let mut out = toml::to_string(&ordered).context("serialise lockfile")?;
        if !out.ends_with('\n') {
            out.push('\n');
        }
        Ok(out)
    }

    fn write_atomic(&self, path: &Path) -> Result<()> {
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
    use tempfile::tempdir;

    use super::*;

    fn pkg(name: &str, version: &str) -> LockfilePackage {
        LockfilePackage {
            name: name.into(),
            version: version.into(),
            sha256: "a".repeat(64),
            wasm_sha256: "b".repeat(64),
            source: "registry+https://wafer.run".into(),
        }
    }

    #[test]
    fn load_missing_file_returns_none() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("wafer.lock");
        assert!(Lockfile::load(&path).unwrap().is_none());
    }

    #[test]
    fn load_parses_valid_v2() {
        let body = r#"version = 2

[[package]]
name = "acme/widget"
version = "0.3.1"
sha256 = "abc"
wasm_sha256 = "def"
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
            "[[package]]\nname = \"a/b\"\nversion = \"0.1.0\"\nsha256 = \"abc\"\nwasm_sha256 = \"def\"\nsource = \"x\"\n";
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("wafer.lock");
        fs::write(&path, body).unwrap();
        let err = format!("{:#}", Lockfile::load(&path).unwrap_err());
        // The shared schema keeps `version` non-defaulted, so serde reports
        // the missing field directly.
        assert!(
            err.contains("missing field") && err.contains("version"),
            "expected missing-version diagnostic, got: {err}"
        );
    }

    /// Cross-consumer contract pin: the exact TOML the CLI serializer emits
    /// is part of the shared on-disk contract. Pinning the bytes here locks
    /// the `[[package]]` section name and every field name, and proves the
    /// pinned text decodes identically through the shared
    /// `wafer_block::lockfile` types — the same types wafer-run's registry
    /// loader deserializes with. If this test breaks, the lockfile format
    /// changed: bump `SCHEMA_VERSION` and update both consumers.
    #[test]
    fn serialized_fixture_pins_shared_schema_contract() {
        let mut lf = Lockfile::new();
        lf.insert_or_replace(LockfilePackage {
            name: "acme/widget".into(),
            version: "0.3.1".into(),
            sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
            wasm_sha256: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".into(),
            source: "registry+https://wafer.run".into(),
        });
        lf.insert_or_replace(LockfilePackage {
            name: "my-org/auth".into(),
            version: "1.2.0".into(),
            sha256: "a".repeat(64),
            wasm_sha256: "c".repeat(64),
            source: "path+./local".into(),
        });

        let expected = format!(
            r#"version = 2

[[package]]
name = "acme/widget"
version = "0.3.1"
sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
wasm_sha256 = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
source = "registry+https://wafer.run"

[[package]]
name = "my-org/auth"
version = "1.2.0"
sha256 = "{}"
wasm_sha256 = "{}"
source = "path+./local"
"#,
            "a".repeat(64),
            "c".repeat(64)
        );
        let serialized = lf.to_toml_string().unwrap();
        assert_eq!(serialized, expected, "on-disk contract drifted");

        // Decode the pinned text through the shared types (the wafer-run
        // reader path) and require structural equality with the original.
        let decoded: wafer_block::lockfile::Lockfile = toml::from_str(&expected).unwrap();
        assert_eq!(decoded, lf);
        assert_eq!(decoded.version, wafer_block::lockfile::SCHEMA_VERSION);
    }
}
