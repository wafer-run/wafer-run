use std::path::Path;

use anyhow::{bail, Context};
use sha2::{Digest, Sha256};

use crate::{manifest::Manifest, validate::validate_wasm};

/// Package the built WAFER block in `dir`.
///
/// Steps:
/// 1. Load manifest.json.
/// 2. Ensure `target/block.wasm` exists.
/// 3. Validate the WASM (required exports + `__wafer_info`).
/// 4. Compare info.name with manifest.name.
/// 5. Compute wasm_size and sha256 from the WASM bytes.
/// 6. Build capabilities from manifest.requires.
/// 7. Write `dist/block.wasm` and `dist/manifest.json` (enriched).
pub fn package(dir: &Path) -> anyhow::Result<()> {
    // -----------------------------------------------------------------------
    // 1. Load manifest.
    // -----------------------------------------------------------------------
    let manifest = Manifest::load(dir)
        .context("Failed to load manifest.json — run `wafer new` to create one")?;

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
    if info.name != manifest.name {
        bail!(
            "Block name mismatch: manifest says {:?} but __wafer_info() returned {:?}.\n\
             Make sure the `name` argument in `#[wafer_block(name = …)]` matches manifest.json.",
            manifest.name,
            info.name
        );
    }

    // -----------------------------------------------------------------------
    // 5. Read WASM bytes → compute size and sha256.
    // -----------------------------------------------------------------------
    let wasm_bytes = std::fs::read(&wasm_path)
        .with_context(|| format!("Failed to read {}", wasm_path.display()))?;

    let wasm_size = wasm_bytes.len() as u64;

    let hash = Sha256::digest(&wasm_bytes);
    let sha256 = hex::encode(hash);

    // -----------------------------------------------------------------------
    // 6. Build capabilities from manifest.requires.
    // -----------------------------------------------------------------------
    let capabilities = if manifest.requires.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::json!({
            "can_call_blocks": true,
            "allowed_blocks": manifest.requires,
        })
    };

    // -----------------------------------------------------------------------
    // 7. Build enriched manifest.
    // -----------------------------------------------------------------------
    let enriched = Manifest {
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        interface: manifest.interface.clone(),
        summary: manifest.summary.clone(),
        requires: manifest.requires,
        capabilities: Some(capabilities),
        wasm_size: Some(wasm_size),
        sha256: Some(sha256.clone()),
    };

    // -----------------------------------------------------------------------
    // 8. Create dist/ directory.
    // -----------------------------------------------------------------------
    let dist_dir = dir.join("dist");
    std::fs::create_dir_all(&dist_dir)
        .with_context(|| format!("Failed to create {}", dist_dir.display()))?;

    // -----------------------------------------------------------------------
    // 9. Copy target/block.wasm → dist/block.wasm.
    // -----------------------------------------------------------------------
    let dist_wasm = dist_dir.join("block.wasm");
    std::fs::copy(&wasm_path, &dist_wasm).with_context(|| {
        format!(
            "Failed to copy {} → {}",
            wasm_path.display(),
            dist_wasm.display()
        )
    })?;

    // -----------------------------------------------------------------------
    // 10. Write enriched manifest as dist/manifest.json.
    // -----------------------------------------------------------------------
    let dist_manifest = dist_dir.join("manifest.json");
    let json = serde_json::to_string_pretty(&enriched).context("Failed to serialize manifest")?;
    std::fs::write(&dist_manifest, json + "\n")
        .with_context(|| format!("Failed to write {}", dist_manifest.display()))?;

    // -----------------------------------------------------------------------
    // 11. Print summary.
    // -----------------------------------------------------------------------
    println!("Packaged  {} v{}", enriched.name, enriched.version);
    println!(
        "  size:   {} bytes ({:.1} KiB)",
        wasm_size,
        wasm_size as f64 / 1024.0
    );
    println!("  sha256: {sha256}");
    println!("  dist/block.wasm");
    println!("  dist/manifest.json");

    Ok(())
}
