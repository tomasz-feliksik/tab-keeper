//! tab-keeper: list, close, group and arrange Chrome tabs across profiles.

mod frame;
mod host;

use std::error::Error;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Lets people and agents list, close, group and arrange Chrome tabs across profiles.
#[derive(Parser)]
#[command(name = "tab-keeper", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// The native messaging relay. Chrome runs this, not people.
    Host {
        /// The extension origin Chrome passes to a native host. Chrome has already
        /// checked it against the manifest's `allowed_origins`, so it is not used.
        origin: Option<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let outcome = match cli.command {
        Command::Host { origin: _ } => host::run_from_environment(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            report(&error);
            ExitCode::FAILURE
        }
    }
}

/// Stderr only: in host mode, stdout belongs to Chrome.
fn report(error: &dyn Error) {
    eprintln!("tab-keeper: {error}");
    let mut cause = error.source();
    while let Some(error) = cause {
        eprintln!("  caused by: {error}");
        cause = error.source();
    }
}
