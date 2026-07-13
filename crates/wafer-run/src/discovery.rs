//! Auto-discovery of WASM block files and flow JSON files.
//!
//! These helpers are used by platform builders (e.g. a consuming application) to scan
//! the working directory for user-defined blocks and flows at startup.

use std::path::{Path, PathBuf};

/// Scan for WASM block files under `blocks_dir/**/target/block.wasm` recursively.
///
/// For each subdirectory found under `blocks_dir`, checks whether
/// `{subdir}/target/block.wasm` exists. Returns the list of matching paths.
/// Returns an empty `Vec` if `blocks_dir` does not exist (not an error).
pub fn discover_wasm_blocks(blocks_dir: &Path) -> Vec<PathBuf> {
    if !blocks_dir.is_dir() {
        return Vec::new();
    }

    let mut results = Vec::new();
    scan_for_wasm_blocks(blocks_dir, &mut results);
    results
}

/// Recursively walk `dir`, collecting any `target/block.wasm` found under it.
fn scan_for_wasm_blocks(dir: &Path, results: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        // Check if this directory contains target/block.wasm.
        let candidate = path.join("target").join("block.wasm");
        if candidate.is_file() {
            results.push(candidate);
        }

        // Recurse into subdirectories (supports nested block layouts).
        scan_for_wasm_blocks(&path, results);
    }
}

/// Scan for flow JSON files under `flows_dir/**/*.json` recursively.
///
/// Returns all `.json` files found anywhere under `flows_dir`.
/// Returns an empty `Vec` if `flows_dir` does not exist (not an error).
pub fn discover_flows(flows_dir: &Path) -> Vec<PathBuf> {
    if !flows_dir.is_dir() {
        return Vec::new();
    }

    let mut results = Vec::new();
    scan_for_flows(flows_dir, &mut results);
    results
}

/// Recursively walk `dir`, collecting all `.json` files.
fn scan_for_flows(dir: &Path, results: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_for_flows(&path, results);
        } else if path.extension().and_then(|s| s.to_str()) == Some("json") {
            results.push(path);
        }
    }
}
