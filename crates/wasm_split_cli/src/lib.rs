use std::{
    path::{Path, PathBuf},
    str::FromStr,
};

use analysis::split_point::SplitProgramInfo;
use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};

// todo: Refactor analysis and emit modules.
mod analysis;
mod emit;
mod helpers;
mod index;
#[cfg(feature = "metadata")]
mod metadata;

mod diff;
// mod js_glue;
mod read;

pub use read::InputModule;

use crate::emit::EmitConfig;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, ValueEnum)]
pub enum ModuleStructure {
    // Includes all shared chunks into main module.
    BigMain,
    // Emit chunks into separate modules.
    EmitChunks,
    // Emit chunks with main module.
    EmitMainChunked,
}
impl ToString for ModuleStructure {
    fn to_string(&self) -> String {
        match self {
            ModuleStructure::BigMain => "big-main".to_string(),
            ModuleStructure::EmitChunks => "emit-chunks".to_string(),
            ModuleStructure::EmitMainChunked => "emit-main-chunked".to_string(),
        }
    }
}
impl FromStr for ModuleStructure {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "big-main" => Ok(ModuleStructure::BigMain),
            "emit-chunks" => Ok(ModuleStructure::EmitChunks),
            "emit-main-chunked" => Ok(ModuleStructure::EmitMainChunked),
            _ => Err(format!("Unknown module structure: {s}")),
        }
    }
}

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

    /// Module structure.
    #[arg(long, default_value_t = ModuleStructure::EmitMainChunked)]
    pub module_structure: ModuleStructure,
}

#[derive(Debug, Args)]
pub struct Diff {
    pub left: PathBuf,
    pub right: PathBuf,
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

    //     let mut javascript = String::new();
    //     javascript.push_str(
    //         r#"import { initSync } from "./main.js";
    // function makeLoad(url, deps) {
    //   let alreadyLoaded = false;
    //   return async(callbackIndex, callbackData) => {
    //     if (alreadyLoaded) return;
    //     for (let dep of deps) {
    //       await dep();
    //     }
    //     let mainExports = undefined;
    //       try {
    //         const response = await fetch(url);
    //         mainExports = initSync(undefined, undefined);
    //         const imports = {
    //           env: {
    //             memory: mainExports.memory,
    //           },
    //           __wasm_split: {
    //             __indirect_function_table: mainExports.__indirect_function_table,
    //             __stack_pointer: mainExports.__stack_pointer,
    //             __tls_base: mainExports.__tls_base,
    //             memory: mainExports.memory,
    //           },
    //         };
    //         const module = await WebAssembly.instantiateStreaming(response, imports);
    //         alreadyLoaded = true;
    //         if (callbackIndex === undefined) return;
    //         mainExports.__indirect_function_table.get(callbackIndex)(
    //           callbackData,
    //           true,
    //         );
    //       } catch (e) {
    //         if (callbackIndex === undefined) throw e;
    //         console.error("Failed to load " + url.href, e);
    //         if (mainExports === undefined) {
    //           mainExports = initSync(undefined, undefined);
    //         }
    //         mainExports.__indirect_function_table.get(callbackIndex)(
    //           callbackData,
    //           false,
    //         );
    //       }
    //   };
    // }
    // "#,
    //     );
    //     let mut split_deps = HashMap::<String, Vec<String>>::new();
    //     for (name, _) in split_program_info.output_modules.iter() {
    //         let SplitModuleIdentifier::Chunk(splits) = name else {
    //             continue;
    //         };
    //         for split in splits {
    //             split_deps
    //                 .entry(split.clone())
    //                 .or_default()
    //                 .push(name.name());
    //         }
    //         javascript.push_str(format!(
    //             "const __wasm_split_load_{name} = makeLoad(new URL(\"./{name}.wasm\", import.meta.url), []);\n",
    //             name = name.name(),
    //         ).as_str())
    //     }
    //     for (identifier, _) in split_program_info.output_modules.iter().rev() {
    //         if matches!(identifier, SplitModuleIdentifier::Chunk(_)) {
    //             continue;
    //         }
    //         let name = identifier.name();
    //         javascript.push_str(format!(
    //             "export const __wasm_split_load_{name} = makeLoad(new URL(\"./{name}.wasm\", import.meta.url), [{deps}]);\n",
    //             name = name,
    //             deps = split_deps
    //             .remove(&name)
    //             .unwrap_or_default()
    //             .iter()
    //             .map(|x| format!("__wasm_split_load_{x}"))
    //             .collect::<Vec<_>>()
    //             .join(", "),
    //         ).as_str())
    //     }

    //     std::fs::write(args.output.join("__wasm_split.js"), javascript)?;
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
        EmitConfig::default(),
        &|_: usize, data: &[u8]| -> Result<()> {
            std::fs::write(&args.output, data)?;
            Ok(())
        },
    )?;

    Ok(())
}
pub fn split(args: Split) -> Result<()> {
    let input_wasm = std::fs::read(&args.input)?;
    let module = InputModule::parse(&input_wasm)?;
    let info = analysis::ModuleInfo::new(&module)?;
    //     // println!("names: {:#?}", module.names);
    let dep_graph = analysis::dep_graph::get_dependencies(&module, &info)?;
    let split_points = analysis::split_point::find_split_points(&module, &info)?;

    log::debug!("split_points={split_points:?}");
    let split_program_info =
        SplitProgramInfo::compute_split_modules(&info, &dep_graph, &split_points)?;

    log::debug!("split_program_info={split_program_info:?}");
    if args.verbose {
        println!("dep_graph={dep_graph:?}");
        for (name, split_deps) in split_program_info.output_modules.iter() {
            split_deps.print(format!("{:?}", name).as_str(), &info, &dep_graph);
        }
    }

    crate::emit::emit_modules(
        &info,
        &split_program_info,
        EmitConfig::default(),
        &|output_module_index: usize, data: &[u8]| -> Result<()> {
            let identifier = &split_program_info.output_modules[output_module_index].0;
            let output_filename = identifier.name() + ".wasm";
            let output_path = args.output.join(output_filename);
            std::fs::create_dir_all(&args.output)?;
            std::fs::write(output_path, data)?;
            Ok(())
        },
    )?;

    #[cfg(feature = "metadata")]
    if args.metadata {
        let metadata_path = args.output.join("metadata.json");
        let metadata = metadata::build_metadata(&info, &split_program_info, args.module_structure);
        let metadata_json = if args.verbose {
            serde_json::to_string_pretty(&metadata)?
        } else {
            serde_json::to_string(&metadata)?
        };
        std::fs::write(metadata_path, metadata_json)?;
    }

    Ok(())
}

pub fn diff(args: Diff) -> Result<()> {
    let left = std::fs::read(&args.left)?;
    let right = std::fs::read(&args.right)?;
    let left_module = InputModule::parse(&left)?;
    let right_module = InputModule::parse(&right)?;

    let diff = diff::Compare::new(&left_module, &right_module);
    diff.print_diff()?;

    Ok(())
}
