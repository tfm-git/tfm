use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(version, about = "Git-native AI localization tooling")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a target-only locale layout. English source text remains in code.
    Init {
        #[arg(long, value_name = "BCP47", required = true)]
        locale: Vec<String>,
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Validate TFM state and target locale catalogs without modifying files.
    Check {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Init { locale, path } => {
            tfm_core::init_project(&path, &locale)?;
            println!("initialized TFM project at {}", path.display());
        }
        Command::Check { path } => {
            let report = tfm_core::check_project(&path)?;
            println!(
                "valid: {} messages, {} target catalogs",
                report.message_count, report.catalog_count
            );
        }
    }
    Ok(())
}
