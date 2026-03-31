//! Implementation of logic related to linkage:
//! - Symbol by name resolution
//! - Relocations implementation
//! - Entity resolution
//!
pub mod file_db;
pub mod name_resolver;
pub mod reloc;

use std::{borrow::Cow, collections::HashMap, ops::Range};

use cranelift_entity::{EntityRef, PrimaryMap};
use file_db::FileSymbolDb;
use log::error;
use wasmparser::SymbolInfo;

use crate::{
    ObjectReader,
    layouts::DataSymbolRef,
    linkage::{file_db::SymbolOffset, reloc::AnyRelocationEntry},
    raw::SegmentId,
    typed::{EntityKind, FunctionRef, GlobalRef, Module, SymbolId, TableRef, TagRef},
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DataDefined<'a> {
    pub segment_id: SegmentId,
    pub name: Cow<'a, str>,
    // Range of bytes in data segment related to this symbol
    pub range: Range<u32>,
}
impl<'a> DataDefined<'a> {
    pub fn from_defined(value: &wasmparser::DefinedDataSymbol, name: Cow<'a, str>) -> Self {
        Self {
            segment_id: SegmentId::from_u32(value.index),
            range: value.offset..(value.offset + value.size),
            name,
        }
    }
}
type Str<'a> = Cow<'a, str>;

pub struct LinkageInfo<'src> {
    pub file_symbol_db: FileSymbolDb,
    pub defined_data_symbols: Vec<(SymbolId, DataDefined<'src>, DataSymbolRef)>,
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

                    defined_data_symbols.push((
                        id,
                        DataDefined::from_defined(defined, (*name).into()),
                        data_ref,
                    ));

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
                let _g = tracing::trace_span!("adding symbol to resolution table", name = %name)
                    .entered();
                if let Some(prev) = name_to_entity.insert(name.into(), idx) {
                    error!(
                        "Duplicate symbol name: {}, previous: {:?}, current = {:?}, flags = {flags:#x}",
                        name, prev, idx
                    );
                }
            }

            let real_id = symbols.push(SymbolOffset::new(idx));
            debug_assert_eq!(id, real_id, "Some symbol was skipped");

            id = id.next();
        }

        let _g = tracing::debug_span!("Sorting defined data symbols").entered();
        defined_data_symbols.sort_by_key(|(_, d, _)| (d.segment_id, d.range.start));

        // renumerate data_symbols - to keep ids ordered
        for (new_ref, (sym_id, _, data_ref)) in defined_data_symbols.iter_mut().enumerate() {
            let new_ref = DataSymbolRef::new(new_ref);
            symbols[*sym_id].entity = new_ref.into();
            *data_ref = new_ref;
        }

        Self {
            file_symbol_db: FileSymbolDb { symbols },
            defined_data_symbols,
        }
    }

    #[tracing::instrument(skip_all)]
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
    #[tracing::instrument(skip_all)]
    pub fn build_regions(input: &Module) -> file_db::Regions {
        let mem_layout = &input.extra.mem_layout;

        let mut code_owners = Vec::with_capacity(input.functions.defined_iter().len());
        let mut data_owners = Vec::with_capacity(mem_layout.defined.len());

        for (func_ref, func) in input.functions.defined_iter() {
            code_owners.push((func.original_range(), func_ref));
        }
        for (data_ref, place) in mem_layout.item_places().iter() {
            let original_range = mem_layout.segments[place.segment_id].parts[place.part_id]
                .defined_entity
                .original_range();
            data_owners.push((original_range, data_ref))
        }
        (code_owners, data_owners)
    }
}
