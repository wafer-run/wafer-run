//! `wafer.toml` reader + `[dependencies]` mutator, preserving user
//! comments and formatting.
//!
//! `toml_edit::DocumentMut` is the editable DOM. Reads use it directly;
//! writes go through `insert_or_replace_dependency` which alphabetizes the
//! `[dependencies]` entries in-place and creates the header if missing.
//!
//! `wafer.toml` layout assumed by this module:
//!
//! ```toml
//! [package]
//! org = "mine"
//! name = "myblock"
//! version = "0.1.0"
//! abi = 1
//!
//! [dependencies]
//! "acme/widget" = "0.3.1"
//! ```
//!
//! The `[package]` section and anything above `[dependencies]` is
//! preserved verbatim.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use toml_edit::{DocumentMut, Item, Value};

/// An in-memory representation of `wafer.toml` that can be mutated and
/// written back with formatting preserved.
#[derive(Debug)]
pub struct WaferToml {
    doc: DocumentMut,
}

impl WaferToml {
    /// Read and parse `wafer.toml` from `path`. Missing file → error (unlike
    /// `Lockfile::load` which returns `Ok(None)`; a missing `wafer.toml`
    /// means "no project", which is never a recoverable state for install).
    pub fn read(path: &Path) -> Result<Self> {
        let body = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let doc: DocumentMut = body
            .parse()
            .with_context(|| format!("parse {}", path.display()))?;
        Ok(Self { doc })
    }

    /// Iterate `[dependencies]` as `(name, version)` pairs. Returns an
    /// empty vector if the table is absent. Values that aren't strings
    /// are skipped silently — v1 only supports exact-pin strings.
    pub fn dependencies(&self) -> Vec<(String, String)> {
        let Some(deps) = self.doc.get("dependencies").and_then(|i| i.as_table()) else {
            return Vec::new();
        };
        deps.iter()
            .filter_map(|(k, v)| match v {
                Item::Value(Value::String(s)) => Some((k.to_string(), s.value().to_string())),
                _ => None,
            })
            .collect()
    }

    /// Insert or replace a `[dependencies]."{name}" = "{version}"` entry.
    /// Creates the table if missing. After mutation, the table is
    /// alphabetized by key.
    pub fn insert_or_replace_dependency(&mut self, name: &str, version: &str) {
        if !self.doc.contains_table("dependencies") {
            self.doc["dependencies"] = toml_edit::table();
        }
        let deps = self.doc["dependencies"].as_table_mut().unwrap();
        deps.insert(name, toml_edit::value(version));
        deps.sort_values();
    }

    /// Remove a `[dependencies]."{name}"` entry if present. Returns true
    /// when an entry was removed.
    ///
    /// Not yet wired to a CLI command; kept as the symmetric counterpart
    /// to [`Self::insert_or_replace_dependency`] for future `wafer remove`.
    #[allow(dead_code)]
    pub fn remove_dependency(&mut self, name: &str) -> bool {
        let Some(table) = self
            .doc
            .get_mut("dependencies")
            .and_then(|i| i.as_table_mut())
        else {
            return false;
        };
        table.remove(name).is_some()
    }

    /// Write back to `path` atomically via temp-file + rename.
    pub fn write_atomic(&self, path: &Path) -> Result<()> {
        let body = self.doc.to_string();
        let tmp = tmp_sibling(path)?;
        fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
        fs::rename(&tmp, path)
            .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
        Ok(())
    }
}

