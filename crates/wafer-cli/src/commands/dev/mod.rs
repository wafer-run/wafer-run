//! `wafer dev` — file-watching dev loop with wafer-aware boot summary.

use std::{path::PathBuf, time::Duration};

use anyhow::Result;
use clap::Args;

mod bin_detect;
// More submodules added in subsequent tasks: watcher, supervisor, summary.

/// Arguments for the `wafer dev` subcommand.
#[derive(Args, Debug, Clone)]
pub struct DevArgs {
    /// Cargo bin target to run. If omitted, auto-detected from Cargo.toml.
    #[arg(long)]
    pub bin: Option<String>,

    /// Pass --release to cargo (slower rebuilds, sometimes useful for perf).
    #[arg(long)]
    pub release: bool,

    /// Add an extra path/glob to watch. Repeatable.
    #[arg(long = "watch", value_name = "PATTERN")]
    pub watch: Vec<String>,

    /// Disable the default watch list (src/**/*.rs, Cargo.toml, wafer.lock).
    #[arg(long)]
    pub no_default_watch: bool,

    /// File-change debounce window in milliseconds.
    #[arg(long, default_value_t = 200)]
    pub debounce: u64,

    /// SIGTERM → SIGKILL grace period in seconds.
    #[arg(long, default_value_t = 3)]
    pub kill_timeout: u64,

    /// Extra args forwarded verbatim to cargo run (after `--`).
    #[arg(last = true)]
    pub cargo_args: Vec<String>,
}

/// Entry point invoked by `main.rs`.
pub async fn run(args: DevArgs) -> Result<()> {
    let manifest = PathBuf::from("Cargo.toml");
    let detected = bin_detect::detect_bins(&manifest)?;
    let _bin = bin_detect::resolve_bin(detected, args.bin.as_deref())?;
    let _debounce = Duration::from_millis(args.debounce);

    // Watcher + supervisor + summary wired in subsequent tasks.
    anyhow::bail!("wafer dev: not yet implemented (Task 3 skeleton; tasks 4-6 wire watcher/supervisor/summary)")
}
