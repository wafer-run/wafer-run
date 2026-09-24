//! `~/.wafer/credentials.toml`: one registry token per normalized registry
//! URL.
//!
//! On disk:
//!
//! ```toml
//! [tokens]
//! "https://wafer.run" = "wafer_pat_…"
//! "https://staging.example" = "wafer_pat_…"
//! ```
//!
//! [`load`] also reads the older layout (a `[default]` entry plus name-keyed
//! `[registries.<name>]` entries, each carrying its own `registry` URL) and
//! folds it into `tokens`; [`save`] writes only `tokens`.

use std::{collections::BTreeMap, fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{paths::wafer_home, registry_client::Registry};

/// The tokens the CLI holds, keyed by [`Registry::as_str`].
#[derive(Serialize, Default, Debug, PartialEq)]
pub struct CredentialsFile {
    tokens: BTreeMap<String, String>,
}

impl CredentialsFile {
    /// The token stored for `registry`, if any.
    pub fn token(&self, registry: &Registry) -> Option<&str> {
        self.tokens.get(registry.as_str()).map(String::as_str)
    }

    /// Store `token` for `registry`, replacing only that registry's token.
    pub fn set_token(&mut self, registry: &Registry, token: String) {
        self.tokens.insert(registry.as_str().to_string(), token);
    }

    /// Drop the token for `registry`; `true` when one was stored.
    pub fn remove(&mut self, registry: &Registry) -> bool {
        self.tokens.remove(registry.as_str()).is_some()
    }
}

/// Every layout [`load`] accepts. The legacy fields are read, converted by
/// [`CredentialsFile::from`], and never written back.
#[derive(Deserialize)]
struct StoredCredentials {
    #[serde(default)]
    tokens: BTreeMap<String, String>,
    #[serde(default)]
    default: Option<LegacyEntry>,
    #[serde(default)]
    registries: BTreeMap<String, LegacyEntry>,
}

#[derive(Deserialize)]
struct LegacyEntry {
    registry: String,
    token: String,
}

impl From<StoredCredentials> for CredentialsFile {
    fn from(stored: StoredCredentials) -> Self {
        let mut cf = CredentialsFile::default();
        // Precedence, lowest first: the legacy layout resolved `default`
        // ahead of the named entries, and `tokens` is the current layout.
        let legacy = stored.registries.into_values().chain(stored.default);
        for entry in legacy {
            cf.set_token(&Registry::new(&entry.registry), entry.token);
        }
        for (registry, token) in stored.tokens {
            cf.set_token(&Registry::new(&registry), token);
        }
        cf
    }
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
    let stored: StoredCredentials =
        toml::from_str(&s).with_context(|| format!("parse {}", p.display()))?;
    Ok(stored.into())
}

pub fn save(cf: &CredentialsFile) -> Result<()> {
    let p = path()?;
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    let s = toml::to_string_pretty(cf)?;

    // SEC-11: write the token file atomically and 0600-from-creation. The old
    // path — `fs::write` then `set_permissions(0600)` — left a window in which
    // a freshly created file inherited a permissive umask (world-readable
    // token), and the in-place truncate was not crash-atomic. Instead: create a
    // temp sibling with mode 0600, write + fsync it, then rename over the
    // target. The token is never visible under weak permissions, and a reader
    // sees either the old file or the new one, never a partial write.
    let tmp = p.with_extension("toml.tmp");

    #[cfg(unix)]
    {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        // Remove any stale temp from a previous crashed write before create_new.
        let _ = fs::remove_file(&tmp);
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(s.as_bytes())
            .with_context(|| format!("write {}", tmp.display()))?;
        f.sync_all()
            .with_context(|| format!("sync {}", tmp.display()))?;
    }
    #[cfg(not(unix))]
    {
        fs::write(&tmp, &s).with_context(|| format!("write {}", tmp.display()))?;
    }

    fs::rename(&tmp, &p).with_context(|| format!("rename {} -> {}", tmp.display(), p.display()))?;
    Ok(())
}

/// Load the credentials file and return the token for `registry`, or fail
/// with the canonical "no token" error. The one preamble shared by every
/// authenticated command (publish, yank, whoami, …).
pub fn require(registry: &Registry) -> Result<String> {
    load()?
        .token(registry)
        .map(str::to_string)
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

    fn fake_home() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }
        tmp
    }

    fn write_raw(contents: &str) {
        let p = path().unwrap();
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, contents).unwrap();
    }

    const WAFER: &str = "https://wafer.run";
    const STAGING: &str = "https://staging.example";

    #[test]
    fn roundtrip() {
        let _guard = env_guard();
        let _home = fake_home();

        let mut cf = CredentialsFile::default();
        cf.set_token(&Registry::new(WAFER), "wafer_pat_abc".into());
        save(&cf).unwrap();

        let loaded = load().unwrap();
        assert_eq!(loaded.token(&Registry::new(WAFER)), Some("wafer_pat_abc"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = path().unwrap();
            let m = std::fs::metadata(&p).unwrap();
            assert_eq!(m.permissions().mode() & 0o777, 0o600);
        }
    }

    // SEC-11: saving over an existing token file is atomic (temp sibling +
    // rename) and leaves no temp file behind; the result stays 0600.
    #[test]
    fn save_overwrites_atomically_and_leaves_no_tmp() {
        let _guard = env_guard();
        let _home = fake_home();

        let mut cf = CredentialsFile::default();
        cf.set_token(&Registry::new(WAFER), "first".into());
        save(&cf).unwrap();

        cf.set_token(&Registry::new(WAFER), "second".into());
        save(&cf).unwrap();

        assert_eq!(load().unwrap().token(&Registry::new(WAFER)), Some("second"));

        let p = path().unwrap();
        assert!(
            !p.with_extension("toml.tmp").exists(),
            "temp file must be renamed away, not left behind"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn set_token_for_one_registry_keeps_the_others() {
        let _guard = env_guard();
        let _home = fake_home();

        let mut cf = load().unwrap();
        cf.set_token(&Registry::new(WAFER), "WAFER".into());
        save(&cf).unwrap();
        let mut cf = load().unwrap();
        cf.set_token(&Registry::new(STAGING), "STAGING".into());
        save(&cf).unwrap();

        let cf = load().unwrap();
        assert_eq!(cf.token(&Registry::new(WAFER)), Some("WAFER"));
        assert_eq!(cf.token(&Registry::new(STAGING)), Some("STAGING"));
    }

    #[test]
    fn token_is_none_when_url_not_present() {
        let mut cf = CredentialsFile::default();
        cf.set_token(&Registry::new(WAFER), "token1".into());
        assert_eq!(cf.token(&Registry::new("http://no-such-registry")), None);
    }

    #[test]
    fn remove_drops_only_that_registry() {
        let mut cf = CredentialsFile::default();
        cf.set_token(&Registry::new(WAFER), "WAFER".into());
        cf.set_token(&Registry::new(STAGING), "STAGING".into());
        assert!(cf.remove(&Registry::new(STAGING)));
        assert!(!cf.remove(&Registry::new(STAGING)));
        assert_eq!(cf.token(&Registry::new(WAFER)), Some("WAFER"));
    }

    #[test]
    fn legacy_layout_is_read_and_rewritten_in_the_current_layout() {
        let _guard = env_guard();
        let _home = fake_home();
        write_raw(&format!(
            "[default]\nregistry = \"{WAFER}/\"\ntoken = \"DEFAULT\"\n\n\
             [registries.staging]\nregistry = \"{STAGING}\"\ntoken = \"STAGING\"\n"
        ));

        let mut cf = load().unwrap();
        assert_eq!(cf.token(&Registry::new(WAFER)), Some("DEFAULT"));
        assert_eq!(cf.token(&Registry::new(STAGING)), Some("STAGING"));

        cf.set_token(&Registry::new("https://other.example"), "OTHER".into());
        save(&cf).unwrap();

        let raw = fs::read_to_string(path().unwrap()).unwrap();
        let written: toml::Table = toml::from_str(&raw).unwrap();
        assert_eq!(
            written.keys().collect::<Vec<_>>(),
            ["tokens"],
            "only the current layout is written: {raw}"
        );
        let cf = load().unwrap();
        assert_eq!(cf.token(&Registry::new(WAFER)), Some("DEFAULT"));
        assert_eq!(cf.token(&Registry::new(STAGING)), Some("STAGING"));
        assert_eq!(
            cf.token(&Registry::new("https://other.example")),
            Some("OTHER")
        );
    }

    #[test]
    fn legacy_default_wins_over_named_on_url_collision() {
        let _guard = env_guard();
        let _home = fake_home();
        write_raw(&format!(
            "[default]\nregistry = \"{WAFER}\"\ntoken = \"DEFAULT\"\n\n\
             [registries.alt]\nregistry = \"{WAFER}\"\ntoken = \"ALT\"\n"
        ));
        assert_eq!(
            load().unwrap().token(&Registry::new(WAFER)),
            Some("DEFAULT")
        );
    }

    #[test]
    fn require_returns_canonical_error_when_no_entry_matches() {
        let _guard = env_guard();
        let _home = fake_home();
        let err = require(&Registry::new(WAFER)).unwrap_err().to_string();
        assert!(
            err.contains("No token for https://wafer.run") && err.contains("wafer login"),
            "{err}"
        );
    }

    #[test]
    fn require_returns_matching_token() {
        let _guard = env_guard();
        let _home = fake_home();
        let mut cf = CredentialsFile::default();
        cf.set_token(&Registry::new(WAFER), "wafer_pat_abc".into());
        save(&cf).unwrap();
        let token = require(&Registry::new("https://wafer.run/")).unwrap();
        assert_eq!(token, "wafer_pat_abc");
    }
}
