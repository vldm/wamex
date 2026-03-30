use std::{fmt::Debug, ops::Range};

use anyhow::Result;
use cranelift_entity::{EntityRef, PrimaryMap, packed_option::ReservedValue};

use crate::{
    helpers::{RangeComp, cmp_range},
    index::GappedMap,
    layouts::DataSymbolRef,
    linkage::reloc::{
        AnyRelocationEntry, Encoding, EntityAddressMode, EntityRelocationEntry, Relative,
        RelocationEntry, RelocationWidth, SymbolType,
    },
    typed::{EntityKind, FnTypeRef, FunctionRef, SymbolId},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelocRange {
    pub(super) relocs: Range<usize>,
}
impl RelocRange {
    pub fn from_range(relocs: Range<usize>) -> Self {
        Self { relocs }
    }
}

impl ReservedValue for RelocRange {
    fn reserved_value() -> Self {
        Self {
            relocs: Range {
                start: usize::MAX,
                end: usize::MIN,
            },
        }
    }
    fn is_reserved_value(&self) -> bool {
        self.relocs.start == usize::MAX && self.relocs.end == usize::MIN
    }
}

/// Store for relocations per file.
/// Internal relocations representation can be ordered or no, based on constructor called.
#[derive(Debug, PartialEq, Eq)]
pub struct FileRelocs {
    // Relocations ordered by offsets in file.
    //
    array: Box<[EntityRelocationEntry]>,

    // In what symbol this relocation is placed
    code_owners: GappedMap<FunctionRef, RelocRange>,
    data_owners: GappedMap<DataSymbolRef, RelocRange>,
    // Elem/global doesn't have relocations.
    // TODO: custom section related relocs?
}

pub(crate) type Regions = (
    Vec<(Range<usize>, FunctionRef)>,
    Vec<(Range<usize>, DataSymbolRef)>,
);

impl FileRelocs {
    /// Build `FileRelocs` from parts.
    pub fn build_from_parts(
        array: Box<[EntityRelocationEntry]>,
        code_owners: GappedMap<FunctionRef, RelocRange>,
        data_owners: GappedMap<DataSymbolRef, RelocRange>,
    ) -> Self {
        let this = Self {
            array,
            code_owners,
            data_owners,
        };
        #[cfg(debug_assertions)]
        this.ensure_ordered_no_gaps();
        this
    }

    /// Resolve relocations symbols (to corresponding entities).
    /// code_owners and data_owners should contain regions in original file that belongs to each symbol.
    ///
    /// Build relocs map based on position in file of entities.
    #[tracing::instrument(skip_all)]
    pub fn build_relocs_static(
        file_relocs: impl IntoIterator<Item = AnyRelocationEntry>,
        file_db: &FileSymbolDb,
        regions: Regions,
    ) -> Result<Self> {
        let mut array = file_relocs
            .into_iter()
            .map(|r| match r {
                AnyRelocationEntry::Linkage(l) => {
                    let offset_info = file_db
                        .symbol_entity(l.symbol_id)
                        .expect("Malformed wasm: relocation references unknown symbol");

                    RelocationEntry {
                        symbol_id: offset_info.entity,
                        symbol_op: EntityAddressMode::from_llvm_relocs(
                            offset_info.entity,
                            l.symbol_op,
                        ),
                        offset: l.offset,
                        encoding: l.encoding,
                        width: l.width,
                        relation: l.relation,
                        addend: Self::checked_addend_increase(
                            l.addend,
                            offset_info.offset_in_entity,
                            l.symbol_op,
                        ),
                    }
                }
                AnyRelocationEntry::Type(t) => RelocationEntry {
                    offset: t.offset,
                    symbol_id: t.index.into(),
                    symbol_op: EntityAddressMode::StaticIndex,
                    encoding: Encoding::Leb,
                    width: RelocationWidth::Bits32,
                    relation: Relative::None,
                    addend: 0,
                },
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();

        array.sort_by_key(|e| e.offset);

        let (code_owners, data_owners) = Self::build_owners(&array, regions);
        let this = Self {
            array,
            code_owners,
            data_owners,
        };

        #[cfg(debug_assertions)]
        this.ensure_ordered_no_gaps();
        Ok(this)
    }

    fn ensure_ordered_no_gaps(&self) {
        // Ensure that each entity has it's entry in relocation, and that this entries are ordered and has no gaps.
        let mut last = 0;
        for (_, relocs) in self.iter_entities() {
            assert_eq!(last, relocs.start);
            last = relocs.end
        }
        assert_eq!(last, self.array.len());
    }

    /// Return range, coresponding to section, which owners provided by var.
    fn section_range(owners: &GappedMap<impl EntityRef, RelocRange>) -> Option<Range<usize>> {
        let start = owners.iter().next().map(|(_, range)| range.relocs.start)?;
        let end_key = owners.last_key()?;
        let end = owners
            .get(end_key)
            .map(|range| range.relocs.end)
            .expect("BUG: last key should have range");
        Some(start..end)
    }

    /// Get all code relocations related to code section
    pub fn get_code_section_relocs(&self) -> &[EntityRelocationEntry] {
        Self::section_range(&self.code_owners)
            .map(|range| {
                debug_assert!(
                    range.start == 0,
                    "Code section should start with first relocation entry"
                );
                &self.array[range]
            })
            .unwrap_or_default()
    }
    /// Get all data relocations related to data section
    pub fn get_data_section_relocs(&self) -> &[EntityRelocationEntry] {
        Self::section_range(&self.data_owners)
            .map(|range| {
                debug_assert!(
                    !(self.code_owners.is_empty() && range.start == 0),
                    "Data section should start with first relocation entry if no code relocations exist"
                );
                &self.array[range]
            })
            .unwrap_or_default()
    }

    // Convert region based ranges to ranges in relocs array.
    fn build_owners(
        relocs: &[EntityRelocationEntry],
        (mut code_regions, mut data_regions): Regions,
    ) -> (
        GappedMap<FunctionRef, RelocRange>,
        GappedMap<DataSymbolRef, RelocRange>,
    ) {
        /// Move relocations from flat list to symbols.
        ///
        fn move_relocs<U: EntityRef + Debug>(
            map: &mut GappedMap<U, RelocRange>,
            symbol_regions: impl IntoIterator<Item = (Range<usize>, U)>,
            start: &mut usize,
            relocs: &[EntityRelocationEntry],
        ) {
            let mut end = *start;
            'next_sym: for (region, sym) in symbol_regions {
                'more_relocs: while let Some(reloc) = relocs.get(end) {
                    match cmp_range(&region, reloc.relocation_range()) {
                        RangeComp::NonComparable | RangeComp::Within => {
                            panic!(
                                "BUG: Relocation entry is not related to symbols: {sym:?} {region:?} and {reloc:?}",
                            );
                        }
                        RangeComp::Equal | RangeComp::Overlap => {
                            end += 1;
                            continue 'more_relocs;
                        } // its our symbol, keep going
                        RangeComp::Right => {
                            panic!(
                                "BUG: Unprocessed relocation range {reloc:?} for symbol {sym:?} {region:?}"
                            )
                        }
                        RangeComp::Left => {} // its next symbol - save range and go to next symbol
                    };
                    if end != *start {
                        map.insert(
                            sym,
                            RelocRange {
                                relocs: *start..end,
                            },
                        );
                        *start = end;
                    }
                    continue 'next_sym;
                }

                // No more relocs, but we still have symbols - just save existing range for them.
                if end != *start {
                    map.insert(
                        sym,
                        RelocRange {
                            relocs: *start..end,
                        },
                    );
                    *start = end;
                }
            }
        }

        code_regions.sort_by_key(|(r, _)| r.start);
        data_regions.sort_by_key(|(r, _)| r.start);
        let mut code_owners = GappedMap::new();
        let mut data_owners = GappedMap::new();
        let start = &mut 0;

        move_relocs(&mut code_owners, code_regions, start, relocs);
        move_relocs(&mut data_owners, data_regions, start, relocs);

        (code_owners, data_owners)
    }

    fn iter_entities(&self) -> impl Iterator<Item = (EntityKind, Range<usize>)> + '_ {
        self.code_owners
            .iter()
            .map(|(k, v)| (EntityKind::Function(k), v.relocs.clone()))
            .chain(
                self.data_owners
                    .iter()
                    .map(|(k, v)| (EntityKind::DataSymbol(k), v.relocs.clone())),
            )
    }
    /// Iter over code and data relocs.
    pub fn iter_relocs(&self) -> impl Iterator<Item = (EntityKind, &[EntityRelocationEntry])> {
        self.iter_entities()
            .map(|(entity, range)| (entity, &self.array[range]))
    }

    /// Iter over code and data relocs.
    pub fn iter_relocs_mut(
        &mut self,
    ) -> impl Iterator<Item = (EntityKind, &mut [EntityRelocationEntry])> {
        FileRelocsIterMut {
            array: &mut self.array,
            truncated_len: 0,
            // embedded iter_entities to allow mutable access to array.
            item_iter: self
                .code_owners
                .iter()
                .map(|(k, v)| (EntityKind::Function(k), v.relocs.clone()))
                .chain(
                    self.data_owners
                        .iter()
                        .map(|(k, v)| (EntityKind::DataSymbol(k), v.relocs.clone())),
                ),
        }
    }

    pub fn get_data_relocs(&self, data_symbol: DataSymbolRef) -> Option<&[EntityRelocationEntry]> {
        self.data_owners
            .get(data_symbol)
            .map(|range| &self.array[range.relocs.clone()])
    }
    pub fn get_code_relocs(&self, func: FunctionRef) -> Option<&[EntityRelocationEntry]> {
        self.code_owners
            .get(func)
            .map(|range| &self.array[range.relocs.clone()])
    }

    pub fn get_entity_relocs(&self, entity: EntityKind) -> Option<&[EntityRelocationEntry]> {
        match entity {
            EntityKind::Function(func) => self.get_code_relocs(func),
            EntityKind::DataSymbol(data) => self.get_data_relocs(data),
            _ => None,
        }
    }
    // Ensure that addend increase is only applied to valid symbol types
    fn checked_addend_increase(addend: i64, increase: u32, symbol_type: SymbolType) -> i64 {
        match symbol_type {
            // only offsets in memory, or in file can be increased
            SymbolType::SectionOffset | SymbolType::FunctionOffset | SymbolType::MemoryAddr => {
                addend + increase as i64
            }
            _ if addend != 0 || increase != 0 => {
                panic!("Cannot increase addend for this symbol type")
            }
            _ => 0,
        }
    }
}

