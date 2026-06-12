#![warn(missing_docs)]
//! `wafer` — command-line tool for creating, building, testing, packaging, and
//! publishing WAFER blocks.
//!
//! This is a bin-only crate (no `lib.rs`). All modules below are private to
//! `main.rs`; items marked `pub` are technically `pub(crate)` in effect because
//! the crate exposes no library API. The `#![warn(missing_docs)]` attribute is
//! kept on for consistency with the rest of the workspace.

mod block_name;
mod build;
mod cache;
mod commands;
mod credentials;
mod detect;
mod install;
mod lockfile;
mod package;
mod registry_client;
mod registry_error;
mod scaffold;
mod sync_check;
mod test_runner;
mod validate;
mod wafer_toml;
mod wasm_stubs;

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "wafer",
    version,
    about = "CLI for creating, building, testing, and packaging WAFER blocks"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new WAFER block project.
    New {
        /// Block name in {org}/{block} format (e.g. myorg/my-block).
        name: String,
        /// Programming language: rust | go.
        #[arg(long, default_value = "rust")]
        lang: String,
    },
    /// Build the block in the current directory.
    Build,
    /// Run the wafer-run app in this directory with file-watch + restart on save.
    ///
    /// Reads Cargo.toml from cwd, picks a bin target (or use --bin), runs
    /// `cargo run`, watches Rust source + Cargo.toml + wafer.lock, restarts
    /// on changes. Prints a wafer-aware boot summary on each successful start.
    Dev(commands::dev::DevArgs),
    /// Run tests against the block.
    Test {
        /// Path to a test fixture or directory (default: ./tests/).
        path: Option<String>,
    },
    /// Package the built block into a publishable .wafer tarball
    /// (target/wafer/{name}-{version}.wafer).
    Package,
    /// Log in to a WAFER registry.
    Login {
        /// Registry URL (overrides WAFER_REGISTRY env).
        #[arg(long)]
        registry: Option<String>,
    },
    /// Remove stored credentials for a registry.
    Logout {
        #[arg(long)]
        registry: Option<String>,
    },
    /// Show the user associated with the stored token.
    Whoami {
        #[arg(long)]
        registry: Option<String>,
    },
    /// Publish a packaged block tarball to a registry.
    Publish {
        /// Path to the .wafer tarball. Defaults to target/wafer/{name}-{version}.wafer.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Registry URL (overrides WAFER_REGISTRY env).
        #[arg(long)]
        registry: Option<String>,
        /// Print what would be published without making an HTTP call.
        #[arg(long)]
        dry_run: bool,
    },
    /// Yank a published version (marks it hidden but keeps it downloadable).
    Yank {
        /// Target in `org/block@version` form.
        target: String,
        /// Optional reason recorded alongside the yank.
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        registry: Option<String>,
    },
    /// Unyank a previously yanked version.
    Unyank {
        /// Target in `org/block@version` form.
        target: String,
        #[arg(long)]
        registry: Option<String>,
    },
    /// Search the registry for packages matching `query`.
    Search {
        /// Query string (substring match on package name).
        query: String,
        /// Emit JSON instead of a formatted table.
        #[arg(long)]
        json: bool,
        /// Registry URL (overrides WAFER_REGISTRY env).
        #[arg(long)]
        registry: Option<String>,
    },
    /// Show package or version detail.
    Info {
        /// Target in `org/block` or `org/block@version` form.
        target: String,
        /// Emit JSON instead of a formatted view.
        #[arg(long)]
        json: bool,
        /// Show all versions (default caps at 5 most-recent).
        #[arg(long)]
        all: bool,
        /// Registry URL (overrides WAFER_REGISTRY env).
        #[arg(long)]
        registry: Option<String>,
    },
    /// Install a package into the cache, and/or install the manifest.
    Install {
        /// Target in `org/block` or `org/block@version` form. Optional —
        /// omitting it installs everything from wafer.toml's [dependencies].
        target: Option<String>,
        /// Download + cache + update wafer.lock, but do not touch wafer.toml.
        #[arg(long)]
        cache_only: bool,
        /// Require wafer.lock to exist and match wafer.toml; no mutations.
        /// Cannot be combined with a target or --cache-only.
        #[arg(long)]
        frozen: bool,
        /// Registry URL (overrides WAFER_REGISTRY env).
        #[arg(long)]
        registry: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::New { name, lang } => {
            let language = detect::Lang::from_str(&lang)
                .with_context(|| format!("Invalid --lang value: {lang:?}"))?;
            scaffold::scaffold(&name, language)?;
        }
        Commands::Build => {
            build::build(&std::env::current_dir()?)?;
        }
        Commands::Dev(args) => {
            commands::dev::run(args).await?;
        }
        Commands::Test { path } => {
            test_runner::run_tests(&std::env::current_dir()?, path.as_deref())?;
        }
        Commands::Package => {
            package::package(&std::env::current_dir()?)?;
        }
        Commands::Login { registry } => {
            commands::login::run(registry).await?;
        }
        Commands::Logout { registry } => {
            commands::logout::run(registry).await?;
        }
        Commands::Whoami { registry } => {
            commands::whoami::run(registry).await?;
        }
        Commands::Publish {
            file,
            registry,
            dry_run,
        } => {
            commands::publish::run(file, registry, dry_run).await?;
        }
        Commands::Yank {
            target,
            reason,
            registry,
        } => {
            commands::yank::run(target, reason, registry, commands::yank::YankOp::Yank).await?;
        }
        Commands::Unyank { target, registry } => {
            commands::yank::run(target, None, registry, commands::yank::YankOp::Unyank).await?;
        }
        Commands::Search {
            query,
            json,
            registry,
        } => {
            commands::search::run(query, json, registry).await?;
        }
        Commands::Info {
            target,
            json,
            all,
            registry,
        } => {
            commands::info::run(target, json, all, registry).await?;
        }
        Commands::Install {
            target,
            cache_only,
            frozen,
            registry,
        } => {
            commands::install::run(target, cache_only, frozen, registry).await?;
        }
    }

    Ok(())
}
