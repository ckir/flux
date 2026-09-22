//! Flux CLI (spec §3.6): argument parsing and command dispatch.

use clap::{Parser, Subcommand};
use flux_fs::{CopyOptions, Durability, OperationId, Preserve, Publish};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "flux", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Copy a single file.
    Copy {
        source: PathBuf,
        destination: PathBuf,
        /// Fail the file if its timestamps cannot be applied (§44.1).
        #[arg(long)]
        preserve_times: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Commands::Copy { source, destination, preserve_times } => {
            let opts = CopyOptions {
                // Explicitly requested is Strict; otherwise applied best-effort.
                preserve_times: if preserve_times { Preserve::Strict } else { Preserve::Default },
                preserve_permissions: Preserve::Default,
                durability: Durability::Normal,
                publish: Publish::Replace,
                operation_id: OperationId::new(format!("{}", std::process::id())),
            };
            let fs = flux_platform::StdFileSystem;
            match flux_core::copy_file(&fs, &source, &destination, &opts) {
                Ok(outcome) => {
                    for f in &outcome.metadata_failures {
                        eprintln!("METADATA_APPLY_FAILED: {:?}: {}", f.item, f.error);
                    }
                    // §44.1: best-effort failures still publish, but the operation exits 1.
                    if outcome.metadata_failures.is_empty() {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::from(1)
                    }
                }
                Err(e) => {
                    eprintln!("{}: {}", e.code.as_str(), e.source);
                    ExitCode::from(1)
                }
            }
        }
    }
}
