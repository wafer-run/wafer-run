//! `wafer dev` — file-watching dev loop with wafer-aware boot summary.

use std::{path::PathBuf, time::Duration};

use anyhow::Result;
use clap::Args;

mod bin_detect;
mod summary;
mod supervisor;
mod watcher;

/// Arguments for the `wafer dev` subcommand.
#[derive(Args, Debug, Clone)]
pub struct DevArgs {
    /// Cargo bin target to run. If omitted, auto-detected from Cargo.toml.
    #[arg(long)]
    pub bin: Option<String>,

    /// Pass --release to cargo (slower rebuilds, sometimes useful for perf).
    #[arg(long)]
    pub release: bool,

    /// Add an extra path/glob to watch. Repeatable. Default watch list:
    /// `src` (recursive), `Cargo.toml`, `wafer.lock`. Use --no-default-watch
    /// to skip the defaults.
    #[arg(long = "watch", value_name = "PATTERN")]
    pub watch: Vec<String>,

    /// Disable the default watch list (src, Cargo.toml, wafer.lock).
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
    let bin = bin_detect::resolve_bin(detected, args.bin.as_deref())?;
    let debounce = Duration::from_millis(args.debounce);

    let patterns = watcher::merge_patterns(&args.watch, !args.no_default_watch);
    let change_rx = watcher::spawn_watcher(&patterns, debounce)?;

    let (line_tx, mut line_rx) = tokio::sync::mpsc::channel::<String>(64);

    // Summary aggregator: accumulates per-spawn state, prints the banner on
    // `event = "listening"`. Resets after each banner so the next spawn
    // starts fresh. Note that flows register BEFORE `starting` (they're
    // queued via `add_flow_json` before `Wafer::start()`), so we cannot
    // reset on `Starting`.
    tokio::spawn(async move {
        let mut state = summary::BannerState::default();
        while let Some(line) = line_rx.recv().await {
            if let Some(ev) = summary::parse_event(&line) {
                if let Some(banner) = state.apply(ev) {
                    eprintln!("[wafer dev] {banner}");
                }
            }
        }
    });

    let cfg = supervisor::SupervisorConfig {
        bin: bin.clone(),
        release: args.release,
        cargo_args: args.cargo_args.clone(),
        kill_timeout: Duration::from_secs(args.kill_timeout),
    };

    eprintln!("[wafer dev] watching {patterns:?}; running bin = {bin}");
    supervisor::run(cfg, change_rx, line_tx).await
}
