use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

// todo: Refactor analysis and emit modules.
pub mod analysis;
pub mod emit;
mod helpers;
#[macro_use]
mod index;
mod diff;
mod list_set;
pub mod read;

pub use analysis::split_point::{ModuleIdentifier, SplitModuleIdentifier, SplitProgramInfo};
pub use anyhow::Result;
pub use read::InputModule;
pub use wamex_types::{BumpVersion, ModuleId};

use crate::emit::CommonEmitInfo;

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

    /// Parse instructions when creating pic modules (slower, but more robust to wasm spec changes).
    #[arg(short, long)]
    pub precise_modification: bool,

    /// Skip writing files (for benchmarking).
    #[arg(long)]
    pub dry_run: bool,

    /// Specify the split point extraction strategy.
    #[arg(value_enum, default_value_t = SplitPointExtractor::Wamex)]
    pub split_point_extractor: SplitPointExtractor,
}

/// This is temporary solution to support old __wamex__ split points
#[derive(Debug, ValueEnum, Clone, Copy)]
pub enum SplitPointExtractor {
    /// Use regexp and _wasm_split_ prefix to identify split points.
    Legacy,
    /// Use _wamex_ prefix and .start_with instead of regexp.
    Wamex,
}

#[derive(Debug, Args)]
pub struct Diff {
    pub left: PathBuf,
    pub right: PathBuf,
    #[arg(short, long)]
    pub structural: bool,
}

#[derive(Debug, Args)]
pub struct Debug {
    pub input: PathBuf,
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

    Debug(Debug),
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
        Command::Debug(args) => debug(args)?,
    };
    Ok(())
}
pub fn roundtrip(args: Roundtrip) -> Result<()> {
    let input_wasm = std::fs::read(&args.input)?;
    let module = InputModule::parse(&input_wasm)?;
    let info = analysis::ModuleInfo::from_raw_module(module)?;
    let dep_graph = analysis::dep_graph::get_dependencies(&info)?;

    let split_program_info = SplitProgramInfo::compute_split_modules(&info, &dep_graph, &[])?;

    assert!(
        split_program_info.output_modules.len() == 1,
        "Roundtrip should produce single module",
    );
    crate::emit::emit_modules(
        &info,
        false,
        &split_program_info,
        &Default::default(),
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
        args.verbose,
        args.precise_modification,
        args.split_point_extractor,
        |identifier: ModuleId, data: &[u8]| -> Result<()> {
            let output_filename = format!("{}.wasm", identifier.module_full_name());
            if !args.dry_run {
                std::fs::create_dir_all(&args.output)?;
                std::fs::write(args.output.join(output_filename), data)?;
            } else {
                log::info!("Skipping writing module {output_filename} (dry run)");
            }
            Ok(())
        },
    )
}

#[doc(hidden)]
// Full split routine, but without file I/O reading
pub fn split_inner(
    input_wasm: &[u8],
    verbose: bool,
    precise_modification: bool,
    split_point_extractor: SplitPointExtractor,
    mut emit_module_fn: impl FnMut(ModuleId, &[u8]) -> Result<()>,
) -> Result<()> {
    let module = InputModule::parse(input_wasm)?;
    let info = analysis::ModuleInfo::from_raw_module(module)?;
    let dep_graph = analysis::dep_graph::get_dependencies(&info)?;
    let split_points = analysis::split_point::find_split_points(&info, split_point_extractor)?;

    let mut split_program_info =
        SplitProgramInfo::compute_split_modules(&info, &dep_graph, &split_points)?;

    // one of the possible mode is to merge all shared with main chunks into main module.
    // The other way can be used in incremental build, when main is not changed but we emit "mini-main".
    crate::emit::merge_main_shared(&mut split_program_info);

    // some wbg functions need to be moved to main before splitting.
    let wbg_fns = crate::emit::hoist_wbg_deps_to_main(&info, &dep_graph, &mut split_program_info);
    if verbose {
        println!("Split points: {split_points:?}");
        println!("Split program info: {split_program_info:?}");
        println!("Dependency graph: {dep_graph:?}");
        println!("Module symbols:");
        info.symbols.print_debug();

        println!("Module split details:");
        for (name, split_deps) in split_program_info.output_modules.iter() {
            split_deps.print(format!("{:?}", name).as_str(), &info, &dep_graph);
        }
    }
    let emit_fn = |identifier: &SplitModuleIdentifier, data: &[u8]| -> Result<()> {
        let module_id = ModuleId::new_from_components(
            identifier.to_string(),
            None, // add versioning later
            None,
        );
        emit_module_fn(module_id, data)
    };

    crate::emit::emit_modules(
        &info,
        verbose,
        &split_program_info,
        &wbg_fns,
        precise_modification,
        emit_fn,
    )?;

    Ok(())
}

pub fn diff(args: Diff) -> Result<()> {
    let left = std::fs::read(&args.left)?;
    let right = std::fs::read(&args.right)?;
    let left_module = InputModule::parse(&left)?;
    let right_module = InputModule::parse(&right)?;
    let left_module_info = analysis::ModuleInfo::from_raw_module(left_module)?;
    let right_module_info = analysis::ModuleInfo::from_raw_module(right_module)?;

    let diff = diff::Compare::new(&left_module_info, &right_module_info, args.structural);
    diff.print_diff()?;

    Ok(())
}

pub fn debug(args: Debug) -> Result<()> {
    let input = std::fs::read(&args.input)?;
    let module = InputModule::parse(&input)?;
    let info = analysis::ModuleInfo::from_raw_module(module)?;

    let program_info = analysis::split_point::SplitProgramInfo::default();
    // verbose flag will print debug info as side effect.
    // TODO: make it more functional.
    let _ci = CommonEmitInfo::new(&info, true, &program_info)?;

    info.symbols.print_debug();
    Ok(())
}

// 1. check imports - exports
// 1.1. no wamex_split.rs imports should be present in main module
// 1.2. all exports should be used in some module
// 1.3. "lazy" imports (indirect fns) should be reserved for specific modules only. This place should be inited in elem section of modules.
// 2. Check that data segments are same from original module (no data loss, and no extra fields).
// 3. same for functions - no loss, no extra functions (only trampolines).

// pub fn validate() {

// }
