//! Tests for wafer_run::discovery — WASM block and flow file auto-discovery.

use std::fs;
use tempfile::TempDir;
use wafer_run::discovery::{discover_flows, discover_wasm_blocks};

// ---------------------------------------------------------------------------
// discover_wasm_blocks
// ---------------------------------------------------------------------------

#[test]
fn discovers_flat_blocks() {
    let tmp = TempDir::new().unwrap();
    let blocks_dir = tmp.path().join("blocks");
    let block_target = blocks_dir.join("my-block").join("target");
    fs::create_dir_all(&block_target).unwrap();
    let wasm_path = block_target.join("block.wasm");
    fs::write(&wasm_path, b"fake wasm").unwrap();

    let found = discover_wasm_blocks(&blocks_dir);
    assert_eq!(found.len(), 1, "expected exactly one WASM block");
    assert_eq!(found[0], wasm_path);
}

#[test]
fn discovers_nested_blocks() {
    let tmp = TempDir::new().unwrap();
    let blocks_dir = tmp.path().join("blocks");
    // Nested: blocks/payments/stripe/target/block.wasm
    let block_target = blocks_dir.join("payments").join("stripe").join("target");
    fs::create_dir_all(&block_target).unwrap();
    let wasm_path = block_target.join("block.wasm");
    fs::write(&wasm_path, b"fake wasm").unwrap();

    let found = discover_wasm_blocks(&blocks_dir);
    assert_eq!(found.len(), 1, "expected exactly one nested WASM block");
    assert_eq!(found[0], wasm_path);
}

#[test]
fn discovers_multiple_blocks() {
    let tmp = TempDir::new().unwrap();
    let blocks_dir = tmp.path().join("blocks");

    // Two blocks at different depths.
    for path in &[
        "auth/target/block.wasm",
        "payments/stripe/target/block.wasm",
    ] {
        let full = blocks_dir.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(&full, b"fake wasm").unwrap();
    }

    let mut found = discover_wasm_blocks(&blocks_dir);
    found.sort();
    assert_eq!(found.len(), 2, "expected two WASM blocks");
}

// ---------------------------------------------------------------------------
// discover_flows
// ---------------------------------------------------------------------------

#[test]
fn discovers_flows_recursively() {
    let tmp = TempDir::new().unwrap();
    let flows_dir = tmp.path().join("flows");

    let main_flow = flows_dir.join("main.json");
    fs::create_dir_all(&flows_dir).unwrap();
    fs::write(&main_flow, b"{}").unwrap();

    let api_dir = flows_dir.join("api");
    fs::create_dir_all(&api_dir).unwrap();
    let users_flow = api_dir.join("users.json");
    fs::write(&users_flow, b"{}").unwrap();

    let mut found = discover_flows(&flows_dir);
    found.sort();

    assert_eq!(found.len(), 2, "expected two flow JSON files");
    assert!(found.contains(&main_flow), "main.json missing");
    assert!(found.contains(&users_flow), "api/users.json missing");
}

#[test]
fn ignores_non_json_files_in_flows() {
    let tmp = TempDir::new().unwrap();
    let flows_dir = tmp.path().join("flows");
    fs::create_dir_all(&flows_dir).unwrap();
    fs::write(flows_dir.join("README.md"), b"docs").unwrap();
    fs::write(flows_dir.join("flow.json"), b"{}").unwrap();

    let found = discover_flows(&flows_dir);
    assert_eq!(found.len(), 1, "only .json files should be discovered");
}

// ---------------------------------------------------------------------------
// Nonexistent directory → empty Vec (not an error)
// ---------------------------------------------------------------------------

#[test]
fn empty_dir_returns_empty() {
    let tmp = TempDir::new().unwrap();
    let nonexistent = tmp.path().join("does_not_exist");

    let blocks = discover_wasm_blocks(&nonexistent);
    assert!(
        blocks.is_empty(),
        "nonexistent blocks dir should return empty"
    );

    let flows = discover_flows(&nonexistent);
    assert!(
        flows.is_empty(),
        "nonexistent flows dir should return empty"
    );
}