fn tmp_sibling(path: &Path) -> Result<PathBuf> {
    let file_name = path.file_name().ok_or_else(|| {
        anyhow::anyhow!("invalid wafer.toml path (no file name): {}", path.display())
    })?;
    let mut name_os = file_name.to_os_string();
    name_os.push(format!(".tmp-{}", uuid::Uuid::new_v4()));
    Ok(path.with_file_name(name_os))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    const BASE: &str = r#"[package]
org = "mine"
name = "myblock"
version = "0.1.0"
abi = 1
"#;

    fn write(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
    }

    #[test]
    fn read_parses_valid() {
        let tmp = tempdir().unwrap();
        let p = tmp.path().join("wafer.toml");
        write(&p, BASE);
        let wt = WaferToml::read(&p).unwrap();
        assert!(wt.dependencies().is_empty());
    }

    #[test]
    fn read_missing_errors() {
        let tmp = tempdir().unwrap();
        let p = tmp.path().join("wafer.toml");
        assert!(WaferToml::read(&p).is_err());
    }

    #[test]
    fn read_parse_error_names_file() {
        let tmp = tempdir().unwrap();
        let p = tmp.path().join("wafer.toml");
        write(&p, "not = valid = = =");
        let err = format!("{:#}", WaferToml::read(&p).unwrap_err());
        assert!(err.contains("wafer.toml"), "{err}");
    }

    #[test]
    fn dependencies_lists_string_entries() {
        let body = format!(
            "{BASE}\n[dependencies]\n\"acme/widget\" = \"0.3.1\"\n\"zeta/z\" = \"1.0.0\"\n"
        );
        let tmp = tempdir().unwrap();
        let p = tmp.path().join("wafer.toml");
        write(&p, &body);
        let wt = WaferToml::read(&p).unwrap();
        let deps = wt.dependencies();
        assert_eq!(
            deps,
            vec![
                ("acme/widget".into(), "0.3.1".into()),
                ("zeta/z".into(), "1.0.0".into())
            ]
        );
    }

    #[test]
    fn dependencies_skips_non_string_values() {
        let body = format!(
            "{BASE}\n[dependencies]\n\"acme/widget\" = \"0.3.1\"\n\"weird/thing\" = {{ version = \"1.0.0\" }}\n"
        );
        let tmp = tempdir().unwrap();
        let p = tmp.path().join("wafer.toml");
        write(&p, &body);
        let wt = WaferToml::read(&p).unwrap();
        let deps = wt.dependencies();
        assert_eq!(deps, vec![("acme/widget".into(), "0.3.1".into())]);
    }

    #[test]
    fn insert_creates_dependencies_section() {
        let tmp = tempdir().unwrap();
        let p = tmp.path().join("wafer.toml");
        write(&p, BASE);
        let mut wt = WaferToml::read(&p).unwrap();
        wt.insert_or_replace_dependency("acme/widget", "0.3.1");
        wt.write_atomic(&p).unwrap();
        let reread = fs::read_to_string(&p).unwrap();
        assert!(reread.contains("[dependencies]"), "{reread}");
        assert!(reread.contains("\"acme/widget\" = \"0.3.1\""), "{reread}");
        assert!(reread.contains("[package]"), "{reread}");
        assert!(reread.contains("org = \"mine\""), "{reread}");
    }

    #[test]
    fn insert_keeps_alphabetical_order() {
        let tmp = tempdir().unwrap();
        let p = tmp.path().join("wafer.toml");
        write(&p, BASE);
        let mut wt = WaferToml::read(&p).unwrap();
        wt.insert_or_replace_dependency("zeta/z", "1.0.0");
        wt.insert_or_replace_dependency("acme/widget", "0.3.1");
        wt.insert_or_replace_dependency("mid/m", "2.0.0");
        wt.write_atomic(&p).unwrap();
        let body = fs::read_to_string(&p).unwrap();
        let a = body.find("\"acme/widget\"").unwrap();
        let m = body.find("\"mid/m\"").unwrap();
        let z = body.find("\"zeta/z\"").unwrap();
        assert!(a < m && m < z, "unsorted: {body}");
    }

    #[test]
    fn insert_replaces_existing_version() {
        let body = format!("{BASE}\n[dependencies]\n\"acme/widget\" = \"0.1.0\"\n");
        let tmp = tempdir().unwrap();
        let p = tmp.path().join("wafer.toml");
        write(&p, &body);
        let mut wt = WaferToml::read(&p).unwrap();
        wt.insert_or_replace_dependency("acme/widget", "0.3.1");
        wt.write_atomic(&p).unwrap();
        let reread = fs::read_to_string(&p).unwrap();
        assert!(reread.contains("\"acme/widget\" = \"0.3.1\""), "{reread}");
        assert!(!reread.contains("\"acme/widget\" = \"0.1.0\""), "{reread}");
    }

    #[test]
    fn insert_preserves_user_comments() {
        let body = format!(
            "# my cool block\n\n{BASE}\n# pinned for a reason\n[dependencies]\n\"acme/widget\" = \"0.3.1\"\n"
        );
        let tmp = tempdir().unwrap();
        let p = tmp.path().join("wafer.toml");
        write(&p, &body);
        let mut wt = WaferToml::read(&p).unwrap();
        wt.insert_or_replace_dependency("zeta/z", "1.0.0");
        wt.write_atomic(&p).unwrap();
        let reread = fs::read_to_string(&p).unwrap();
        assert!(reread.contains("# my cool block"), "{reread}");
        assert!(reread.contains("# pinned for a reason"), "{reread}");
    }

    #[test]
    fn remove_dependency_drops_entry() {
        let body = format!(
            "{BASE}\n[dependencies]\n\"acme/widget\" = \"0.3.1\"\n\"zeta/z\" = \"1.0.0\"\n"
        );
        let tmp = tempdir().unwrap();
        let p = tmp.path().join("wafer.toml");
        write(&p, &body);
        let mut wt = WaferToml::read(&p).unwrap();
        assert!(wt.remove_dependency("acme/widget"));
        assert!(!wt.remove_dependency("nope/not-there"));
        wt.write_atomic(&p).unwrap();
        let reread = fs::read_to_string(&p).unwrap();
        assert!(!reread.contains("acme/widget"), "{reread}");
        assert!(reread.contains("zeta/z"), "{reread}");
    }
}
