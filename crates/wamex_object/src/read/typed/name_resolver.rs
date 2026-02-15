use std::{borrow::Cow, collections::HashMap, ops::Range};

use anyhow::Result;
use cranelift_entity::{PrimaryMap, packed_option::ReservedValue};
use log::error;
use wasmparser::SymbolInfo;

use crate::{
    Module, ObjectReader,
    index::GappedMap,
    read::{
        FuncTypeId, FunctionRef, GlobalRef, TableRef, TagRef,
        common_index::{EntitiesSnapshot, ErasedEntityRef, TaggedEntityRef},
        typed::{common_index::AnyEntityRef, data::DataDefined},
    },
    symbols::{
        SymbolId,
        reloc::{
            AnyRelocationEntry, Encoding, Relative, RelocationEntry, RelocationWidth, SymbolType,
        },
    },
};

pub struct NameResolver<'src> {
    names: std::collections::HashMap<Cow<'src, str>, AnyEntityRef>,
}

impl<'src> NameResolver<'src> {
    pub fn new() -> Self {
        Self {
            names: std::collections::HashMap::new(),
        }
    }

    pub fn get(&self, name: &str) -> Option<AnyEntityRef> {
        self.names.get(name).copied()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelocRange {
    relocs: Range<usize>,
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
pub struct Relocations {
    // Relocations ordered by offsets in file.
    //
    // Uses `ErasedEntityRef` as index, since relocation entry
    // already contain type information in external tag `SymbolType`
    array: Box<[RelocationEntry<ErasedEntityRef>]>,

    // In what symbol this relocation is placed
    owners: GappedMap<AnyEntityRef, RelocRange>,
}

impl Relocations {
    // Resolve relocations symbols (to corresponding entities).
    pub fn build_relocs(
        file_relocs: impl IntoIterator<Item = AnyRelocationEntry>,
        file_db: &FileSymbolDb,
        owners: GappedMap<AnyEntityRef, RelocRange>,
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

        Ok(Self { array, owners })
    }

    pub fn get(&self, owner: AnyEntityRef) -> &[RelocationEntry<ErasedEntityRef>] {
        let Some(range) = self.owners.get(owner) else {
            return &[];
        };

        &self.array[range.relocs.clone()]
    }

    pub fn get_mut(&mut self, owner: AnyEntityRef) -> &mut [RelocationEntry<ErasedEntityRef>] {
        let Some(range) = self.owners.get(owner) else {
            return &mut [];
        };
        &mut self.array[range.relocs.clone()]
    }
    /// Convert enum to erased form.
    fn unwrap_entity_ref(src: TaggedEntityRef, symbol_type: SymbolType) -> ErasedEntityRef {
        match (src, symbol_type) {
            (
                TaggedEntityRef::Function(f),
                SymbolType::FunctionIndex | SymbolType::FunctionOffset | SymbolType::TableIndex,
            ) => ErasedEntityRef::from_u32(f.as_u32()),
            (TaggedEntityRef::Global(g), SymbolType::GlobalIndex) => {
                ErasedEntityRef::from_u32(g.as_u32())
            }
            (
                TaggedEntityRef::DataSymbol(d),
                SymbolType::MemoryAddrLocrel | SymbolType::MemoryAddr,
            ) => ErasedEntityRef::from_u32(d.as_u32()),
            (TaggedEntityRef::Table(t), SymbolType::TableNumber) => {
                ErasedEntityRef::from_u32(t.as_u32())
            }
            (TaggedEntityRef::Memory(_) | TaggedEntityRef::Tag(_), _) => {
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
    // We can use `AnyEntityRef` or `TaggedEntityRef` but for first one, we also need track `EntitiesCount`
    // thats why we use `TaggedEntityRef` instead.
    // This also will enhance debugging.
    /// Reference to entity in wasm object.
    pub entity: TaggedEntityRef,
    /// If symbol was merged into another entity, this is the offset in that entity.
    /// Only aplicable for data symbols.
    pub offset_in_entity: u32,
    /// If one symbol replaces definition, previous symbol should be marked as "not used",
    /// to remove "relocs" that targets symbol content.
    /// (Like functionoffset, memoryaddrlocrel, etc).
    pub used_defintion: bool,
}
impl SymbolOffset {
    pub fn new(entity: TaggedEntityRef) -> Self {
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
    pub fn resolve_type_id(&self, type_id: FuncTypeId) -> Option<ErasedEntityRef> {
        //TODO: currently not supported - so just copy as is.
        Some(ErasedEntityRef::from_u32(type_id.as_u32()))
    }
}

pub struct LinkageInfo {
    pub file_symbol_db: FileSymbolDb,
    pub defined_data_symbols: Vec<(SymbolId, DataDefined)>,
    // pub defined_data_symbols: PrimaryMap<DataSymbolRef, DataDefined>,
    // pub relocations: Relocations,
}

type Str<'a> = Cow<'a, str>;
impl LinkageInfo {
    pub fn from_reader(reader: &ObjectReader) -> Self {
        let mut id = SymbolId::from_u32(0);

        let mut symbols = PrimaryMap::new();
        // placeholders of future data entities
        let mut defined_data_ids = PrimaryMap::new();
        // symbol ids of defined data symbols
        let mut defined_data_symbols = Vec::new();
        let mut name_to_entity: HashMap<Str<'_>, TaggedEntityRef> = HashMap::new();

        for sym in reader.linking.linking_symbols.symbols.iter() {
            let (idx, name, flags) = match sym {
                SymbolInfo::Func { flags, index, name } => {
                    let func_index = FunctionRef::from_u32(*index);
                    let idx = TaggedEntityRef::Function(func_index);
                    (idx, *name, flags)
                }
                SymbolInfo::Event { flags, index, name } => {
                    let tag_ref = TagRef::from_u32(*index);
                    let idx = TaggedEntityRef::Tag(tag_ref);
                    (idx, *name, flags)
                }
                SymbolInfo::Global { flags, index, name } => {
                    let global_ref = GlobalRef::from_u32(*index);
                    let idx = TaggedEntityRef::Global(global_ref);
                    (idx, *name, flags)
                }
                SymbolInfo::Table { flags, index, name } => {
                    let table_index = TableRef::from_u32(*index);
                    let idx = TaggedEntityRef::Table(table_index);
                    error!("Unsupported symbol: table symbol");
                    (idx, *name, flags)
                }
                SymbolInfo::Data {
                    flags,
                    name,
                    symbol: Some(defined),
                } => {
                    let data_ref = defined_data_ids.push(());
                    let idx = TaggedEntityRef::DataSymbol(data_ref);

                    defined_data_symbols.push((id, DataDefined::from(defined)));
                    (idx, Some(*name), flags)
                }
                SymbolInfo::Data { symbol: None, .. } => {
                    // for linker it is just imported data symbol.
                    error!("Unsupported symbol: data symbol without definition");

                    id = id.next();
                    continue;
                }
                SymbolInfo::Section { .. } => {
                    error!("Unsupported symbol: section symbol");

                    id = id.next();
                    continue;
                }
            };

            if let Some(name) = name {
                if name_to_entity.insert(name.into(), idx).is_some() {
                    error!("Duplicate symbol name: {}", name);
                }
            }

            let real_id = symbols.push(SymbolOffset::new(idx));
            assert_eq!(id, real_id, "Some symbol was skipped");

            id = id.next();
        }

        defined_data_symbols.sort_by_key(|(_, d)| (d.segment_id, d.range.start));
        Self {
            file_symbol_db: FileSymbolDb { symbols },
            defined_data_symbols,
        }
    }

    pub fn collect_ordered_relocs(
        input: &ObjectReader,
    ) -> impl Iterator<Item = AnyRelocationEntry> {
        // Build a flat list of all relocations, adjusting offsets to be relative to the start of the module
        let code = &input.relocs.relocs[input.code.section_index].entries;
        let data = &input.relocs.relocs[input.data.section_index].entries;

        // make sure entries do not overlap and ordered
        #[cfg(debug_assertions)]
        {
            for pair in code.windows(2) {
                let first = &pair[0];
                let second = &pair[1];
                let first_end = first.relocation_range().end as u32;
                if first_end > second.offset {
                    panic!(
                        "Overlapping relocations found: first={first:?} (end={first_end}), second={second:?}"
                    );
                }
            }
            for pair in data.windows(2) {
                let first = &pair[0];
                let second = &pair[1];
                let first_end = first.relocation_range().end as u32;
                if first_end > second.offset {
                    panic!(
                        "Overlapping relocations found: first={first:?} (end={first_end}), second={second:?}"
                    );
                }
            }
        }

        code.into_iter()
            .map(|entry| AnyRelocationEntry::from_raw(*entry, 0)) // save original offset
            .chain(
                data.into_iter()
                    .map(|entry| AnyRelocationEntry::from_raw(*entry, 0)),
            )
    }

    // Build data and code parts:
    // - Functions body for code part
    // - Data chunks for data part
    pub fn build_owners(
        input: &Module,
        snapshot: EntitiesSnapshot,
    ) -> GappedMap<AnyEntityRef, RelocRange> {
        let mut owners = GappedMap::new();

        for (func_ref, func) in input.functions.defined_iter() {
            let entity_ref = TaggedEntityRef::Function(func_ref);

            owners.insert(
                snapshot.as_any_ref(&entity_ref),
                RelocRange {
                    relocs: func.body.range(),
                },
            );
        }

        for (data_ref, data) in input.data.iter() {
            let entity_ref = TaggedEntityRef::DataSymbol(data_ref);

            owners.insert(
                snapshot.as_any_ref(&entity_ref),
                RelocRange {
                    relocs: data.original_offset..data.original_offset + data.data.len(),
                },
            );
        }

        owners
    }
}
