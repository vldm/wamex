
import { initSync } from "./main.js";

function mergeWithConflicts(obj1, obj2) {
    const merged = {};
    const conflicts = [];

    // Copy properties from obj1
    for (let key of Object.keys(obj1)) {
        merged[key] = obj1[key];
    }

    // Copy properties from obj2 and check for conflicts
    for (let key of Object.keys(obj2)) {
        if (key in merged) {
            conflicts.push(key);
        }
        merged[key] = obj2[key];
    }

    return {
        new: merged,
        conflicts
    };
}


const wasmSplitState = {
    exports: {},
    mainVars: undefined

};

function _updateGlobalExports(newExports) {
    const result = mergeWithConflicts(wasmSplitState.exports, newExports);
    wasmSplitState.exports = result.new;
    if (result.conflicts.length > 0) {
        console.warn("Conflicts in exports:", result.conflicts);
    }
}

function ensureMainLoaded() {
    if (wasmSplitState.mainVars != undefined) {
        return;
    }
    let mainExports = initSync(undefined, undefined);

    wasmSplitState.mainVars = {
        __stack_pointer: mainExports.__stack_pointer,
        __tls_base: mainExports.__tls_base,
        memory: mainExports.memory,
    };
}

// TODO: Handle deps
function makeLoadLibrary(url) {
    let alreadyLoaded = false;
    return async (exportName, callbackData) => {
        if (alreadyLoaded) return;
        ensureMainLoaded();
        const response = await fetch(url);
        const imports = {
            __wasm_split: {
                __stack_pointer: wasmSplitState.mainVars.__stack_pointer,
                __tls_base: wasmSplitState.mainVars.__tls_base,
                memory: wasmSplitState.mainVars.memory,
            },
        };

        const module = await WebAssembly.instantiateStreaming(response, imports);
        const module_exports = module.instance.exports;
        _updateGlobalExports(module_exports);

        alreadyLoaded = true;
        mainExports.exports[exportName](
            callbackData,
            true,
        );

    };
}

export const __wasm_split_load_static_str = makeLoadLibrary(new URL("./static_str.wasm", import.meta.url), []);