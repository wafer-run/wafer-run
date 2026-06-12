//! `wafer package` — build the publishable `.wafer` gzipped tarball.
//!
//! The tarball layout is exactly what the registry server's publish endpoint
//! validates (`site` repo, `blocks/registry/tarball.rs`):
//!
//! - `wafer.toml` — the package manifest (copied verbatim).
//! - `block.wasm` — the built WASM module (exactly one `.wasm` entry).
//! - `README.md` — optional, rendered on the registry detail page.
//!
//! The output path `target/wafer/{name}-{version}.wafer` is the default
//! `wafer publish` looks for, so `new → build → package → publish` works
//! without flags.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use flate2::{write::GzEncoder, Compression};
use sha2::{Digest, Sha256};

use crate::{validate::validate_wasm, wafer_toml::WaferToml};

/// Default tarball output path for a package `name` + `version`, relative
/// to the project dir. Shared by `wafer package` (writer) and
/// `wafer publish` (reader) so the two can never drift.
pub fn tarball_path(dir: &Path, name: &str, version: &str) -> PathBuf {
    dir.join("target")
        .join("wafer")
        .join(format!("{name}-{version}.wafer"))
}

/// Package the built WAFER block in `dir`.
///
/// Steps:
/// 1. Load `wafer.toml` and its `[package]` identity.
/// 2. Ensure `target/block.wasm` exists.
/// 3. Validate the WASM (required exports + `__wafer_info`).
/// 4. Compare info.name with `[package]` `{org}/{name}`.
/// 5. Write the gzipped tarball (wafer.toml + block.wasm + optional
///    README.md) to `target/wafer/{name}-{version}.wafer`.
pub fn package(dir: &Path) -> anyhow::Result<()> {
    // -----------------------------------------------------------------------
    // 1. Load wafer.toml [package].
    // -----------------------------------------------------------------------
    let wafer_toml_path = dir.join("wafer.toml");
    let pkg = WaferToml::read(&wafer_toml_path)
        .context("Failed to read wafer.toml — run `wafer new` to create one")?
        .package()?;
    let full_name = pkg.full_name();

    // -----------------------------------------------------------------------
    // 2. Check target/block.wasm exists.
    // -----------------------------------------------------------------------
    let wasm_path = dir.join("target").join("block.wasm");
    if !wasm_path.exists() {
        bail!(
            "target/block.wasm not found — run `wafer build` first.\n\
             Expected: {}",
            wasm_path.display()
        );
    }

    // -----------------------------------------------------------------------
    // 3. Validate WASM.
    // -----------------------------------------------------------------------
    println!("Validating {}…", wasm_path.display());
    let info = validate_wasm(&wasm_path).context("WASM validation failed")?;

    // -----------------------------------------------------------------------
    // 4. Compare names.
    // -----------------------------------------------------------------------
    if info.name != full_name {
        bail!(
            "Block name mismatch: wafer.toml says {:?} but __wafer_info() returned {:?}.\n\
             Make sure the `name` argument in `#[wafer_block(name = …)]` matches \
             wafer.toml's [package] org/name.",
            full_name,
            info.name
        );
    }

    // -----------------------------------------------------------------------
    // 5. Assemble the gzipped tarball.
    // -----------------------------------------------------------------------
    let wafer_toml_bytes = std::fs::read(&wafer_toml_path)
        .with_context(|| format!("Failed to read {}", wafer_toml_path.display()))?;
    let wasm_bytes = std::fs::read(&wasm_path)
        .with_context(|| format!("Failed to read {}", wasm_path.display()))?;
    let readme_path = dir.join("README.md");
    let readme_bytes = if readme_path.is_file() {
        Some(
            std::fs::read(&readme_path)
                .with_context(|| format!("Failed to read {}", readme_path.display()))?,
        )
    } else {
        None
    };

    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut tar = tar::Builder::new(&mut gz);
        append_file(&mut tar, "wafer.toml", &wafer_toml_bytes)?;
        append_file(&mut tar, "block.wasm", &wasm_bytes)?;
        if let Some(readme) = &readme_bytes {
            append_file(&mut tar, "README.md", readme)?;
        }
        tar.finish().context("Failed to finish tarball")?;
    }
    let tarball = gz.finish().context("Failed to gzip tarball")?;

    let out_path = tarball_path(dir, &pkg.name, &pkg.version);
    let out_dir = out_path.parent().expect("tarball_path always has a parent");
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("Failed to create {}", out_dir.display()))?;
    std::fs::write(&out_path, &tarball)
        .with_context(|| format!("Failed to write {}", out_path.display()))?;

    // -----------------------------------------------------------------------
    // 6. Print summary. sha256 is over the gzipped bytes — the same value
    //    the registry stores and `wafer install` verifies.
    // -----------------------------------------------------------------------
    let sha256 = hex::encode(Sha256::digest(&tarball));
    println!("Packaged  {} v{} (abi {})", full_name, pkg.version, pkg.abi);
    if let Some(summary) = &pkg.summary {
        println!("  summary: {summary}");
    }
    if let Some(license) = &pkg.license {
        println!("  license: {license}");
    }
    println!(
        "  wasm:    {} bytes ({:.1} KiB)",
        wasm_bytes.len(),
        wasm_bytes.len() as f64 / 1024.0
    );
    println!(
        "  tarball: {} bytes ({:.1} KiB)",
        tarball.len(),
        tarball.len() as f64 / 1024.0
    );
    println!("  sha256:  {sha256}");
    println!(
        "  {}",
        out_path.strip_prefix(dir).unwrap_or(&out_path).display()
    );

    Ok(())
}

/// Append one in-memory file to the tar with deterministic metadata.
fn append_file<W: std::io::Write>(
    tar: &mut tar::Builder<W>,
    name: &str,
    bytes: &[u8],
) -> anyhow::Result<()> {
    let mut header = tar::Header::new_gnu();
    header
        .set_path(name)
        .with_context(|| format!("Failed to set tar path {name:?}"))?;
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_cksum();
    tar.append(&header, bytes)
        .with_context(|| format!("Failed to append {name} to tarball"))
}
