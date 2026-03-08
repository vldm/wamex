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
    linkage::{file_db::SymbolOffset, reloc::AnyRelocationEntry},
    typed::{
        FunctionRef, GlobalRef, Module, SymbolId, TableRef, TagRef,
        common_index::EntityKind,
        data::{DataDefined, DataSymbolRef},
    },
};

type Str<'a> = Cow<'a, str>;

pub struct LinkageInfo<'src> {
    pub file_symbol_db: FileSymbolDb,
    pub defined_data_symbols: Vec<(SymbolId, DataDefined<'src>)>,
    // pub relocations: Relocations,
}

impl<'src> LinkageInfo<'src> {
    pub fn from_reader(reader: &ObjectReader<'src>) -> Self {
        let mut id = SymbolId::from_u32(0);

        let mut symbols = PrimaryMap::new();
        // placeholders of future data entities
        let mut data_ref = DataSymbolRef::from_u32(0);
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
                    (idx, *name, flags)
                }
                SymbolInfo::Data {
                    flags,
                    name,
                    symbol: Some(defined),
                } => {
                    let idx = EntityKind::DataSymbol(data_ref);

                    defined_data_symbols
                        .push((id, DataDefined::from_defined(defined, (*name).into())));

                    data_ref = data_ref.next();
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

            #[allow(
                clippy::collapsible_if,
                reason = "it's more clear when insert in separate line"
            )]
            if let Some(name) = name {
                if let Some(prev) = name_to_entity.insert(name.into(), idx) {
                    error!(
                        "Duplicate symbol name: {}, previous: {:?}, current = {:?}, flags = {flags:#x}",
                        name, prev, idx
                    );
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
        let code = &input
            .relocs
            .relocs
            .get(input.code.section_index)
            .map(|r| &r.entries[..])
            .unwrap_or_default();
        let data = &input
            .relocs
            .relocs
            .get(input.data.section_index)
            .map(|r| &r.entries[..])
            .unwrap_or_default();

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
        }

        code.iter()
            .map(|entry| AnyRelocationEntry::from_raw(*entry, input.code.starting_offset as isize)) // save original offset
            .chain(data.iter().map(|entry| {
                AnyRelocationEntry::from_raw(*entry, input.data.starting_offset as isize)
            }))
    }

    /// Returns regions of code and data symbols in the original module:
    /// - for each functions body
    /// - for each data chunks
    pub fn build_regions(input: &Module) -> file_db::Regions {
        let mut code_owners = Vec::with_capacity(input.functions.defined.len());
        let mut data_owners = Vec::with_capacity(input.data.defined.len());

        for (func_ref, func) in input.functions.defined_iter() {
            code_owners.push((func.original_range(), func_ref));
        }

        for (data_ref, data) in input.data.defined_iter() {
            data_owners.push((data.original_range(), data_ref));
        }

        (code_owners, data_owners)
    }
}
