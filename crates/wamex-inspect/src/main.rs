use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about = "Interactive TUI for wamex-object")]
struct Cli {
    /// Path to input wasm object file
    input: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    wamex_inspect::run_path(cli.input)
}
