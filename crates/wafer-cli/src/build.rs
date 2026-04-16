use std::{path::Path, process::Command};

use anyhow::{bail, Context};

use crate::{
    detect::{detect_language, Lang},
    manifest::Manifest,
    validate::validate_wasm,
};

/// Size threshold (in bytes) above which we warn the user.
const WASM_SIZE_WARN_BYTES: u64 = 16 * 1024 * 1024; // 16 MiB

/// Build the WAFER block in `dir`.
///
/// Steps:
/// 1. Load and validate `manifest.json`.
/// 2. Auto-detect the project language.
/// 3. Run the appropriate toolchain to produce a `.wasm` binary.
/// 4. Copy the output to `<dir>/target/block.wasm`.
/// 5. Validate the WASM (required exports + `__wafer_info`).
/// 6. Compare the info name with the manifest name.
/// 7. Warn if the binary exceeds 16 MiB.
pub fn build(dir: &Path) -> anyhow::Result<()> {
    // -----------------------------------------------------------------------
    // 1. Load manifest.
    // -----------------------------------------------------------------------
    let manifest = Manifest::load(dir)
        .context("Failed to load manifest.json — run `wafer new` to create one")?;

    println!("Building block: {}", manifest.name);

    // -----------------------------------------------------------------------
    // 2. Detect language.
    // -----------------------------------------------------------------------
    let lang = detect_language(dir)?;

    // -----------------------------------------------------------------------
    // 3. Run the toolchain.
    // -----------------------------------------------------------------------
    let block_wasm_path = dir.join("target").join("block.wasm");

    match lang {
        Lang::Rust => build_rust(dir, &block_wasm_path)?,
        Lang::Go => build_go(dir, &block_wasm_path)?,
        Lang::TypeScript => build_typescript(dir, &block_wasm_path)?,
    }

    // -----------------------------------------------------------------------
    // 4. Validate and compare.
    // -----------------------------------------------------------------------
    println!("Validating {}…", block_wasm_path.display());

    let info = validate_wasm(&block_wasm_path).context("WASM validation failed")?;

    if info.name != manifest.name {
        bail!(
            "Block name mismatch: manifest says {:?} but __wafer_info() returned {:?}.\n\
             Make sure the `name` argument in `#[wafer_block(name = …)]` matches manifest.json.",
            manifest.name,
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

fn build_rust(dir: &Path, out: &Path) -> anyhow::Result<()> {
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

    let wasm_file = find_wasm_in_dir(&release_dir)
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

fn build_typescript(dir: &Path, out: &Path) -> anyhow::Result<()> {
    // -----------------------------------------------------------------------
    // Step 1: Check for esbuild.
    // -----------------------------------------------------------------------
    let esbuild_check = Command::new("npx")
        .args(["esbuild", "--version"])
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if esbuild_check.is_err() || !esbuild_check.unwrap().success() {
        bail!(
            "esbuild not found. Install it with:\n  npm install --save-dev esbuild\n\
             or globally:\n  npm install -g esbuild"
        );
    }

    // -----------------------------------------------------------------------
    // Step 2: Bundle TypeScript → runtime/bundle.js (IIFE format).
    // -----------------------------------------------------------------------
    println!("Running: esbuild src/index.ts → runtime/bundle.js");
    let status = Command::new("npx")
        .args([
            "esbuild",
            "src/index.ts",
            "--bundle",
            "--outfile=runtime/bundle.js",
            "--format=iife",
            "--platform=neutral",
            "--target=es2020",
            "--main-fields=main,module",
        ])
        .current_dir(dir)
        .status()
        .context("Failed to run esbuild")?;
    if !status.success() {
        bail!("esbuild bundle failed");
    }

    // -----------------------------------------------------------------------
    // Step 3: Compile Rust runtime → WASM.
    // -----------------------------------------------------------------------
    let runtime_dir = dir.join("runtime");
    if !runtime_dir.join("Cargo.toml").exists() {
        bail!(
            "runtime/Cargo.toml not found.\n\
             Make sure the block was scaffolded with `wafer new --lang ts` (v0.2+)."
        );
    }

    // Verify the wasm32-wasip1 target is installed.
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

    println!("Running: cargo build --target wasm32-wasip1 --release (in runtime/)");
    let status = Command::new("cargo")
        .args(["build", "--target", "wasm32-wasip1", "--release"])
        .current_dir(&runtime_dir)
        .status()
        .context("Failed to run `cargo build` in runtime/")?;
    if !status.success() {
        bail!("`cargo build --target wasm32-wasip1 --release` failed in runtime/");
    }

    // -----------------------------------------------------------------------
    // Step 4: Find and copy .wasm output.
    // -----------------------------------------------------------------------
    let wasm_dir = runtime_dir
        .join("target")
        .join("wasm32-wasip1")
        .join("release");
    let wasm_file = find_wasm_in_dir(&wasm_dir)
        .with_context(|| format!("No .wasm file found in {}", wasm_dir.display()))?;

    println!("Found: {}", wasm_file.display());

    let dest_dir = out.parent().unwrap();
    std::fs::create_dir_all(dest_dir)
        .with_context(|| format!("Failed to create {}", dest_dir.display()))?;
    std::fs::copy(&wasm_file, out)
        .with_context(|| format!("Failed to copy {} → {}", wasm_file.display(), out.display()))?;

    println!("Copied to {}", out.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Helper: find a single .wasm file in a directory (skip .d files).
// ---------------------------------------------------------------------------

fn find_wasm_in_dir(dir: &Path) -> anyhow::Result<std::path::PathBuf> {
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
            // Multiple .wasm files — prefer one whose stem matches common output names.
            // Fall back to the first one and warn.
            eprintln!(
                "Warning: multiple .wasm files found in {}; using {}",
                dir.display(),
                found[0].display()
            );
            Ok(found.into_iter().next().unwrap())
        }
    }
}
