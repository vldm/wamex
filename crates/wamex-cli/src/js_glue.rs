struct WasmJsGlue {
    
}

fn implement_js_glue() -> String {
    let mut javascript = String::new();
    javascript.push_str(
        r#"import { initSync } from "./main.js";
    function makeLoad(url, deps) {
      let alreadyLoaded = false;
      return async(callbackIndex, callbackData) => {
        if (alreadyLoaded) return;
        for (let dep of deps) {
          await dep();
        }
        let mainExports = undefined;
          try {
            const response = await fetch(url);
            mainExports = initSync(undefined, undefined);
            const imports = {
              env: {
                memory: mainExports.memory,
              },
              __wasm_split: {
                __indirect_function_table: mainExports.__indirect_function_table,
                __stack_pointer: mainExports.__stack_pointer,
                __tls_base: mainExports.__tls_base,
                memory: mainExports.memory,
              },
            };
            const module = await WebAssembly.instantiateStreaming(response, imports);
            alreadyLoaded = true;
            if (callbackIndex === undefined) return;
            mainExports.__indirect_function_table.get(callbackIndex)(
              callbackData,
              true,
            );
          } catch (e) {
            if (callbackIndex === undefined) throw e;
            console.error("Failed to load " + url.href, e);
            if (mainExports === undefined) {
              mainExports = initSync(undefined, undefined);
            }
            mainExports.__indirect_function_table.get(callbackIndex)(
              callbackData,
              false,
            );
          }
      };
    }
    "#,
    );
    let mut split_deps = HashMap::<String, Vec<String>>::new();
    for (name, _) in split_program_info.output_modules.iter() {
        let SplitModuleIdentifier::Chunk(splits) = name else {
            continue;
        };
        for split in splits {
            split_deps
                .entry(split.clone())
                .or_default()
                .push(name.name());
        }
        javascript.push_str(format!(
                "const __wasm_split_load_{name} = makeLoad(new URL(\"./{name}.wasm\", import.meta.url), []);\n",
                name = name.name(),
            ).as_str())
    }
    for (identifier, _) in split_program_info.output_modules.iter().rev() {
        if matches!(identifier, SplitModuleIdentifier::Chunk(_)) {
            continue;
        }
        let name = identifier.name();
        javascript.push_str(format!(
                "export const __wasm_split_load_{name} = makeLoad(new URL(\"./{name}.wasm\", import.meta.url), [{deps}]);\n",
                name = name,
                deps = split_deps
                .remove(&name)
                .unwrap_or_default()
                .iter()
                .map(|x| format!("__wasm_split_load_{x}"))
                .collect::<Vec<_>>()
                .join(", "),
            ).as_str())
    }
}
