use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, VecDeque},
    ops::Range,
};

use anyhow::{Result, ensure};
use cranelift_entity::{EntityRef, packed_option::ReservedValue};
pub use diff::{DiffEntry, DiffResult, Differ, StaticModuleInfo};
#[cfg(feature = "unstable")]
pub use diff::{SymbolMapWithContent, SymbolMapping};
use smallvec::SmallVec;
use wamex_types::map_vec::MiniSet;

use crate::{
    InputObject, ObjectReader,
    helpers::{RangeComp, RangeExt, ShiftMap},
    index::{GappedMap, IdVec},
    read::{
        FunctionRef, GlobalRef, TableRef,
        raw::{DataSegmentId, DefinedFuncId},
    },
};
mod diff;
// mod reloc;

impl_entity_index! {
    #[display = ""] // Basic symbol no need prefix for display
    pub struct SymbolId(for<'a> SymbolRecord<'a>);
}

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum SymbolKind {
    Func {
        input_id: FunctionRef,
    },
    DataDefined {
        segment_id: DataSegmentId,
        offset: usize,
        length: usize,
    },
    Global(GlobalRef),
    Table(TableRef),
    Duplicate(SymbolId),
}

#[derive(PartialEq, Eq, Debug, Clone)]
pub struct SymbolRecord<'a> {
    /// From Name + linking section, fail in case of conflicts.
    /// undefined and non-named
    pub debug_name: Cow<'a, str>,
    pub linking_name: Option<Cow<'a, str>>,
    pub flags: wasmparser::SymbolFlags,
    pub kind: SymbolKind,
    /// Relocation entries inside this symbol
    /// Every index in relocation entry is corresponding to one symbol in symbol table.
    /// Except for TypeIndexLeb relocations, which is in type index space.
    pub relocs: Vec<wasmparser::RelocationEntry>,
    /// Map of symbols that use this symbol in their relocations.
    pub reloc_users: MiniSet<SymbolId>, //TODO: Replace by `MiniSet`
}

impl SymbolRecord<'_> {
    fn apply_empty_relocs(body: &mut [u8], relocations: &[wasmparser::RelocationEntry]) {
        for rel in relocations {
            let reloc_range = rel.relocation_range();
            body[reloc_range].fill(0);
        }
    }

    pub fn stable_name(&self) -> Option<&str> {
        match self.kind {
            SymbolKind::Func { .. } => Some(&self.debug_name),
            _ => None,
        }
    }

    // Return content with cleared relocations.
    pub fn stable_content(&self, input: &InputObject) -> Option<Vec<u8>> {
        match self.kind {
            SymbolKind::Func { input_id } => {
                let Some(defined_id) = input.as_defined_function_id(input_id) else {
                    // No content for imported functions
                    return None;
                };

                let func = &input.wasm_reader.code.defined_funcs[defined_id];
                let mut body = func.body.as_bytes().to_vec();
                Self::apply_empty_relocs(&mut body, &self.relocs);
                Some(body)
            }
            SymbolKind::DataDefined {
                segment_id,
                offset,
                length,
            } => {
                let segment = &input.wasm_reader.data.data_segments[segment_id];
                let start = offset;
                let end = start + length;
                let mut data = segment.data[start..end].to_vec();
                Self::apply_empty_relocs(&mut data, &self.relocs);
                Some(data)
            }
            SymbolKind::Global(_) | SymbolKind::Table(_) | SymbolKind::Duplicate(_) => {
                // No content for globals/tables/duplicates
                None
            }
        }
    }

    pub fn childs(&self) -> impl Iterator<Item = SymbolId> + '_ {
        let filter_non_types = |reloc: &&wasmparser::RelocationEntry| {
            !matches!(reloc.ty, wasmparser::RelocationType::TypeIndexLeb)
        };

        let duplicate_iter = match self.kind {
            SymbolKind::Duplicate(original_id) => Some(original_id),
            _ => None,
        };
        self.relocs
            .iter()
            .filter(filter_non_types)
            .map(|reloc| SymbolId::from_u32(reloc.index))
            .chain(duplicate_iter)
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
struct DataSymbolKey {
    data_segment: DataSegmentId,
    offset: usize,
    symbol_id: SymbolId,
}

#[derive(Clone, Default, Debug)]
pub struct Symbols<'src> {
    symbols: IdVec<SymbolRecord<'src>>,
    funcs_ids: GappedMap<FunctionRef, SymbolId>,
    datas_ids: BTreeSet<DataSymbolKey>,
}

