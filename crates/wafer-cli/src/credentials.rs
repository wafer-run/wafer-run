use std::{collections::HashMap, fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{paths::wafer_home, registry_client::Registry};

#[derive(Serialize, Deserialize, Default)]
pub struct CredentialsFile {
    #[serde(default)]
    pub default: Option<Entry>,
    #[serde(default)]
    pub registries: HashMap<String, Entry>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    pub registry: String,
    pub token: String,
}

pub fn path() -> Result<PathBuf> {
    Ok(wafer_home()?.join("credentials.toml"))
}

pub fn load() -> Result<CredentialsFile> {
    let p = path()?;
    if !p.exists() {
        return Ok(CredentialsFile::default());
    }
    let s = fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
    let cf: CredentialsFile =
        toml::from_str(&s).with_context(|| format!("parse {}", p.display()))?;
    Ok(cf)
}

pub fn save(cf: &CredentialsFile) -> Result<()> {
    let p = path()?;
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    let s = toml::to_string_pretty(cf)?;
    fs::write(&p, s)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&p)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&p, perms)?;
    }
    Ok(())
}

pub fn upsert(cf: &mut CredentialsFile, name: Option<&str>, entry: Entry) {
    match name {
        None | Some("default") => cf.default = Some(entry),
        Some(n) => {
            cf.registries.insert(n.to_string(), entry);
        }
    }
}

pub fn resolve<'a>(cf: &'a CredentialsFile, registry_url: &str) -> Option<&'a Entry> {
    if let Some(d) = &cf.default {
        if d.registry == registry_url {
            return Some(d);
        }
    }
    cf.registries.values().find(|e| e.registry == registry_url)
}

/// Load the credentials file and resolve the entry for `registry`, or fail
/// with the canonical "no token" error. The one preamble shared by every
/// authenticated command (publish, yank, whoami, …).
pub fn require(registry: &Registry) -> Result<Entry> {
    let cf = load()?;
    resolve(&cf, registry.as_str())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("No token for {registry}. Run `wafer login` first."))
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use super::*;

    /// Serializes tests that mutate the process-wide `HOME` env var so parallel
    /// test threads don't race each other (one test reading `HOME` while
    /// another removes it). Mirrors the guard in `cache.rs`.
    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        static M: OnceLock<Mutex<()>> = OnceLock::new();
        M.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn roundtrip() {
        let _guard = env_guard();
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }

        let mut cf = CredentialsFile::default();
        upsert(
            &mut cf,
            None,
            Entry {
                registry: "https://wafer.run".into(),
                token: "wafer_pat_abc".into(),
            },
        );
        save(&cf).unwrap();

        let loaded = load().unwrap();
        assert_eq!(loaded.default.as_ref().unwrap().token, "wafer_pat_abc");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = path().unwrap();
            let m = std::fs::metadata(&p).unwrap();
            assert_eq!(m.permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn resolve_returns_none_when_url_not_present() {
        let cf = CredentialsFile {
            default: Some(Entry {
                registry: "https://wafer.run".into(),
                token: "token1".into(),
            }),
            registries: Default::default(),
        };
        let result = resolve(&cf, "http://no-such-registry");
        assert_eq!(result, None);
    }

    #[test]
    fn resolve_default_wins_over_named_on_url_collision() {
        let mut registries = std::collections::HashMap::new();
        registries.insert(
            "alt".into(),
            Entry {
                registry: "https://example.com".into(),
                token: "ALT".into(),
            },
        );
        let cf = CredentialsFile {
            default: Some(Entry {
                registry: "https://example.com".into(),
                token: "DEFAULT".into(),
            }),
            registries,
        };
        let result = resolve(&cf, "https://example.com");
        assert_eq!(result.map(|e| e.token.as_str()), Some("DEFAULT"));
    }

    #[test]
    fn require_returns_canonical_error_when_no_entry_matches() {
        let _guard = env_guard();
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }
        let err = require(&Registry::new("https://wafer.run"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("No token for https://wafer.run") && err.contains("wafer login"),
            "{err}"
        );
    }

    #[test]
    fn require_returns_matching_entry() {
        let _guard = env_guard();
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }
        let mut cf = CredentialsFile::default();
        upsert(
            &mut cf,
            None,
            Entry {
                registry: "https://wafer.run".into(),
                token: "wafer_pat_abc".into(),
            },
        );
        save(&cf).unwrap();
        let entry = require(&Registry::new("https://wafer.run/")).unwrap();
        assert_eq!(entry.token, "wafer_pat_abc");
    }
}
