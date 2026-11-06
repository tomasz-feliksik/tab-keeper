//! tab-keeper: list, close, group and arrange Chrome tabs across profiles.

use clap::Parser;

/// Lets people and agents list, close, group and arrange Chrome tabs across profiles.
#[derive(Parser)]
#[command(name = "tab-keeper", version)]
struct Cli {}

fn main() {
    Cli::parse();
}
