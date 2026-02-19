//! Implementation of logic related to linkage:
//! - Symbol by name resolution
//! - Relocations implementation
//! - Entity resolution
//!
pub mod file_db;
pub mod name_resolver;
pub mod reloc;

use std::{borrow::Cow, collections::HashMap};

use cranelift_entity::PrimaryMap;
use file_db::FileSymbolDb;
use log::error;
use wasmparser::SymbolInfo;

use crate::{
    ObjectReader,
    index::GappedMap,
    linkage::{
        file_db::{RelocRange, SymbolOffset},
        reloc::AnyRelocationEntry,
    },
    typed::{
        FunctionRef, GlobalRef, Module, SymbolId, TableRef, TagRef,
        common_index::EntityKind,
        data::{DataDefined, DataSymbolRef},
    },
};

type Str<'a> = Cow<'a, str>;

pub struct LinkageInfo {
    pub file_symbol_db: FileSymbolDb,
    pub defined_data_symbols: Vec<(SymbolId, DataDefined)>,
    // pub defined_data_symbols: PrimaryMap<DataSymbolRef, DataDefined>,
    // pub relocations: Relocations,
}

impl LinkageInfo {
    pub fn from_reader(reader: &ObjectReader) -> Self {
        let mut id = SymbolId::from_u32(0);

        let mut symbols = PrimaryMap::new();
        // placeholders of future data entities
        let mut defined_data_ids = PrimaryMap::new();
        // symbol ids of defined data symbols
        let mut defined_data_symbols = Vec::new();
        let mut name_to_entity: HashMap<Str<'_>, EntityKind> = HashMap::new();

        for sym in reader.linking.linking_symbols.symbols.iter() {
            let (idx, name, flags) = match sym {
                SymbolInfo::Func { flags, index, name } => {
                    let func_index = FunctionRef::from_u32(*index);
                    let idx = EntityKind::Function(func_index);
                    (idx, *name, flags)
                }
                SymbolInfo::Event { flags, index, name } => {
                    let tag_ref = TagRef::from_u32(*index);
                    let idx = EntityKind::Tag(tag_ref);
                    (idx, *name, flags)
                }
                SymbolInfo::Global { flags, index, name } => {
                    let global_ref = GlobalRef::from_u32(*index);
                    let idx = EntityKind::Global(global_ref);
                    (idx, *name, flags)
                }
                SymbolInfo::Table { flags, index, name } => {
                    let table_index = TableRef::from_u32(*index);
                    let idx = EntityKind::Table(table_index);
                    error!("Unsupported symbol: table symbol");
                    (idx, *name, flags)
                }
                SymbolInfo::Data {
                    flags,
                    name,
                    symbol: Some(defined),
                } => {
                    let data_ref = defined_data_ids.push(());
                    let idx = EntityKind::DataSymbol(data_ref);

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

    // Returns pair of maps of relocation owners:
    // - for each functions body
    // - for each data chunks
    pub fn build_owners(
        input: &Module,
    ) -> (
        GappedMap<FunctionRef, RelocRange>,
        GappedMap<DataSymbolRef, RelocRange>,
    ) {
        let mut code_owners = GappedMap::new();
        let mut data_owners = GappedMap::new();

        for (func_ref, func) in input.functions.defined_iter() {
            code_owners.insert(
                func_ref,
                RelocRange {
                    relocs: func.original_range(),
                },
            );
        }

        for (data_ref, data) in input.data.iter() {
            data_owners.insert(
                data_ref,
                RelocRange {
                    relocs: data.original_offset..data.original_offset + data.data.len(),
                },
            );
        }

        (code_owners, data_owners)
    }
}
