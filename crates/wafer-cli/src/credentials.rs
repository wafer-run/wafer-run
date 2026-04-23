use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs, path::PathBuf};

#[derive(Serialize, Deserialize, Default)]
pub struct CredentialsFile {
    #[serde(default)]
    pub default: Option<Entry>,
    #[serde(default)]
    pub registries: HashMap<String, Entry>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Entry {
    pub registry: String,
    pub token: String,
}

pub fn path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME env var required")?;
    Ok(PathBuf::from(home).join(".wafer").join("credentials.toml"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
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
}