/// Iterate over relocations entities, ensure that only one entity bound to each relocation entry,
/// and ensure that all entries are ordered and covered.
// Items in array should be ordered and consistend with iterator order
struct FileRelocsIterMut<'a, I> {
    array: &'a mut [EntityRelocationEntry],
    truncated_len: usize,
    item_iter: I,
}

impl<'a, I> Iterator for FileRelocsIterMut<'a, I>
where
    I: Iterator<Item = (EntityKind, Range<usize>)>,
{
    type Item = (EntityKind, &'a mut [EntityRelocationEntry]);

    fn next(&mut self) -> Option<Self::Item> {
        if let Some((entity, range)) = self.item_iter.next() {
            debug_assert_eq!(range.start, self.truncated_len);
            let len = range.len();

            let array = std::mem::take(&mut self.array);

            let (head, tail) = array.split_at_mut(len);
            self.array = tail;
            self.truncated_len += len;

            return Some((entity, head));
        }
        debug_assert!(
            self.array.is_empty(),
            "Not all relocation entries were covered by iter"
        );
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SymbolOffset {
    // We can use `FlatEntityRef` or `TaggedEntityRef` but for first one, we also need track `EntitiesCount`
    // thats why we use `TaggedEntityRef` instead.
    // This also will enhance debugging.
    /// Reference to entity in wasm object.
    pub entity: EntityKind,
    /// If symbol was merged into another entity, this is the offset in that entity.
    /// Only aplicable for data symbols.
    pub offset_in_entity: u32,
    /// If one symbol replaces definition, previous symbol should be marked as "not used",
    /// to remove "relocs" that targets symbol content.
    /// (Like functionoffset, memoryaddrlocrel, etc).
    pub used_definition: bool,
}

impl SymbolOffset {
    pub fn new(entity: EntityKind) -> Self {
        Self {
            entity,
            offset_in_entity: 0,
            used_definition: true,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct FileSymbolDb {
    /// Map `SymbolId` from FILE linkage symbol table -> `EntityKind` in wasm object.
    ///
    /// Usecases:
    /// - reloc.* section contain `SymbolId` reference to this table.
    ///   But for our needs `EntityKind` is used.
    ///
    pub symbols: PrimaryMap<SymbolId, SymbolOffset>,
}

impl FileSymbolDb {
    /// Get symbol entity by its SymbolId
    #[inline]
    pub fn symbol_entity(&self, symbol_id: SymbolId) -> Option<&SymbolOffset> {
        self.symbols.get(symbol_id)
    }

    /// Get `EntityKind` for `FuncTypeId`.
    #[inline]
    pub fn resolve_type_id(&self, type_id: FnTypeRef) -> Option<EntityKind> {
        //TODO: currently not supported - so just copy as is.
        Some(EntityKind::Type(type_id))
    }
}

#[cfg(test)]
mod tests {
    use super::{FileRelocs, RelocRange};
    use crate::{index::GappedMap, typed::EntityKind};

    fn rebuild_relocs(relocs: &FileRelocs) -> FileRelocs {
        let mut new_array = Vec::new();
        let mut code_owners = GappedMap::new();
        let mut data_owners = GappedMap::new();

        for (entity, range) in relocs.iter_entities() {
            let original_relocs = &relocs.array[range.clone()];

            let start = new_array.len();
            new_array.extend_from_slice(original_relocs);
            let range = start..new_array.len();

            if range.is_empty() {
                continue;
            }
            match entity {
                EntityKind::Function(func) => {
                    code_owners.insert(func, RelocRange { relocs: range })
                }
                EntityKind::DataSymbol(data) => {
                    data_owners.insert(data, RelocRange { relocs: range })
                }
                _ => None,
            };
        }

        FileRelocs::build_from_parts(new_array.into_boxed_slice(), code_owners, data_owners)
    }
    #[test]
    fn test_rebuild_relocs() {
        let bytes = crate::testfiles::EXAMPLE_WASM;
        let loaded = crate::typed::LoadedFile::from_wasm_bytes(bytes).unwrap();

        let rebuilt = rebuild_relocs(&loaded.relocs);

        assert_eq!(loaded.relocs, rebuilt);
    }

    #[test]
    fn check_ordered() {
        let bytes = crate::testfiles::EXAMPLE_WASM;
        let loaded = crate::typed::LoadedFile::from_wasm_bytes(bytes).unwrap();

        loaded.relocs.ensure_ordered_no_gaps();

        let rebuilt = rebuild_relocs(&loaded.relocs);
        rebuilt.ensure_ordered_no_gaps();
    }

    #[test]
    fn check_multiple_files_iter_mut() {
        let bytes = crate::testfiles::EXAMPLE_WASM;
        check_iter_mut(bytes);

        let bytes = crate::testfiles::LAZY_ROUTES;
        check_iter_mut(bytes);
    }

    fn check_iter_mut(file: &[u8]) {
        let mut loaded = crate::typed::LoadedFile::from_wasm_bytes(file).unwrap();

        let mut copy = rebuild_relocs(&loaded.relocs);

        for (_, reloc) in loaded.relocs.iter_relocs_mut() {
            for reloc in reloc {
                reloc.addend += 1; // modify addend to check that we can modify relocs through iter_mut
            }
        }

        for ((_, left), (_, right)) in copy.iter_relocs_mut().zip(loaded.relocs.iter_relocs()) {
            for (l, r) in left.iter_mut().zip(right.iter()) {
                l.addend += 1; // moddify copied and check with changed.
                assert_eq!(&*l, r);
            }
        }
    }

    #[test]
    fn check_sections_range() {
        let bytes = crate::testfiles::EXAMPLE_WASM;
        let loaded = crate::typed::LoadedFile::from_wasm_bytes(bytes).unwrap();

        let code_relocs = loaded.relocs.get_code_section_relocs();
        let data_relocs = loaded.relocs.get_data_section_relocs();

        assert_eq!(
            code_relocs.len() + data_relocs.len(),
            loaded.relocs.array.len()
        );
        let first_code_range = loaded
            .relocs
            .code_owners
            .iter()
            .next()
            .unwrap()
            .1
            .relocs
            .clone();

        let last_data_range = loaded
            .relocs
            .data_owners
            .iter()
            .last()
            .unwrap()
            .1
            .relocs
            .clone();

        let first_entry = loaded.relocs.array.first().unwrap();
        let first_code_entry = loaded
            .relocs
            .array
            .get(first_code_range.clone())
            .unwrap()
            .first()
            .unwrap();

        let last_entry = loaded.relocs.array.last().unwrap();
        let last_data_entry = loaded
            .relocs
            .array
            .get(last_data_range.clone())
            .unwrap()
            .last()
            .unwrap();

        assert!(
            code_relocs.first().unwrap() == first_code_entry && first_entry == first_code_entry
        );
        assert!(data_relocs.last().unwrap() == last_data_entry && last_entry == last_data_entry);
    }
}
