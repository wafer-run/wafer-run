//! Resolution of the `~/.wafer` directory — the single source for every
//! path under it (credentials file, package cache), so all consumers agree
//! on one home-directory resolution.

use std::path::PathBuf;

use anyhow::{Context, Result};

/// `~/.wafer`, resolved via the `dirs` crate (not the raw `HOME` env var)
/// so the resolution also works on platforms where `HOME` is unset (e.g.
/// Windows).
pub fn wafer_home() -> Result<PathBuf> {
    let home = dirs::home_dir().context("no home directory; cannot locate ~/.wafer")?;
    Ok(home.join(".wafer"))
}
