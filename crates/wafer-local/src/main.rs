//! `solobase-dev` — Local development runner.
//!
//! Runs the same WAFER runtime as production but with local backends:
//! - SQLite or local Postgres for database
//! - Local filesystem for storage
//! - .wasm files from ./blocks/ directory for custom blocks
//!
//! Registers both wafer-core infrastructure blocks AND solobase feature blocks,
//! providing the full Solobase API locally.
//!
//! # Usage
//!
//! ```bash
//! # Start with defaults (SQLite, port 8090)
//! solobase-dev
//!
//! # Custom config
//! DATABASE_TYPE=postgres DATABASE_URL=postgres://... solobase-dev
//!
//! # Load custom WASM blocks from a directory
//! BLOCKS_DIR=./my-blocks solobase-dev
//! ```

use std::path::Path;

use tracing_subscriber::{fmt, EnvFilter};
use wafer_run::Wafer;

#[tokio::main]
async fn main() {
    // 1. Initialize tracing
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,wafer=debug,solobase=debug"));

    fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_thread_ids(false)
        .init();

    tracing::info!("solobase-dev starting (local development mode)");

    // 2. Create WAFER runtime
    let mut wafer = Wafer::new();

    // 3. Load infrastructure block configs
    let blocks_json = std::env::var("BLOCKS_JSON").unwrap_or_else(|_| "blocks.json".into());
    if let Err(e) = wafer.load_blocks_json(&blocks_json) {
        tracing::warn!("could not load {}: {} — using defaults", blocks_json, e);
    }

    // 4. Register wafer-core infrastructure blocks
    wafer_core::register_all(&mut wafer);
    tracing::info!("infrastructure blocks registered");

    // 5. Register solobase feature blocks (env-var-driven)
    solobase::blocks::register_selected(&mut wafer);
    tracing::info!("solobase feature blocks registered");

    // 6. Load custom WASM blocks from directory
    let blocks_dir = std::env::var("BLOCKS_DIR").unwrap_or_else(|_| "blocks".into());
    load_wasm_blocks(&mut wafer, &blocks_dir);

    // 7. Register flow definitions (wafer-core base + solobase features)
    let _ = wafer_core::flows::register_flows(&mut wafer);
    solobase::flows::register_selected_flows(&mut wafer);
    tracing::info!("flow definitions registered");

    // 8. Start runtime
    let wafer = wafer
        .start()
        .await
        .expect("failed to start WAFER runtime");
    tracing::info!("WAFER runtime started — local dev server ready");

    // 9. Wait for shutdown
    shutdown_signal().await;
    wafer.shutdown().await;
    tracing::info!("solobase-dev shutdown complete");
}

/// Load .wasm files from a directory as sandboxed WASM blocks.
fn load_wasm_blocks(wafer: &mut Wafer, dir: &str) {
    let path = Path::new(dir);
    if !path.exists() {
        tracing::debug!("no blocks directory at '{}' — skipping", dir);
        return;
    }

    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("could not read blocks directory '{}': {}", dir, e);
            return;
        }
    };

    let mut count = 0;
    for entry in entries.flatten() {
        let file_path = entry.path();
        if file_path.extension().map(|e| e == "wasm").unwrap_or(false) {
            match load_single_wasm_block(wafer, &file_path) {
                Ok(name) => {
                    tracing::info!(block = %name, path = %file_path.display(), "loaded WASM block");
                    count += 1;
                }
                Err(e) => {
                    tracing::warn!(path = %file_path.display(), error = %e, "failed to load WASM block");
                }
            }
        }
    }

    if count > 0 {
        tracing::info!(count, "custom WASM blocks loaded");
    }
}

/// Load a single .wasm file and register it with the runtime.
#[cfg(feature = "wasm")]
fn load_single_wasm_block(wafer: &mut Wafer, path: &Path) -> Result<String, String> {
    use std::sync::Arc;
    use wafer_run::block::Block;
    use wafer_run::wasm::WASMBlock;

    let block = WASMBlock::load(
        path.to_str().ok_or("invalid path")?,
    )?;
    let name = block.info().name.clone();

    wafer.register_block(&name, Arc::new(block));
    Ok(name)
}

#[cfg(not(feature = "wasm"))]
fn load_single_wasm_block(_wafer: &mut Wafer, path: &Path) -> Result<String, String> {
    Err(format!(
        "WASM support not enabled — cannot load {}",
        path.display()
    ))
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("received Ctrl+C — shutting down"),
        _ = terminate => tracing::info!("received SIGTERM — shutting down"),
    }
}
