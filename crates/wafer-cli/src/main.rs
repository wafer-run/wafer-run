mod build;
mod commands;
mod credentials;
mod detect;
mod manifest;
mod package;
mod registry_client;
mod scaffold;
mod test_runner;
mod validate;

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
        /// Programming language: rust | go | typescript.
        #[arg(long, default_value = "rust")]
        lang: String,
    },
    /// Build the block in the current directory.
    Build,
    /// Run tests against the block.
    Test {
        /// Path to a test fixture or directory (default: ./tests/).
        path: Option<String>,
    },
    /// Package the built block for publishing.
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
    }

    Ok(())
}
