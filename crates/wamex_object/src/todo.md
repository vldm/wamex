# Check/Unknowk:
- [ ] one of two globals is missing - probably not referenced in relocs. (should be included into main)
- [ ] check that starting offset and relocs work like they should
- [ ] no data in output (broken test)

# Bugs:
- [x] if table already exist - ignore. (don't push duplicate of indirect_table)
one created during Module::new() and other is referenced by reloc
- [X] memory non copied (no references in reloc i think).

# Features/low-priority:
- [ ] names sections
- [ ] linker sections
- [ ] More sections that in input (data count, tags, sart fn?)
- [ ] features in binary reader should be set from feature section
- [ ] segment is argument for generate fn, but should be part of self, to reduce missuse.