# Check/Unknowk:
- [ ] check that starting offset and relocs work like they should
- [X] no data in output (broken test)

# Bugs:
- [X] if table already exist - ignore. (don't push duplicate of indirect_table)
one created during Module::new() and other is referenced by reloc
- [X] memory non copied (no references in reloc i think).
- [X] elements missing
- [X] one of two globals is missing - probably not referenced in relocs. (should be included into main)

# Features/low-priority:
- [ ] names sections
- [ ] linker sections
- [X] exports sections
- [ ] More sections that in input (data count, tags, sart fn?) - skip empty
- [ ] features in binary reader should be set from feature section
- [ ] segment is argument for generate fn, but should be part of self, to reduce missuse. 
- [ ] use one start fn, and emit it before finalize.