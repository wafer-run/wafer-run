use std::{path::Path, process::Command};

use anyhow::{bail, Context};

use crate::{
    detect::{detect_language, Lang},
    validate::validate_wasm,
    wafer_toml::WaferToml,
};

/// Size threshold (in bytes) above which we warn the user.
const WASM_SIZE_WARN_BYTES: u64 = 16 * 1024 * 1024; // 16 MiB

/// Build the WAFER block in `dir`.
///
/// Steps:
/// 1. Load `wafer.toml` and its `[package]` identity.
/// 2. Auto-detect the project language.
/// 3. Run the appropriate toolchain to produce a `.wasm` binary.
/// 4. Copy the output to `<dir>/target/block.wasm`.
/// 5. Validate the WASM (required exports + `__wafer_info`).
/// 6. Compare the info name with the `[package]` `{org}/{name}`.
/// 7. Warn if the binary exceeds 16 MiB.
pub fn build(dir: &Path) -> anyhow::Result<()> {
    // -----------------------------------------------------------------------
    // 1. Load wafer.toml [package].
    // -----------------------------------------------------------------------
    let package = WaferToml::read(&dir.join("wafer.toml"))
        .context("Failed to read wafer.toml — run `wafer new` to create one")?
        .package()?;
    let full_name = package.full_name();

    println!("Building block: {full_name}");

    // -----------------------------------------------------------------------
    // 1b. Lockfile ↔ wafer.toml sync check.
    // Silent when wafer.lock is absent; errors on drift with a hint.
    // -----------------------------------------------------------------------
    check_wafer_lock_sync(dir)?;

    // -----------------------------------------------------------------------
    // 2. Detect language.
    // -----------------------------------------------------------------------
    let lang = detect_language(dir)?;

    // -----------------------------------------------------------------------
    // 3. Run the toolchain.
    // -----------------------------------------------------------------------
    let block_wasm_path = dir.join("target").join("block.wasm");

    // The `{block}` half of the `{org}/{block}` name is the best
    // deterministic anchor for picking among multiple `.wasm` outputs.
    match lang {
        Lang::Rust => build_rust(dir, &block_wasm_path, &package.name)?,
        Lang::Go => build_go(dir, &block_wasm_path)?,
    }

    // -----------------------------------------------------------------------
    // 4. Validate and compare.
    // -----------------------------------------------------------------------
    println!("Validating {}…", block_wasm_path.display());

    let info = validate_wasm(&block_wasm_path).context("WASM validation failed")?;

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
    // 5. Size warning.
    // -----------------------------------------------------------------------
    let metadata = std::fs::metadata(&block_wasm_path)
        .with_context(|| format!("Failed to stat {}", block_wasm_path.display()))?;

    let size = metadata.len();
    if size > WASM_SIZE_WARN_BYTES {
        eprintln!(
            "Warning: block.wasm is {:.1} MiB — consider optimizing with `wasm-opt`.",
            size as f64 / (1024.0 * 1024.0)
        );
    }

    println!(
        "OK  {} v{}  ({:.1} KiB)",
        info.name,
        info.version,
        size as f64 / 1024.0
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Language-specific build steps
// ---------------------------------------------------------------------------

fn build_rust(dir: &Path, out: &Path, block_name: &str) -> anyhow::Result<()> {
    // Verify the target is installed.
    let check = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .context("Failed to run `rustup target list --installed`")?;

    let installed = String::from_utf8_lossy(&check.stdout);
    if !installed.contains("wasm32-wasip1") {
        bail!(
            "The `wasm32-wasip1` target is not installed.\n\
             Run: rustup target add wasm32-wasip1"
        );
    }

    // cargo build --target wasm32-wasip1 --release
    println!("Running: cargo build --target wasm32-wasip1 --release");
    let status = Command::new("cargo")
        .args(["build", "--target", "wasm32-wasip1", "--release"])
        .current_dir(dir)
        .status()
        .context("Failed to run `cargo build`")?;

    if !status.success() {
        bail!("`cargo build --target wasm32-wasip1 --release` failed");
    }

    // Find the .wasm output.  The package name may differ from the crate name;
    // look for any .wasm in the release directory.
    let release_dir = dir.join("target").join("wasm32-wasip1").join("release");

    let wasm_file = find_wasm_in_dir(&release_dir, block_name)
        .with_context(|| format!("No .wasm file found in {}", release_dir.display()))?;

    println!("Found: {}", wasm_file.display());

    // Ensure the destination directory exists.
    let dest_dir = out.parent().unwrap();
    std::fs::create_dir_all(dest_dir)
        .with_context(|| format!("Failed to create {}", dest_dir.display()))?;

    std::fs::copy(&wasm_file, out)
        .with_context(|| format!("Failed to copy {} → {}", wasm_file.display(), out.display()))?;

    println!("Copied to {}", out.display());
    Ok(())
}

fn build_go(dir: &Path, out: &Path) -> anyhow::Result<()> {
    // Ensure the destination directory exists.
    let dest_dir = out.parent().unwrap();
    std::fs::create_dir_all(dest_dir)
        .with_context(|| format!("Failed to create {}", dest_dir.display()))?;

    println!("Running: tinygo build -target wasi -o target/block.wasm .");
    let status = Command::new("tinygo")
        .args(["build", "-target", "wasi", "-o", out.to_str().unwrap(), "."])
        .current_dir(dir)
        .status()
        .context("Failed to run `tinygo build` — is TinyGo installed?")?;

    if !status.success() {
        bail!("`tinygo build` failed");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helper: find a single .wasm file in a directory (skip .d files).
// ---------------------------------------------------------------------------

/// Normalize a file stem / block name for comparison: lowercase and treat
/// `-` and `_` as equivalent. Cargo replaces `-` with `_` in output artifact
/// names, so a `my-block` manifest matches a `my_block.wasm` output.
fn normalize_stem(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c == '-' {
                '_'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect()
}

/// Find the block's `.wasm` output in `dir`.
///
/// `read_dir` order is filesystem-arbitrary, so when more than one `.wasm`
/// exists we cannot rely on position. Instead we prefer the file whose stem
/// matches `block_name` (with `-`/`_` normalized, since cargo rewrites `-` to
/// `_` in artifact names). Only if no stem matches do we fall back to the
/// first entry, warning that the choice is ambiguous.
fn find_wasm_in_dir(dir: &Path, block_name: &str) -> anyhow::Result<std::path::PathBuf> {
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("Failed to read directory: {}", dir.display()))?;

    let mut found: Vec<std::path::PathBuf> = Vec::new();

    for entry in entries {
        let entry = entry.with_context(|| format!("Failed to read entry in {}", dir.display()))?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "wasm") {
            found.push(path);
        }
    }

    match found.len() {
        0 => bail!("No .wasm files found in {}", dir.display()),
        1 => Ok(found.into_iter().next().unwrap()),
        _ => {
            let want = normalize_stem(block_name);
            let matched = found.iter().find(|p| {
                p.file_stem()
                    .is_some_and(|s| normalize_stem(&s.to_string_lossy()) == want)
            });
            match matched {
                Some(path) => Ok(path.clone()),
                None => {
                    // No stem matches the block name — the choice is genuinely
                    // ambiguous, so fall back to the first entry and warn.
                    eprintln!(
                        "Warning: multiple .wasm files found in {} and none match block {:?}; using {}",
                        dir.display(),
                        block_name,
                        found[0].display()
                    );
                    Ok(found.into_iter().next().unwrap())
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Lockfile sync check
// ---------------------------------------------------------------------------

/// If wafer.lock exists, enforce §Lockfile ↔ manifest sync against
/// wafer.toml. A missing wafer.lock is silent — pre-install projects are
/// allowed to build. (wafer.toml itself is guaranteed by the caller, which
/// already loaded `[package]` from it.)
fn check_wafer_lock_sync(dir: &Path) -> anyhow::Result<()> {
    let toml_path = dir.join("wafer.toml");
    let lock_path = dir.join("wafer.lock");
    if !lock_path.is_file() {
        return Ok(());
    }
    let wt = crate::wafer_toml::WaferToml::read(&toml_path)?;
    let lf = crate::lockfile::Lockfile::load(&lock_path)?
        .ok_or_else(|| anyhow::anyhow!("wafer.lock exists but failed to load (unreachable)"))?;
    if let Err(e) = crate::sync_check::check(&wt, &lf) {
        anyhow::bail!("{e}\nhint: run 'wafer install' without --frozen to update wafer.lock");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"\0asm").unwrap();
    }

    #[test]
    fn find_wasm_single_file_returned() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "anything.wasm");
        let got = find_wasm_in_dir(tmp.path(), "my-block").unwrap();
        assert_eq!(got.file_name().unwrap(), "anything.wasm");
    }

    #[test]
    fn find_wasm_prefers_stem_matching_block_name() {
        let tmp = tempfile::tempdir().unwrap();
        // Two outputs; the deps/* one sorts arbitrarily but must not win.
        touch(tmp.path(), "aaaa_unrelated.wasm");
        touch(tmp.path(), "my_block.wasm");
        let got = find_wasm_in_dir(tmp.path(), "my-block").unwrap();
        assert_eq!(
            got.file_name().unwrap(),
            "my_block.wasm",
            "should prefer the .wasm whose stem matches the block name (- normalized to _)"
        );
    }

    #[test]
    fn find_wasm_falls_back_to_first_when_no_stem_matches() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "alpha.wasm");
        touch(tmp.path(), "beta.wasm");
        // No file stem matches "my-block"; fall back is allowed (still a .wasm).
        let got = find_wasm_in_dir(tmp.path(), "my-block").unwrap();
        let name = got.file_name().unwrap().to_string_lossy();
        assert!(name == "alpha.wasm" || name == "beta.wasm", "got {name}");
    }

    #[test]
    fn normalize_stem_treats_dash_and_underscore_equal() {
        assert_eq!(normalize_stem("My-Block"), normalize_stem("my_block"));
        assert_eq!(normalize_stem("a-b-c"), "a_b_c");
    }
}
