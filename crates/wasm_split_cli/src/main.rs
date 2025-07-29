use std::{collections::HashMap, path::Path};

use analysis::split_point::SplitProgramInfo;
use anyhow::Result;
use clap::Parser;
// use split_point::SplitModuleIdentifier;

// todo: Refactor analysis and emit modules.
mod analysis;
mod emit;
mod helpers;
mod index;
mod read;

use read::InputModule;

#[derive(Debug, Parser)]
#[command(name = "wasm-split")]
struct Cli {
    /// Input .wasm file.
    input: Box<Path>,

    /// Output directory.
    output: Box<Path>,

    /// Print verbose split information.
    #[arg(short, long)]
    verbose: bool,
}

//The flow of the program is simple:
// 1. Parse the input wasm file.
// 2. Analyze the wasm module to gather information about its structural dependencies. And indetify split points.
// 3. Modify module:
//    - Relocate functions.
//    - Patch data lookups. (e.g. const.get -> global.get)
// 4. Emit processed modules to the output directory.
// Also there should be a routine that can compare and reload changed chunks.

fn main() -> Result<()> {
    let _ = env_logger::Builder::new()
        .parse_filters("debug")
        .parse_default_env()
        .init();
    let args = Cli::parse();
    let input_wasm = std::fs::read(&args.input)?;
    let module = InputModule::parse(&input_wasm)?;
    let info = analysis::ModuleInfo::new(&module)?;
    //     // println!("names: {:#?}", module.names);
    let dep_graph = analysis::dep_graph::get_dependencies(&module, &info)?;
    println!("dep_graph={dep_graph:?}");
    let split_points = analysis::split_point::find_split_points(&module, &info)?;

    println!("split_points={split_points:?}");
    let split_program_info =
        SplitProgramInfo::compute_split_modules(&info, &dep_graph, &split_points)?;

    println!("split_program_info={split_program_info:?}");
    // if args.verbose {
    //     for (name, split_deps) in split_program_info.output_modules.iter() {
    //         split_deps.print(format!("{:?}", name).as_str(), &info);
    //     }
    // }

    crate::emit::emit_modules(&info, &split_program_info, &|output_module_index: usize,
                                                            data: &[u8]|
     -> Result<()> {
        let identifier = &split_program_info.output_modules[output_module_index].0;
        let output_filename = identifier.name() + ".wasm";
        let output_path = args.output.join(output_filename);
        std::fs::create_dir_all(&args.output)?;
        std::fs::write(output_path, data)?;
        Ok(())
    })?;

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
