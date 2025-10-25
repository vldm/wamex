use std::path::{Path, PathBuf};

use analysis::split_point::SplitProgramInfo;
use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use gxhash::{HashSet, HashSetExt};

// todo: Refactor analysis and emit modules.
pub mod analysis;
pub mod emit;
mod helpers;
#[macro_use]
mod index;
#[cfg(feature = "metadata")]
mod metadata_ext;

mod diff;
// mod js_glue;
pub mod read;

pub use read::InputModule;

use crate::analysis::split_point::SplitModuleIdentifier;

#[derive(Debug, Parser)]
#[command(name = "wasm-split")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}
#[derive(Debug, Args)]
#[command(name = "wasm-split")]
pub struct Split {
    /// Input .wasm file.
    pub input: PathBuf,

    /// Output directory.
    pub output: PathBuf,

    /// Print verbose split information.
    /// Also if metadata is enabled, it will print it in pretty JSON format.
    #[arg(short, long)]
    pub verbose: bool,

    #[arg(short, long)]
    pub metadata: bool,

    #[arg(short, long)]
    pub precise_modification: bool,

    /// Skip writing files (for benchmarking).
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct Diff {
    pub left: PathBuf,
    pub right: PathBuf,
    #[arg(short, long)]
    pub structural: bool,
}

#[derive(Debug, Args)]
pub struct Roundtrip {
    pub input: PathBuf,
    pub output: PathBuf,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Split wasm module into multiple parts.
    Split(Split),

    /// Compare two wasm modules.
    Diff(Diff),

    /// Roundtrip wasm module.
    Roundtrip(Roundtrip),
}

//The flow of the program is simple:
// 1. Parse the input wasm file.
// 2. Analyze the wasm module to gather information about its structural dependencies. And indetify split points.
// 3. Emit processed modules to the output directory.
// 3.1 Modify module:
//    - Relocate functions.
//    - Patch data lookups. (e.g. const.get -> global.get)
// Also there should be a routine that can compare and reload changed chunks.

pub fn main(args: Cli) -> Result<()> {
    match args.command {
        Command::Split(args) => split(args)?,
        Command::Diff(args) => diff(args)?,
        Command::Roundtrip(args) => roundtrip(args)?,
    };
    Ok(())
}
pub fn roundtrip(args: Roundtrip) -> Result<()> {
    let input_wasm = std::fs::read(&args.input)?;
    let module = InputModule::parse(&input_wasm)?;
    let info = analysis::ModuleInfo::new(&module)?;
    //     // println!("names: {:#?}", module.names);
    let dep_graph = analysis::dep_graph::get_dependencies(&module, &info)?;

    let split_program_info = SplitProgramInfo::compute_split_modules(&info, &dep_graph, &[])?;

    assert!(
        split_program_info.output_modules.len() == 1,
        "Roundtrip should produce single module",
    );
    crate::emit::emit_modules(
        &info,
        &split_program_info,
        &HashSet::new(),
        false,
        |_: &SplitModuleIdentifier, data: &[u8]| -> Result<()> {
            std::fs::write(&args.output, data)?;
            Ok(())
        },
    )?;

    Ok(())
}
pub fn split(args: Split) -> Result<()> {
    let input_wasm = std::fs::read(&args.input)?;
    split_inner(
        &input_wasm,
        args.metadata,
        args.verbose,
        args.precise_modification,
        args.dry_run.then(|| args.output.as_path()),
        |identifier: &SplitModuleIdentifier, data: &[u8]| -> Result<()> {
            if !args.dry_run {
                let output_filename = identifier.name() + ".wasm";
                std::fs::create_dir_all(&args.output)?;
                std::fs::write(args.output.join(output_filename), data)?;
            } else {
                log::info!("Skipping writing module {} (dry run)", identifier.name());
            }
            Ok(())
        },
    )
}

#[doc(hidden)]
// Full split routine, but without file I/O reading
pub fn split_inner(
    input_wasm: &[u8],
    emit_metadata: bool,
    verbose: bool,
    precise_modification: bool,
    metadata_output: Option<&Path>,
    emit_module_fn: impl FnMut(&SplitModuleIdentifier, &[u8]) -> Result<()>,
) -> Result<()> {
    let module = InputModule::parse(&input_wasm)?;
    let info = analysis::ModuleInfo::new(&module)?;
    //     // println!("names: {:#?}", module.names);
    let dep_graph = analysis::dep_graph::get_dependencies(&module, &info)?;
    let split_points = analysis::split_point::find_split_points(&module, &info)?;

    log::debug!("split_points={split_points:?}");
    let mut split_program_info =
        SplitProgramInfo::compute_split_modules(&info, &dep_graph, &split_points)?;

    log::debug!("split_program_info={split_program_info:?}");
    if verbose {
        println!("dep_graph={dep_graph:?}");
        for (name, split_deps) in split_program_info.output_modules.iter() {
            split_deps.print(format!("{:?}", name).as_str(), &info, &dep_graph);
        }
    }
    // one of the possible mode is to merge all shared with main chunks into main module.
    // The other way can be used in incremental build, when main is not changed but we emit "mini-main".
    crate::emit::merge_main_shared(&mut split_program_info);

    // some wbg functions need to be moved to main before splitting.
    let wbg_fns = crate::emit::hoist_wbg_deps_to_main(&info, &dep_graph, &mut split_program_info);
    crate::emit::emit_modules(
        &info,
        &split_program_info,
        &wbg_fns,
        precise_modification,
        emit_module_fn,
    )?;

    #[cfg(feature = "metadata")]
    if emit_metadata {
        if let Some(output_path) = metadata_output {
            let metadata_path = output_path.join("metadata.json");
            let metadata = metadata_ext::build_metadata_and_snapshot(&info, &split_program_info);
            let metadata_json = if verbose {
                serde_json::to_string_pretty(&metadata)?
            } else {
                serde_json::to_string(&metadata)?
            };

            std::fs::write(metadata_path, metadata_json)?;
        }
    }

    Ok(())
}

pub fn diff(args: Diff) -> Result<()> {
    let left = std::fs::read(&args.left)?;
    let right = std::fs::read(&args.right)?;
    let left_module = InputModule::parse(&left)?;
    let right_module = InputModule::parse(&right)?;

    let diff = diff::Compare::new(&left_module, &right_module, args.structural);
    diff.print_diff()?;

    Ok(())
}