impl<'src> Symbols<'src> {
    pub fn empty() -> Self {
        Self {
            symbols: IdVec::new(),
            funcs_ids: GappedMap::new(),
            datas_ids: BTreeSet::new(),
        }
    }
    pub fn new(
        wasm: &'_ crate::read::ObjectReader<'src>,
        num_imports_fn: usize,
        remove_duplicates: bool,
    ) -> Result<Self> {
        use wasmparser::SymbolInfo;

        #[derive(Debug, Clone)]
        struct SymbolRange {
            id: SymbolId,
            range: Range<usize>,
        }
        struct SymIm<'src> {
            flags: wasmparser::SymbolFlags,
            name: Cow<'src, str>,
            linking_name: Option<&'src str>,
            symbol_kind: SymbolKind,
        }

        #[derive(Clone)]
        struct DupForRange(SymbolRange, DupIds);
        impl ReservedValue for DupForRange {
            fn reserved_value() -> Self {
                Self(
                    SymbolRange {
                        id: SymbolId::reserved_value(),
                        range: 0..0,
                    },
                    SmallVec::new(),
                )
            }
            fn is_reserved_value(&self) -> bool {
                self.0.id.is_reserved_value()
            }
        }

        let (code_relocs, data_relocs) = Self::collect_ordered_relocs(wasm)?;
        type DupIds = SmallVec<[SymbolId; 4]>;
        let mut func_ids = GappedMap::<FunctionRef, DupForRange>::new();
        // TODO: Handle symbols that overlap in data segments.
        let mut data_ids = BTreeMap::<DataSymbolKey, SymbolRange>::new();

        let mut symbols = IdVec::default();

        let data_section_start = wasm.data.starting_offset;

        for (symbol_id, symbol) in wasm.linking.linking_symbols.symbols.iter().enumerate() {
            let symbol_id = SymbolId::new(symbol_id);

            let sym = match *symbol {
                SymbolInfo::Func { index, flags, name } => {
                    let input_function_id = FunctionRef::from_u32(index);
                    let fn_name = Self::get_or_create_name(
                        name,
                        wasm.names
                            .functions
                            .get(input_function_id)
                            .map(|n| n.into_inner()),
                        || panic!("function {index} does not have a name"),
                    );

                    let fn_range = if input_function_id.index() >= num_imports_fn {
                        let defined_index = input_function_id.index() - num_imports_fn;
                        let func = wasm
                            .code
                            .defined_funcs
                            .get(DefinedFuncId::new(defined_index))
                            .expect("defined function id should be valid");
                        func.body.range().shift_left(wasm.code.starting_offset)
                    } else {
                        0..0
                    };

                    // TODO: Handle weak symbols.
                    let dup_funcs = func_ids.entry(input_function_id).or_insert(DupForRange(
                        SymbolRange {
                            id: symbol_id,
                            range: fn_range,
                        },
                        SmallVec::new(),
                    ));
                    dup_funcs.1.push(symbol_id);

                    SymIm {
                        flags,
                        name: fn_name,
                        linking_name: name,
                        symbol_kind: SymbolKind::Func {
                            input_id: input_function_id,
                        },
                    }
                }
                SymbolInfo::Data {
                    flags,
                    name,
                    symbol,
                } => {
                    let Some(defined) = symbol.as_ref() else {
                        log::warn!(
                            "Skipping undefined data symbol '{}' in linking section",
                            name
                        );
                        continue;
                    };

                    let segment_id = DataSegmentId::from_u32(defined.index);
                    let segment = &wasm.data.data_segments[segment_id];

                    // Remove header size from segment range.
                    let segment_data_start = segment.range.end - segment.data.len();
                    // offset of segment relative to data section start
                    let segment_offset = segment_data_start - data_section_start;
                    let segment_end = segment_offset + segment.data.len();

                    let start = segment_offset + defined.offset as usize;
                    let end = start + defined.size as usize;

                    ensure!(
                        end <= segment_end,
                        "Data symbol '{}' extends beyond segment",
                        name
                    );

                    let data_range = start..end;
                    data_ids.insert(
                        DataSymbolKey {
                            data_segment: segment_id,
                            offset: start,
                            symbol_id,
                        },
                        SymbolRange {
                            id: symbol_id,
                            range: data_range,
                        },
                    );

                    SymIm {
                        flags,
                        name: name.into(),
                        linking_name: name.into(),
                        symbol_kind: SymbolKind::DataDefined {
                            segment_id,
                            offset: defined.offset as usize,
                            length: defined.size as usize,
                        },
                    }
                }
                SymbolInfo::Global { flags, name, index } => {
                    let global_id = GlobalRef::from_u32(index);
                    let global_name = Self::get_or_create_name(
                        name,
                        wasm.names.globals.get(global_id).map(|n| n.into_inner()),
                        || format!("global_{index}"),
                    );
                    SymIm {
                        flags,
                        name: global_name,
                        linking_name: name,
                        symbol_kind: SymbolKind::Global(global_id),
                    }
                }
                SymbolInfo::Table { index, name, flags } => {
                    let table_id = TableRef::from_u32(index);
                    let table_name = Self::get_or_create_name(
                        name,
                        wasm.names.tables.get(table_id).map(|n| n.into_inner()),
                        || format!("table_{}", table_id),
                    );
                    SymIm {
                        flags,
                        name: table_name,
                        linking_name: name,
                        symbol_kind: SymbolKind::Table(table_id),
                    }
                }
                // TODO: Handle other symbol types.
                SymbolInfo::Event { .. } | SymbolInfo::Section { .. } => {
                    continue;
                }
            };

            let id = symbols.push(SymbolRecord {
                debug_name: sym.name,
                linking_name: sym.linking_name.map(From::from),
                flags: sym.flags,
                kind: sym.symbol_kind,
                relocs: Vec::new(),
                reloc_users: MiniSet::new(),
            });
            debug_assert_eq!(id, symbol_id)
        }

