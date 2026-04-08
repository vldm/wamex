# Check/Unknowk:
- [X] check that starting offset and relocs work like they should
- [X] no data in output (broken test)

# Bugs:
- [X] if table already exist - ignore. (don't push duplicate of indirect_table)
one created during Module::new() and other is referenced by reloc
- [X] memory non copied (no references in reloc i think).
- [X] elements missing
- [X] one of two globals is missing - probably not referenced in relocs. (should be included into main)
- [X] reloc broke code 
- [X] sub module don't use memory_base for their variables.
- [X] data relocs in main modue is shifted
- [X] handle data imports from main module (without got)
- [ ] no stack pointer preserved
 
# Features/low-priority:
- [x] process relocs
- [x] names section
- [ ] linker sections (symtable + relocs)
- [X] exports sections
- [x] More sections that in input (data count, tags, sart fn?) - skip empty
- [ ] features in binary reader should be set from feature section
- [x] segment is argument for generate fn, but should be part of self, to reduce missuse. 
- [x] use one start fn, and emit it before finalize.
- [x] Extract new imports processing from copy-plan 

# wasm_emitter:
- [ ] publish crate as demo that allow emitting state with relocs

# Split:
- [ ] extract analysis/split into crate
- [ ] compact import (indirect_table)
- [ ] Start testing 

# Codegen:
- [ ] generate new module from scratch

# Linker:
- [ ] link multiple modules

# Objdump [tui]:
- [ ] interactive jump by relocs/index
- [ ] colorfull relocs + legend
- [ ] validate

# Next to research:
- Modifier that either remove entry, or create new non related to processed entry. e.g. (replace import fn by defined, create trampoline, replace const data offsets with global var, etc.).