# Symbols diffng technique
To implement incremental splitting we need to identify which functions and data parts was changed.

Using `linking` section we can match symbols by name, and later check if their content (instructions or data) changed.
Function ids and data offsets can change when module is recompiled. So actual content is represented by it's normalized bytes and array of `used` symbols.

Data segment symbols usually anonymous (e.g `.Lanon.<hash>.<index>`), so we cannot use their names to match them across versions.
Instead of names we use their `content` (list of users of this symbol), and fuzzy match them.

Then we can check which module includes changed symbols, and mark them for re-splitting. If changed symbol is included in `main` module, then complete re-splitting is required. 

# Known limitations
  
## `Span` changes in symbol diffing.
As in [Rust incremental compiliation](https://rustc-dev-guide.rust-lang.org/queries/incremental-compilation-in-detail.html) wamex experience [similar problems](https://github.com/rust-lang/rust/issues/47389) with `Span` changes causing many symbols to be marked as changed.

### Using `proc-macro::Span` to unique identify method names.

Originally `wasm-split` and `wamex` used `proc-macro::Span` to identify function names in diffing. This was used to avoid name mangling issues.
However this approach made unusable match by name, not only for entrypoints but also for inner closures in macro-generated code.
Example:
```
Added: _ZN4core3ptr172drop_in_place$LT$example..sub..dep_dyn..__wamex_00dep_dyn00_export_dep_dyn_crates_example_src_sub_rs_46_1_uf3906ba086ca8c3d0c406b26bd560d61..$u7b$$u7b$closure$u7d$$u7d$$GT$17h93931f6d742f5728E [151]
Removed: _ZN4core3ptr172drop_in_place$LT$example..sub..dep_dyn..__wamex_00dep_dyn00_export_dep_dyn_crates_example_src_sub_rs_47_1_uf3906ba086ca8c3d0c406b26bd560d61..$u7b$$u7b$closure$u7d$$u7d$$GT$17h59eee5e1011b05cdE [151]
Added: _ZN4core3ops8function6FnOnce9call_once17h12f562084a0a8079E [4233]
Removed: _ZN4core3ops8function6FnOnce9call_once17hec7b9b3c10461016E [4233]
```
Note: not only name changed, but also distinguish hash. And some symbols don't include macro generated name at all.

For this reasons we avoid using `Span` for matching names, and only `file_name` is used. Therefore no functions `#[wamex::split]` functions with same name in one file are allowed.


### `Location::caller` in panics.
Generating code that panics (either by explicit `panic!` or implicit `unwrap` or by using `async fn`), will include `Location::caller` information in generated code.

For example, adding a new import to a module (line with `use super::*` in this case) can change `Location` of many functions, causing them to be marked as changed even if their actual body didn't change.
```
Replaced: 8, Added: 0, Removed: 0, Same: 6678
Replaced: .Lanon.5a8021ba5a3be6e331d93d472da636f5.2 [4306] -> .Lanon.5a8021ba5a3be6e331d93d472da636f5.2 [4306] Detail: Body changed from 0000000019000000060000001f000000 to 0000000019000000050000001f000000
Replaced: .Lanon.5a8021ba5a3be6e331d93d472da636f5.4 [4308] -> .Lanon.5a8021ba5a3be6e331d93d472da636f5.4 [4308] Detail: Body changed from 00000000190000001c0000001f000000 to 00000000190000001b0000001f000000
Replaced: .Lanon.5a8021ba5a3be6e331d93d472da636f5.6 [4311] -> .Lanon.5a8021ba5a3be6e331d93d472da636f5.6 [4311] Detail: Body changed from 00000000190000001e00000005000000 to 00000000190000001d00000005000000
Replaced: .Lanon.5a8021ba5a3be6e331d93d472da636f5.8 [4313] -> .Lanon.5a8021ba5a3be6e331d93d472da636f5.8 [4313] Detail: Body changed from 00000000190000000b0000001f000000 to 00000000190000000a0000001f000000
Replaced: .Lanon.5a8021ba5a3be6e331d93d472da636f5.10 [4315] -> .Lanon.5a8021ba5a3be6e331d93d472da636f5.10 [4315] Detail: Body changed from 0000000019000000250000001f000000 to 0000000019000000240000001f000000
Replaced: .Lanon.5a8021ba5a3be6e331d93d472da636f5.11 [4316] -> .Lanon.5a8021ba5a3be6e331d93d472da636f5.11 [4316] Detail: Body changed from 0000000019000000280000003c000000 to 0000000019000000270000003c000000
Replaced: .Lanon.5a8021ba5a3be6e331d93d472da636f5.13 [4318] -> .Lanon.5a8021ba5a3be6e331d93d472da636f5.13 [4318] Detail: Body changed from 0000000019000000130000001f000000 to 0000000019000000120000001f000000
Replaced: .Lanon.5a8021ba5a3be6e331d93d472da636f5.15 [4320] -> .Lanon.5a8021ba5a3be6e331d93d472da636f5.15 [4320] Detail: Body changed from 00000000190000002e0000001f000000 to 00000000190000002d0000001f000000
[2025-11-16T17:43:29Z WARN  wamex_cli::diff::cli] Changed module: main
Replaced: 3, Added: 0, Removed: 0, Same: 0
Replaced: .Lanon.5a8021ba5a3be6e331d93d472da636f5.2 [4306] -> .Lanon.5a8021ba5a3be6e331d93d472da636f5.2 [4306] Detail: Body changed from 0000000019000000060000001f000000 to 0000000019000000050000001f000000
Replaced: .Lanon.5a8021ba5a3be6e331d93d472da636f5.8 [4313] -> .Lanon.5a8021ba5a3be6e331d93d472da636f5.8 [4313] Detail: Body changed from 00000000190000000b0000001f000000 to 00000000190000000a0000001f000000
Replaced: .Lanon.5a8021ba5a3be6e331d93d472da636f5.13 [4318] -> .Lanon.5a8021ba5a3be6e331d93d472da636f5.13 [4318] Detail: Body changed from 0000000019000000130000001f000000 to 0000000019000000120000001f000000
[2025-11-16T17:43:29Z WARN  wamex_cli::diff::cli] Changed module: async_string
Replaced: 1, Added: 0, Removed: 0, Same: 0
Replaced: .Lanon.5a8021ba5a3be6e331d93d472da636f5.6 [4311] -> .Lanon.5a8021ba5a3be6e331d93d472da636f5.6 [4311] Detail: Body changed from 00000000190000001e00000005000000 to 00000000190000001d00000005000000
[2025-11-16T17:43:29Z WARN  wamex_cli::diff::cli] Changed module: multiple_dyn_fns
Replaced: 1, Added: 0, Removed: 0, Same: 0
Replaced: .Lanon.5a8021ba5a3be6e331d93d472da636f5.11 [4316] -> .Lanon.5a8021ba5a3be6e331d93d472da636f5.11 [4316] Detail: Body changed from 0000000019000000280000003c000000 to 0000000019000000270000003c000000
[2025-11-16T17:43:29Z WARN  wamex_cli::diff::cli] Changed module: main_async_string
Replaced: 1, Added: 0, Removed: 0, Same: 0
Replaced: .Lanon.5a8021ba5a3be6e331d93d472da636f5.4 [4308] -> .Lanon.5a8021ba5a3be6e331d93d472da636f5.4 [4308] Detail: Body changed from 00000000190000001c0000001f000000 to 00000000190000001b0000001f000000
[2025-11-16T17:43:29Z WARN  wamex_cli::diff::cli] Changed module: main_dep_dyn
Replaced: 2, Added: 0, Removed: 0, Same: 0
Replaced: .Lanon.5a8021ba5a3be6e331d93d472da636f5.10 [4315] -> .Lanon.5a8021ba5a3be6e331d93d472da636f5.10 [4315] Detail: Body changed from 0000000019000000250000001f000000 to 0000000019000000240000001f000000
Replaced: .Lanon.5a8021ba5a3be6e331d93d472da636f5.15 [4320] -> .Lanon.5a8021ba5a3be6e331d93d472da636f5.15 [4320] Detail: Body changed from 00000000190000002e0000001f000000 to 00000000190000002d0000001f000000
```

This made even worse if we use `async/await` or `unwrap` in generated code.
Therefore we try to minimize usage of `Span`s in macro-generated code, and move as much logic as possible into helper types (see `wamex::loader_combinator`).
Using this combinator instead of `async fn` in macro-generated remove all `Replaced` entries from main module.

For other modules, and `Location` in general we can add mem-patching step in future to simplify hot-patch delivery for small data changes like this.
Additionally `core::panic::Location` have distinct format and can be identified.

All this mitigations is not perfect, and therefore left for "after" mvp optimizations.