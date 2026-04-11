# Inspect wasm modules in more fancy way.

Most of modern wasm apps are built with llvm and combine multiple layers that need to live together.
Wasm functions, data segments, data symbols, memories, symbol table, relocations, components adapters, and so on.

During developing low-level tools, structural representation is crucial and helps debugging huge array of codegen problems.

There multiple tools that helps debug and visualise Web Assembly module.
WABT has `wasm-validate` and `wasm-objdump` bytecodealiance has `wasm-tools`.
But all that tooling is very limited in representing structural information and reporting errors:

- validators shows only offset and short errors (without visual representation of what caused this error);
- there is no linkage between entities (no way to follow calls, relocs, etc. );
- linkage symbols is supported but in limited way;
- relocations list is hard to read;
- no support of dylibs;
- etc.

`wamex-object` is a library that aims to utilize duality of wasm representations (wasm spec and linkage information), and be the building brick for code transformers and wasm tooling. So during it's build a lot of helper functionality was born (hexdumps, depgraph, dylink tacking). So `wamex-inspect` is a try to extract this functionality in a small reusable tui aplication, linke `wasm-objdump` + `wasm-validate` but interactive and with better support of extensions linkage, dylink, and later component model.

## Structural representation

The representation is very straighforward, you start on section view, and can go drill deeper into smaller objects.