        /// Move relocations from flat list to symbols.
        ///
        /// And mark reloc_users.
        ///
        fn move_relocs<'a, U: 'a>(
            symbols: &mut IdVec<SymbolRecord>,
            iterator: impl IntoIterator<Item = (U, &'a SymbolRange)>,
            mut relocations: VecDeque<wasmparser::RelocationEntry>,
        ) {
            'next_sym: for (_, symbol) in iterator {
                while let Some(reloc) = relocations.front() {
                    match symbol.range.cmp_range(reloc.relocation_range()) {
                        RangeComp::Equal | RangeComp::Overlap => {}
                        RangeComp::Left => continue 'next_sym,
                        RangeComp::Right | RangeComp::NonComparable | RangeComp::Within => {
                            panic!(
                                "BUG: Relocation entry is not related to symbols: {symbol:?} and {reloc:?}",
                            );
                        }
                    }
                    // Collect relocations with offset relative to symbol start
                    let mut reloc = relocations.pop_front().unwrap();
                    reloc.offset -= symbol.range.start as u32;
                    symbols[symbol.id].relocs.push(reloc);

                    // Record that this symbol uses the target symbol of the relocation
                    // skip TypeIndexLeb because it contain type index, not symbol index
                    if reloc.ty != wasmparser::RelocationType::TypeIndexLeb {
                        let target_symbol_id = SymbolId::from_u32(reloc.index);
                        symbols[target_symbol_id].reloc_users.insert(symbol.id);
                    }
                }
            }
        }
        // Move relocs from duplicate symbols to main symbol, and mark duplicates.
        fn move_dup_symbols(
            this: &mut Symbols,
            func_ids: GappedMap<FunctionRef, DupForRange>,
        ) -> ShiftMap<SymbolId> {
            let mut removed = ShiftMap::new();
            for (_input_id, DupForRange(sym_range, dup_ids)) in func_ids.iter() {
                if dup_ids.len() <= 1 {
                    continue;
                }

                for &dup_id in dup_ids.iter() {
                    if dup_id == sym_range.id {
                        continue;
                    }
                    // Notify all users of duplicate symbol to use new one.
                    this.replace_usage(dup_id, sym_range.id, 0);

                    // take relocs, and mark as duplicate
                    let dup_sym = &mut this.symbols[dup_id];
                    let relocs = std::mem::take(&mut dup_sym.relocs);
                    let relocs_users = std::mem::take(&mut dup_sym.reloc_users);
                    dup_sym.kind = SymbolKind::Duplicate(sym_range.id);
                    // extend main symbol with
                    this.symbols[sym_range.id].relocs.extend(relocs.into_iter());
                    this.symbols[sym_range.id]
                        .reloc_users
                        .extend(relocs_users.into_iter());
                    // order relocs
                    this.symbols[sym_range.id].relocs.sort_by_key(|r| r.offset);

                    removed.remove(dup_id, 1); // always remove 1
                }
            }
            removed
        }

        // 1. Get ordered relocs (data segments, code).
        // 2. Iterate symbols by function_id, and get pop relocs untill it in range of function body.
        // 3. same for data relocs.
        move_relocs(
            &mut symbols,
            func_ids.iter().map(|(k, DupForRange(v, _))| (k, v)),
            code_relocs,
        );
        move_relocs(&mut symbols, data_ids.iter(), data_relocs);
        let mut this = Self {
            symbols,
            funcs_ids: func_ids
                .iter()
                .map(|(k, DupForRange(v, _))| (k, v.id))
                .collect(),
            datas_ids: data_ids.into_iter().map(|(k, _)| k).collect(),
        };

