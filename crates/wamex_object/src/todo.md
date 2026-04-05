# Check/Unknowk:
- [ ] check that starting offset and relocs work like they should
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
- [ ] handle data imports from main module (without got)
 
# Features/low-priority:
- [ ] process relocs
- [ ] names sections
- [ ] linker sections
- [X] exports sections
- [x] More sections that in input (data count, tags, sart fn?) - skip empty
- [ ] features in binary reader should be set from feature section
- [x] segment is argument for generate fn, but should be part of self, to reduce missuse. 
- [x] use one start fn, and emit it before finalize.
- [ ] Extract new imports processing from copy-plan 
