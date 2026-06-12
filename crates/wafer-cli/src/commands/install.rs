//! `wafer install [<target>] [--cache-only] [--frozen] [--registry URL]`
//!
//! Invocation matrix:
//!
//! | target | --cache-only | --frozen | mode                     |
//! |--------|--------------|----------|--------------------------|
//! | Some   | false        | false    | Full install             |
//! | Some   | true         | false    | Cache-only (PR b)        |
//! | None   | false        | false    | From manifest            |
//! | None   | false        | true     | Frozen install           |
//!
//! Other combinations are rejected up front.

use std::path::PathBuf;

use anyhow::{bail, Result};

use crate::{
    block_name::parse_target,
    cache::CacheRoot,
    install::{install_cache_only, install_from_manifest, install_full},
    registry_client,
};

pub async fn run(
    target: Option<String>,
    cache_only: bool,
    frozen: bool,
    registry: Option<String>,
) -> Result<()> {
    // Flag validation.
    if cache_only && frozen {
        bail!("--cache-only and --frozen are mutually exclusive");
    }
    if frozen && target.is_some() {
        bail!("--frozen does not accept a target; run `wafer install --frozen` with no argument");
    }
    if cache_only && target.is_none() {
        bail!("wafer install --cache-only requires an <org>/<block>[@<ver>] argument");
    }

    // Spec §wafer install step 1: wafer.toml must exist.
    let wafer_toml_path = PathBuf::from("wafer.toml");
    if !wafer_toml_path.is_file() {
        bail!("wafer.toml not found in current directory");
    }

    let url = registry_client::resolve_registry(registry);
    let cache = CacheRoot::default_location()?;
    let lockfile_path = PathBuf::from("wafer.lock");

    match (target, cache_only, frozen) {
        (Some(target), true, false) => {
            // Cache-only single package (PR b).
            let (org, block, ver) = parse_target(&target)?;
            let outcome =
                install_cache_only(&url, &cache, &lockfile_path, &org, &block, ver.as_deref())
                    .await?;
            let source = if outcome.from_cache { " (cached)" } else { "" };
            println!(
                "\u{2714} installed {}/{}@{}{}",
                outcome.org, outcome.block, outcome.version, source
            );
        }
        (Some(target), false, false) => {
            // Full install: single package + wafer.toml mutation.
            let (org, block, ver) = parse_target(&target)?;
            let outcome = install_full(
                &url,
                &cache,
                &lockfile_path,
                &wafer_toml_path,
                &org,
                &block,
                ver.as_deref(),
            )
            .await?;
            let source = if outcome.from_cache { " (cached)" } else { "" };
            println!(
                "\u{2714} installed {}/{}@{}{}",
                outcome.org, outcome.block, outcome.version, source
            );
        }
        (None, false, _) => {
            // From manifest — frozen or not.
            let outcomes =
                install_from_manifest(&url, &cache, &wafer_toml_path, &lockfile_path, frozen)
                    .await?;
            for o in &outcomes {
                let source = if o.from_cache { " (cached)" } else { "" };
                println!(
                    "\u{2714} installed {}/{}@{}{}",
                    o.org, o.block, o.version, source
                );
            }
        }
        _ => unreachable!("flag validation above handles all invalid cases"),
    }

    Ok(())
}
