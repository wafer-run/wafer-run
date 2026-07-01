use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

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

    match lang {
        Lang::Rust => build_rust(dir, &block_wasm_path)?,
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

    // Build and read the exact `.wasm` path back from cargo's JSON artifact
    // stream instead of guessing the target-directory layout. This is correct
    // no matter where the target directory lives — including a shared
    // `CARGO_TARGET_DIR` / `[build] target-dir` where many blocks compile into
    // one release directory — and regardless of how the crate name maps to the
    // artifact filename. `--message-format=json-render-diagnostics` keeps
    // human-readable diagnostics on stderr while emitting machine-readable
    // artifact messages on stdout.
    println!("Running: cargo build --target wasm32-wasip1 --release");
    let mut child = Command::new("cargo")
        .args([
            "build",
            "--target",
            "wasm32-wasip1",
            "--release",
            "--message-format=json-render-diagnostics",
        ])
        .current_dir(dir)
        .stdout(Stdio::piped())
        .spawn()
        .context("Failed to run `cargo build`")?;

    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("child stdout was piped")
        .read_to_string(&mut stdout)
        .context("Failed to read `cargo build` output")?;

    let status = child.wait().context("Failed to wait for `cargo build`")?;
    if !status.success() {
        bail!("`cargo build --target wasm32-wasip1 --release` failed");
    }

    let wasm_file = wasm_path_from_cargo_json(&stdout)?;
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

    // Filesystem paths are arbitrary byte sequences on Unix, so `to_str()` can
    // be `None`. Surface a clean error instead of panicking when `wafer build`
    // is run from a directory whose path isn't valid UTF-8.
    let out_str = out
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("output path is not valid UTF-8: {}", out.display()))?;

    println!("Running: tinygo build -target wasi -o target/block.wasm .");
    let status = Command::new("tinygo")
        .args(["build", "-target", "wasi", "-o", out_str, "."])
        .current_dir(dir)
        .status()
        .context("Failed to run `tinygo build` — is TinyGo installed?")?;

    if !status.success() {
        bail!("`tinygo build` failed");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helper: locate the block's .wasm from cargo's JSON artifact stream.
// ---------------------------------------------------------------------------

/// Extract the block's `.wasm` output path from cargo's
/// `--message-format=json` stream.
///
/// Cargo emits one JSON object per line. `compiler-artifact` messages carry the
/// compiled target's `crate_types` and the `filenames` it produced. A block
/// crate is the only `cdylib` in its build (its dependencies compile as `lib`s),
/// so the `cdylib` artifact's `.wasm` filename is the block itself. Reading the
/// path straight from cargo means we never assume the target-directory layout
/// or the crate-name → filename mapping, so a `CARGO_TARGET_DIR` shared across
/// many blocks resolves to the right artifact. If more than one `cdylib` ever
/// appears, the last wins — the root crate links after its dependencies.
fn wasm_path_from_cargo_json(stdout: &str) -> anyhow::Result<PathBuf> {
    let mut wasm: Option<PathBuf> = None;

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // stdout carries only JSON with this message format; ignore stray lines.
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if msg.get("reason").and_then(serde_json::Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let is_cdylib = msg
            .get("target")
            .and_then(|t| t.get("crate_types"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|kinds| kinds.iter().any(|k| k.as_str() == Some("cdylib")));
        if !is_cdylib {
            continue;
        }
        if let Some(files) = msg.get("filenames").and_then(serde_json::Value::as_array) {
            for f in files {
                if let Some(path) = f.as_str() {
                    if path.ends_with(".wasm") {
                        wasm = Some(PathBuf::from(path));
                    }
                }
            }
        }
    }

    wasm.ok_or_else(|| {
        anyhow::anyhow!(
            "cargo build produced no cdylib `.wasm` artifact — ensure the block crate sets \
             `crate-type = [\"cdylib\"]`"
        )
    })
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
    use crate::lockfile::LockfileToml as _;
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

    #[test]
    fn wasm_path_picks_the_cdylib_over_dep_libs() {
        // A realistic stream: dep `lib` artifacts plus the block `cdylib`. The
        // block's `.wasm` must win even though its filename (crate name) does
        // not resemble the wafer.toml block name, and even though the release
        // dir is shared (all paths under one `/shared/...` target dir).
        let stdout = concat!(
            r#"{"reason":"compiler-artifact","target":{"name":"serde","crate_types":["lib"]},"filenames":["/shared/wasm32-wasip1/release/libserde.rlib"]}"#,
            "\n",
            r#"{"reason":"compiler-artifact","target":{"name":"gizza-ai-image-resize-block","crate_types":["cdylib","rlib"]},"filenames":["/shared/wasm32-wasip1/release/gizza_ai_image_resize_block.wasm","/shared/wasm32-wasip1/release/libgizza_ai_image_resize_block.rlib"]}"#,
            "\n",
            r#"{"reason":"build-finished","success":true}"#,
            "\n",
        );
        let got = wasm_path_from_cargo_json(stdout).unwrap();
        assert_eq!(
            got,
            PathBuf::from("/shared/wasm32-wasip1/release/gizza_ai_image_resize_block.wasm")
        );
    }

    #[test]
    fn wasm_path_errors_when_no_cdylib_wasm_present() {
        let stdout = concat!(
            r#"{"reason":"compiler-artifact","target":{"name":"serde","crate_types":["lib"]},"filenames":["/t/libserde.rlib"]}"#,
            "\n",
            r#"{"reason":"build-finished","success":true}"#,
            "\n",
        );
        assert!(wasm_path_from_cargo_json(stdout).is_err());
    }

    #[test]
    fn wasm_path_ignores_non_json_and_blank_lines() {
        let stdout = concat!(
            "\n",
            "   \n",
            "warning: some rustc line that leaked to stdout\n",
            r#"{"reason":"compiler-artifact","target":{"name":"b","crate_types":["cdylib"]},"filenames":["/t/b.wasm"]}"#,
            "\n",
        );
        let got = wasm_path_from_cargo_json(stdout).unwrap();
        assert_eq!(got, PathBuf::from("/t/b.wasm"));
    }

    #[test]
    fn wasm_path_takes_last_cdylib_when_multiple() {
        // Defensive: the root crate links after its dependencies, so the last
        // cdylib `.wasm` is the block.
        let stdout = concat!(
            r#"{"reason":"compiler-artifact","target":{"name":"a","crate_types":["cdylib"]},"filenames":["/t/a.wasm"]}"#,
            "\n",
            r#"{"reason":"compiler-artifact","target":{"name":"b","crate_types":["cdylib"]},"filenames":["/t/b.wasm"]}"#,
            "\n",
        );
        let got = wasm_path_from_cargo_json(stdout).unwrap();
        assert_eq!(got, PathBuf::from("/t/b.wasm"));
    }
}
