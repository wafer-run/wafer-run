//! `wafer install <org>/<block>[@<ver>] --cache-only [--registry URL]`
//!
//! PR b ships only the `--cache-only` path. Full install (wafer.toml
//! mutation, argument-less install, `--frozen`) lands in PR c.

use std::path::PathBuf;

use anyhow::{bail, Result};

use crate::{
    cache::CacheRoot, commands::info::parse_target, install::install_cache_only, registry_client,
};

pub async fn run(target: Option<String>, cache_only: bool, registry: Option<String>) -> Result<()> {
    if !cache_only {
        bail!(
            "PR b only supports `wafer install <org>/<block>[@<ver>] --cache-only`. \
             Full install (wafer.toml mutation, --frozen, argument-less) lands in the next PR."
        );
    }
    let target = target.ok_or_else(|| {
        anyhow::anyhow!("wafer install --cache-only requires an <org>/<block>[@<ver>] argument")
    })?;
    let (org, block, ver) = parse_target(&target)?;

    // Honour spec §wafer install step 1: wafer.toml must exist to ground us
    // in a project directory.
    let wafer_toml = PathBuf::from("wafer.toml");
    if !wafer_toml.is_file() {
        bail!("wafer.toml not found in current directory");
    }

    let url = registry_client::resolve_registry(registry);
    let cache = CacheRoot::default_location()?;
    let lockfile_path = PathBuf::from("wafer.lock");

    let outcome =
        install_cache_only(&url, &cache, &lockfile_path, &org, &block, ver.as_deref()).await?;

    let source = if outcome.from_cache { " (cached)" } else { "" };
    println!(
        "\u{2714} installed {}/{}@{}{}",
        outcome.org, outcome.block, outcome.version, source
    );
    Ok(())
}
