use clap::Parser;

fn main() -> anyhow::Result<()> {
    let _ = env_logger::Builder::new()
        .parse_filters("info")
        .parse_default_env()
        .init();
    let args = wasm_split_cli::Cli::parse();

    wasm_split_cli::main(args)?;

    Ok(())
}
