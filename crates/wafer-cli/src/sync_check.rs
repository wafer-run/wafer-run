//! Lockfile ↔ `wafer.toml` sync check.
//!
//! A `wafer.toml` + `wafer.lock` pair is "in sync" when:
//! 1. Every `[dependencies]."{name}" = "{version}"` entry has a matching
//!    `[[package]]` with the same `name` AND `version`.
//! 2. Every `[[package]]` corresponds to a `[dependencies]` entry (no
//!    orphans in the lockfile).
//!
//! Drift surfaces as a `DriftError` listing the specific offending keys.

use std::collections::{BTreeMap, BTreeSet};

use crate::{lockfile::Lockfile, wafer_toml::WaferToml};

#[derive(Debug, thiserror::Error)]
pub enum DriftError {
    #[error("wafer.toml and wafer.lock are out of sync:\n{0}")]
    OutOfSync(String),
}

/// Compare `wafer.toml` [dependencies] against `wafer.lock` packages.
/// Returns `Ok(())` when they agree; `Err(DriftError)` otherwise.
pub fn check(toml: &WaferToml, lock: &Lockfile) -> Result<(), DriftError> {
    let manifest: BTreeMap<String, String> = toml.dependencies().into_iter().collect();
    let lockmap: BTreeMap<String, String> = lock
        .packages
        .iter()
        .map(|p| (p.name.clone(), p.version.clone()))
        .collect();

    let manifest_keys: BTreeSet<&String> = manifest.keys().collect();
    let lock_keys: BTreeSet<&String> = lockmap.keys().collect();

    let missing_in_lock: Vec<&String> = manifest_keys.difference(&lock_keys).copied().collect();
    let orphan_in_lock: Vec<&String> = lock_keys.difference(&manifest_keys).copied().collect();
    let version_mismatch: Vec<(&String, &String, &String)> = manifest_keys
        .intersection(&lock_keys)
        .copied()
        .filter_map(|k| {
            let m = &manifest[k];
            let l = &lockmap[k];
            if m != l {
                Some((k, m, l))
            } else {
                None
            }
        })
        .collect();

    if missing_in_lock.is_empty() && orphan_in_lock.is_empty() && version_mismatch.is_empty() {
        return Ok(());
    }

    let mut msg = String::new();
    for k in &missing_in_lock {
        msg.push_str(&format!(
            "  - {k}: in wafer.toml but missing from wafer.lock\n"
        ));
    }
    for k in &orphan_in_lock {
        msg.push_str(&format!("  - {k}: in wafer.lock but not in wafer.toml\n"));
    }
    for (k, m, l) in &version_mismatch {
        msg.push_str(&format!(
            "  - {k}: wafer.toml pins {m}, wafer.lock has {l}\n"
        ));
    }
    Err(DriftError::OutOfSync(msg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lockfile::LockfilePackage;
    use std::fs;
    use tempfile::TempDir;

    fn mk_pkg(name: &str, version: &str) -> LockfilePackage {
        LockfilePackage {
            name: name.into(),
            version: version.into(),
            sha256: "a".repeat(64),
            source: "registry+https://wafer.run".into(),
        }
    }

    /// Return both the WaferToml AND its backing TempDir, so the caller can
    /// keep the tempdir alive for the duration of the test.
    fn mk_toml(body: &str) -> (WaferToml, TempDir) {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("wafer.toml");
        fs::write(&p, body).unwrap();
        let wt = WaferToml::read(&p).unwrap();
        (wt, tmp)
    }

    const HEADER: &str = "[package]\norg=\"me\"\nname=\"me\"\nversion=\"0.0.1\"\nabi=1\n";

    #[test]
    fn empty_and_empty_is_in_sync() {
        let (toml, _tmp) = mk_toml(HEADER);
        let lock = Lockfile::new();
        check(&toml, &lock).unwrap();
    }

    #[test]
    fn matching_pair_is_in_sync() {
        let (toml, _tmp) = mk_toml(&format!(
            "{HEADER}\n[dependencies]\n\"acme/widget\" = \"0.3.1\"\n"
        ));
        let mut lock = Lockfile::new();
        lock.insert_or_replace(mk_pkg("acme/widget", "0.3.1"));
        check(&toml, &lock).unwrap();
    }

    #[test]
    fn missing_lock_entry_reports_key() {
        let (toml, _tmp) = mk_toml(&format!(
            "{HEADER}\n[dependencies]\n\"acme/widget\" = \"0.3.1\"\n"
        ));
        let lock = Lockfile::new();
        let err = check(&toml, &lock).unwrap_err().to_string();
        assert!(err.contains("acme/widget"), "{err}");
        assert!(err.contains("missing from wafer.lock"), "{err}");
    }

    #[test]
    fn orphan_lock_entry_reports_key() {
        let (toml, _tmp) = mk_toml(HEADER);
        let mut lock = Lockfile::new();
        lock.insert_or_replace(mk_pkg("acme/widget", "0.3.1"));
        let err = check(&toml, &lock).unwrap_err().to_string();
        assert!(err.contains("acme/widget"), "{err}");
        assert!(err.contains("not in wafer.toml"), "{err}");
    }

    #[test]
    fn version_mismatch_reports_both() {
        let (toml, _tmp) = mk_toml(&format!(
            "{HEADER}\n[dependencies]\n\"acme/widget\" = \"0.3.1\"\n"
        ));
        let mut lock = Lockfile::new();
        lock.insert_or_replace(mk_pkg("acme/widget", "0.2.0"));
        let err = check(&toml, &lock).unwrap_err().to_string();
        assert!(err.contains("0.3.1"), "{err}");
        assert!(err.contains("0.2.0"), "{err}");
    }

    #[test]
    fn reports_all_drifts_in_one_error() {
        let (toml, _tmp) = mk_toml(&format!(
            "{HEADER}\n[dependencies]\n\"a/a\" = \"1.0.0\"\n\"b/b\" = \"2.0.0\"\n"
        ));
        let mut lock = Lockfile::new();
        lock.insert_or_replace(mk_pkg("b/b", "1.0.0")); // version mismatch
        lock.insert_or_replace(mk_pkg("c/c", "3.0.0")); // orphan
                                                        // a/a missing from lock.
        let err = check(&toml, &lock).unwrap_err().to_string();
        assert!(err.contains("a/a"), "{err}");
        assert!(err.contains("b/b"), "{err}");
        assert!(err.contains("c/c"), "{err}");
    }
}
