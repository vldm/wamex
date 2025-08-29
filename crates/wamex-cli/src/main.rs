use clap::Parser;

fn main() -> anyhow::Result<()> {
    let _ = env_logger::Builder::new()
        .parse_filters("info")
        .parse_default_env()
        .init();
    let args = wamex_cli::Cli::parse();

    wamex_cli::main(args)?;

    Ok(())
}