        let removed = move_dup_symbols(&mut this, func_ids);
        // Optional phase that shifts all non-used symbols out.
        let this = if remove_duplicates {
            this.shrink_table(removed)
        } else {
            this
        };
        // We can now remove
        Ok(this)
    }

    pub fn clone_owned(&self) -> Symbols<'static> {
        let symbols = self
            .symbols
            .iter()
            .map(|(_id, sym)| SymbolRecord {
                debug_name: sym.debug_name.clone().into_owned().into(),
                linking_name: sym.linking_name.clone().map(|n| Cow::Owned(n.into_owned())),
                flags: sym.flags,
                relocs: sym.relocs.clone(),
                kind: sym.kind,
                reloc_users: sym.reloc_users.clone(),
            })
            .collect();
        Symbols {
            symbols,
            funcs_ids: self.funcs_ids.clone(),
            datas_ids: self.datas_ids.clone(),
        }
    }

    pub fn get_mut(&mut self, id: SymbolId) -> Option<&mut SymbolRecord<'src>> {
        self.symbols.get_mut(id)
    }

    // TODO: Build remap index instead of handling duplicates here.
    pub fn get(&self, id: SymbolId) -> Option<&SymbolRecord<'src>> {
        self.symbols.get(id).map(|sym| match &sym.kind {
            SymbolKind::Duplicate(original_id) => &self.symbols[*original_id],
            _ => sym,
        })
    }
    pub fn get_function_symbol(&self, func_id: FunctionRef) -> Option<SymbolId> {
        self.funcs_ids.get(func_id).copied()
    }

    pub fn as_input_function(&self, id: SymbolId) -> Option<FunctionRef> {
        match &self.symbols.get(id)?.kind {
            SymbolKind::Func { input_id } => Some(*input_id),
            _ => None,
        }
    }
    pub fn as_duplicate_mapped(&self, id: SymbolId) -> Option<SymbolId> {
        match &self.symbols.get(id)?.kind {
            SymbolKind::Duplicate(original_id) => Some(*original_id),
            _ => None,
        }
    }

    pub fn is_function(&self, id: SymbolId) -> bool {
        self.as_input_function(id).is_some()
    }
    pub fn is_data(&self, id: SymbolId) -> bool {
        matches!(self.symbols[id].kind, SymbolKind::DataDefined { .. })
    }

    fn get_or_create_name<'a>(
        linking_name: Option<&'a str>,
        name_section_name: Option<&'a str>,
        create_name: impl Fn() -> String,
    ) -> Cow<'a, str> {
        match (linking_name, name_section_name) {
            (Some(linking), Some(name_section)) => {
                if linking != name_section {
                    // This is not an error - linking section contain internal name of symbol, while name_section may contain #[demangle] name.
                    log::trace!(
                        "Conflicting names for symbol: linking section name '{linking}', name section name '{name_section}'"
                    );
                }
                name_section.into()
            }
            (Some(linking), None) => linking.into(),
            (None, Some(name_section)) => name_section.into(),
            (None, None) => create_name().into(),
        }
    }

    // Return a list of all relocations in the module, ordered by their offset.
    fn collect_ordered_relocs(
        input: &'_ ObjectReader<'src>,
    ) -> Result<(
        VecDeque<wasmparser::RelocationEntry>,
        VecDeque<wasmparser::RelocationEntry>,
    )> {
        // Build a flat list of all relocations, adjusting offsets to be relative to the start of the module
        let mut code = input.relocs.relocs[input.code.section_index]
            .entries
            .clone();
        let mut data = input.relocs.relocs[input.data.section_index]
            .entries
            .clone();

        code.sort_by_key(|r| r.offset);
        data.sort_by_key(|r| r.offset);
        // make sure entries do not overlap
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

        Ok((code.into(), data.into()))
    }

    pub fn print_debug(&self) {
        for (id, symbol) in self.symbols.iter() {
            println!("---{id} <{name}>", name = &symbol.debug_name);
            println!("    users: {:?}", symbol.reloc_users);
            println!("    records: {:?}", symbol);
            for reloc in &symbol.relocs {
                let id = SymbolId::from_u32(reloc.index);
                println!(
                    "    -->{id} <{name}> reloc{:?}",
                    reloc,
                    name = self.symbols[id].debug_name,
                );
            }
        }
    }

    pub fn iter_data_symbols(
        &self,
    ) -> impl Iterator<Item = (DataSegmentId, SymbolId, &SymbolRecord<'src>)> {
        self.datas_ids.iter().map(|key| {
            (
                key.data_segment,
                key.symbol_id,
                &self.symbols[key.symbol_id],
            )
        })
    }
    pub fn iter_data_symbols_for_segment(
        &self,
        segment_id: DataSegmentId,
    ) -> impl Iterator<Item = (SymbolId, &SymbolRecord<'src>)> {
        self.datas_ids
            .range(
                DataSymbolKey {
                    data_segment: segment_id,
                    offset: 0,
                    symbol_id: SymbolId::reserved_value(),
                }..DataSymbolKey {
                    data_segment: DataSegmentId::from_u32(segment_id.as_u32() + 1),
                    offset: 0,
                    symbol_id: SymbolId::reserved_value(),
                },
            )
            .map(|key| (key.symbol_id, &self.symbols[key.symbol_id]))
    }
    pub fn iter(&self) -> impl Iterator<Item = (SymbolId, &SymbolRecord<'src>)> {
        self.symbols.iter()
    }

    /// Replace symbol usage in all relocations.
    pub fn replace_usage(&mut self, id: SymbolId, new_id: SymbolId, addend_change: i64) {
        // take it to avoid &mut symbols aliasing issues
        let users = std::mem::take(&mut self.symbols[id].reloc_users);
        let raw_id = id.as_u32();
        for user_id in &users {
            // patch relocs in user symbol
            let user_sym = &mut self.symbols[*user_id];
            for reloc in &mut user_sym.relocs {
                if reloc.index != raw_id || reloc.ty == wasmparser::RelocationType::TypeIndexLeb {
                    continue;
                }
                // index is a symbol which <address> should be placed somewhere at `offset`
                reloc.index = new_id.as_u32();
                // addend is a value that should be added to the symbol <address>
                // `replace_usage` is operation of merging (embedding) two symbols, so we need to adjust addend accordingly
                reloc.addend = reloc.addend + addend_change;
            }
        }
        self.symbols[id].reloc_users = users;
    }

    /// Rebuild symbol tables using `ShiftMap`.
    /// `ShiftMap` contain mapping from old symbol id to new symbol id.
    /// Expects that `ShiftMap` does not contain any "inserts", only `removals` is allowed.
    ///
    /// Update all relocations to use new symbol ids.
    ///
    /// Make sure that 'linking_name' is not important and can be lost.
    /// Also if this is called as part of moving operation, make sure to also update `relocs` and `reloc_users` accordingly.
    pub fn shrink_table(self, shifts: ShiftMap<SymbolId>) -> Self {
        let mut symbols = IdVec::new();
        let mut funcs_ids = GappedMap::<FunctionRef, SymbolId>::new();
        let mut datas_ids = BTreeSet::<DataSymbolKey>::new();

        for (id, mut symbol) in self.symbols.into_inner().into_iter() {
            let Some(new_id_expected) = shifts.get_shifted_offset(id) else {
                continue;
            };
            // update relocs and reloc_users
            for reloc in &mut symbol.relocs {
                if reloc.ty == wasmparser::RelocationType::TypeIndexLeb {
                    continue;
                }
                // skip removed relocs
                let new_idx = shifts
                    .get_shifted_offset(SymbolId::from_u32(reloc.index))
                    .unwrap_or_else(|| {
                        panic!(
                            "Relocation in symbol {id} points to removed symbol: {:?}",
                            reloc
                        );
                    });
                reloc.index = new_idx.as_u32();
            }
            for user_id in std::mem::take(&mut symbol.reloc_users) {
                // skip removed users
                let new_user_id = shifts.get_shifted_offset(user_id).unwrap_or_else(|| {
                    panic!("Relocation user of symbol {id} points to removed symbol: {user_id}");
                });
                symbol.reloc_users.insert(new_user_id);
            }

            let new_id = symbols.push(symbol);
            debug_assert_eq!(new_id.as_u32(), new_id_expected.as_u32());
            match &symbols[new_id].kind {
                SymbolKind::Func { input_id } => {
                    funcs_ids.insert(*input_id, new_id);
                }
                SymbolKind::DataDefined {
                    segment_id, offset, ..
                } => {
                    datas_ids.insert(DataSymbolKey {
                        data_segment: *segment_id,
                        offset: *offset,
                        symbol_id: new_id,
                    });
                }
                _ => {}
            }
        }
        Self {
            symbols,
            funcs_ids,
            datas_ids,
        }
    }
}
