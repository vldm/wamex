use std::{fmt::Debug, ops::Range};

use anyhow::Result;
use cranelift_entity::{EntityRef, PrimaryMap, packed_option::ReservedValue};

use crate::{
    helpers::{RangeComp, cmp_range},
    index::GappedMap,
    linkage::reloc::{
        AnyRelocationEntry, Encoding, Relative, RelocationEntry, RelocationWidth, SymbolType,
    },
    typed::{
        FnTypeRef, FunctionRef, SymbolId,
        common_index::{EntityKind, ErasedEntityRef},
        data::DataSymbolRef,
    },
};
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelocRange {
    pub(super) relocs: Range<usize>,
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

#[derive(Debug)]
pub struct FileRelocs {
    // Relocations ordered by offsets in file.
    //
    // Uses `ErasedEntityRef` as index, since relocation entry
    // already contain type information in external tag `SymbolType`
    array: Box<[RelocationEntry<ErasedEntityRef>]>,

    // In what symbol this relocation is placed
    code_owners: GappedMap<FunctionRef, RelocRange>,
    data_owners: GappedMap<DataSymbolRef, RelocRange>,
    // Elem/global doesn't have relocations.
    // TODO: custom section related relocs?
}

impl FileRelocs {
    /// Resolve relocations symbols (to corresponding entities).
    /// code_owners and data_owners should contain regions in original file that belongs to each symbol.
    pub fn build_relocs(
        file_relocs: impl IntoIterator<Item = AnyRelocationEntry>,
        file_db: &FileSymbolDb,
        regions: (
            Vec<(Range<usize>, FunctionRef)>,
            Vec<(Range<usize>, DataSymbolRef)>,
        ),
    ) -> Result<Self> {
        let mut array = file_relocs
            .into_iter()
            .map(|r| match r {
                AnyRelocationEntry::Linkage(l) => {
                    let offset_info = file_db
                        .symbol_entity(l.symbol_id)
                        .expect("Malformed wasm: relocation references unknown symbol");
                    RelocationEntry {
                        offset: l.offset,
                        symbol_id: Self::unwrap_entity_ref(offset_info.entity, l.symbol_type),
                        symbol_type: l.symbol_type,
                        encoding: l.encoding,
                        width: l.width,
                        relation: l.relation,
                        addend: Self::checked_addend_increase(
                            l.addend,
                            offset_info.offset_in_entity,
                            l.symbol_type,
                        ),
                    }
                }
                AnyRelocationEntry::Type(t) => RelocationEntry {
                    offset: t.offset,
                    symbol_id: file_db
                        .resolve_type_id(t.index)
                        .expect("Malformed wasm: relocation references unknown type"),
                    symbol_type: SymbolType::TypeIndex,
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
        Ok(Self {
            array,
            code_owners,
            data_owners,
        })
    }

    // Convert region based ranges to ranges in relocs array.
    fn build_owners(
        relocs: &[RelocationEntry<ErasedEntityRef>],
        (mut code_regions, mut data_regions): (
            Vec<(Range<usize>, FunctionRef)>,
            Vec<(Range<usize>, DataSymbolRef)>,
        ),
    ) -> (
        GappedMap<FunctionRef, RelocRange>,
        GappedMap<DataSymbolRef, RelocRange>,
    ) {
        /// Move relocations from flat list to symbols.
        ///
        fn move_relocs<'a, U: EntityRef + Debug>(
            map: &mut GappedMap<U, RelocRange>,
            symbol_regions: impl IntoIterator<Item = (Range<usize>, U)>,
            start: &mut usize,
            relocs: &[RelocationEntry<ErasedEntityRef>],
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
        let ref mut start = 0;

        move_relocs(&mut code_owners, code_regions.into_iter(), start, relocs);
        move_relocs(&mut data_owners, data_regions.into_iter(), start, relocs);

        (code_owners, data_owners)
    }

    /// Iter over code and data relocs.
    pub fn iter_relocs(
        &self,
    ) -> impl Iterator<Item = (EntityKind, &[RelocationEntry<ErasedEntityRef>])> {
        self.code_owners
            .iter()
            .map(|(k, v)| (EntityKind::Function(k), &self.array[v.relocs.clone()]))
            .chain(
                self.data_owners
                    .iter()
                    .map(|(k, v)| (EntityKind::DataSymbol(k), &self.array[v.relocs.clone()])),
            )
    }

    /// Convert enum to erased form.
    fn unwrap_entity_ref(src: EntityKind, symbol_type: SymbolType) -> ErasedEntityRef {
        match (src, symbol_type) {
            (
                EntityKind::Function(f),
                SymbolType::FunctionIndex | SymbolType::FunctionOffset | SymbolType::TableIndex,
            ) => ErasedEntityRef::from_u32(f.as_u32()),
            (EntityKind::Global(g), SymbolType::GlobalIndex) => {
                ErasedEntityRef::from_u32(g.as_u32())
            }
            (EntityKind::DataSymbol(d), SymbolType::MemoryAddrLocrel | SymbolType::MemoryAddr) => {
                ErasedEntityRef::from_u32(d.as_u32())
            }
            (EntityKind::Table(t), SymbolType::TableNumber) => {
                ErasedEntityRef::from_u32(t.as_u32())
            }
            (EntityKind::Memory(_) | EntityKind::Tag(_), _) => {
                panic!("Unsupported entity ref for relocation")
            }
            _ => panic!("Mismatched entity ref and symbol type"),
        }
    }
    // Ensure that addend increase is only applied to valid symbol types
    fn checked_addend_increase(addend: i64, increase: u32, symbol_type: SymbolType) -> i64 {
        match symbol_type {
            // only offsets in memory, or in file can be increased
            SymbolType::SectionOffset
            | SymbolType::FunctionOffset
            | SymbolType::MemoryAddrLocrel
            | SymbolType::MemoryAddr => addend + increase as i64,
            _ if addend != 0 || increase != 0 => {
                panic!("Cannot increase addend for this symbol type")
            }
            _ => 0,
        }
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
    pub used_defintion: bool,
}

impl SymbolOffset {
    pub fn new(entity: EntityKind) -> Self {
        Self {
            entity,
            offset_in_entity: 0,
            used_defintion: true,
        }
    }
}

#[derive(Debug)]
pub struct FileSymbolDb {
    /// Map `SymbolId` from linkage symbol table -> `EntityRef` in wasm object.
    ///
    /// Usecases:
    /// - reloc.* section contain `symbol_id` reference to this table.
    /// But for our needs `EntityRef` is used.
    ///
    pub symbols: PrimaryMap<SymbolId, SymbolOffset>,
}

impl FileSymbolDb {
    /// Get symbol entity by its SymbolId
    #[inline]
    pub fn symbol_entity(&self, symbol_id: SymbolId) -> Option<&SymbolOffset> {
        self.symbols.get(symbol_id)
    }

    /// Get `ErasedEntityRef` for `FuncTypeId`.
    #[inline]
    pub fn resolve_type_id(&self, type_id: FnTypeRef) -> Option<ErasedEntityRef> {
        //TODO: currently not supported - so just copy as is.
        Some(ErasedEntityRef::from_u32(type_id.as_u32()))
    }
}
