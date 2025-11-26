use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, VecDeque},
    ops::Range,
};

use anyhow::{Result, ensure};
pub use diff::{DiffEntry, DiffResult, Differ, StaticModuleInfo};
use smallvec::SmallVec;

use crate::{
    InputModule, analysis,
    helpers::{RangeComp, RangeExt},
    index::{DataSegmentId, Id, IdMap, IdVec, InputFuncId, InputGlobalId, SymbolId, TableId},
};
mod diff;

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum SymbolKind {
    Func {
        input_id: InputFuncId,
    },
    DataDefined {
        segment_id: DataSegmentId,
        offset: usize,
        length: usize,
    },
    Global(InputGlobalId),
    Table(TableId),
    Duplicate(SymbolId),
}

#[derive(PartialEq, Eq, Debug, Clone)]
pub struct SymbolRecord<'a> {
    /// From Name + linking section, fail in case of conflicts.
    /// undefined and non-named
    pub name: Cow<'a, str>,
    pub linking_name: Option<Cow<'a, str>>,
    pub flags: wasmparser::SymbolFlags,
    /// Relocation entries inside this symbol
    /// Every index in relocation entry is corresponding to one symbol in symbol table.
    /// Except for TypeIndexLeb relocations, which is in type index space.
    pub relocs: Vec<wasmparser::RelocationEntry>,
    pub kind: SymbolKind,
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
            SymbolKind::Func { .. } => Some(&self.name),
            _ => None,
        }
    }

    // Return content with cleared relocations.
    pub fn stable_content(&self, input: &analysis::ModuleInfo) -> Option<Vec<u8>> {
        match self.kind {
            SymbolKind::Func { input_id } => {
                let Some(defined_id) = input.as_defined_function_id(input_id) else {
                    // No content for imported functions
                    return None;
                };

                let func = &input.wasm.code.defined_funcs[defined_id];
                let mut body = func.body.as_bytes().to_vec();
                Self::apply_empty_relocs(&mut body, &self.relocs);
                Some(body)
            }
            SymbolKind::DataDefined {
                segment_id,
                offset,
                length,
            } => {
                let segment = &input.wasm.data.data_segments[segment_id];
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
            .map(|reloc| Id::from_index(reloc.index))
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
pub struct SymbolMap<'src> {
    symbols: IdVec<SymbolRecord<'src>>,
    funcs_ids: IdMap<InputFuncId, SymbolId>,
    datas_ids: BTreeSet<DataSymbolKey>,
}

impl<'src> SymbolMap<'src> {
    pub fn empty() -> Self {
        Self {
            symbols: IdVec::new(),
            funcs_ids: IdMap::new(),
            datas_ids: BTreeSet::new(),
        }
    }
    pub fn new(wasm: &'_ crate::read::InputModule<'src>, num_imports_fn: usize) -> Result<Self> {
        use wasmparser::SymbolInfo;

        #[derive(Debug)]
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

        let (code_relocs, data_relocs) = Self::collect_ordered_relocs(wasm)?;

        type DupIds = SmallVec<[SymbolId; 4]>;
        let mut func_ids = IdMap::<InputFuncId, (SymbolRange, DupIds)>::new();
        // TODO: Handle symbols that overlap in data segments.
        let mut data_ids = BTreeMap::<DataSymbolKey, SymbolRange>::new();

        let mut symbols = IdVec::new();

        let data_section_start = wasm.data.starting_offset;

        for (symbol_id, symbol) in wasm.linking.linking_symbols.symbols.iter().enumerate() {
            let symbol_id = Id::from_index(symbol_id);

            let sym = match *symbol {
                SymbolInfo::Func { index, flags, name } => {
                    let input_function_id = Id::from_index(index);
                    let fn_name = Self::get_or_create_name(
                        name,
                        wasm.names.functions.get(input_function_id).map(|n| *n),
                        || panic!("function {index} does not have a name"),
                    );

                    let fn_range = if input_function_id.as_raw_index() >= num_imports_fn {
                        let defined_index = input_function_id.as_raw_index() - num_imports_fn;
                        let func = wasm
                            .code
                            .defined_funcs
                            .get(Id::from_index(defined_index))
                            .expect("defined function id should be valid");
                        func.body.range().shift_left(wasm.code.starting_offset)
                    } else {
                        0..0
                    };

                    // TODO: Handle weak symbols.
                    func_ids
                        .entry(input_function_id)
                        .or_insert_with(|| {
                            (
                                SymbolRange {
                                    id: symbol_id,
                                    range: fn_range,
                                },
                                DupIds::new(),
                            )
                        })
                        .1
                        .push(symbol_id);

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

                    let segment_id = Id::from_index(defined.index);
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
                    let global_id = Id::from_index(index);
                    let global_name = Self::get_or_create_name(
                        name,
                        wasm.names.globals.get(global_id).map(|n| *n),
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
                    let table_id = Id::from_index(index);
                    let table_name = Self::get_or_create_name(
                        name,
                        wasm.names.tables.get(table_id).map(|n| *n),
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
                name: sym.name,
                linking_name: sym.linking_name.map(From::from),
                flags: sym.flags,
                kind: sym.symbol_kind,
                relocs: Vec::new(),
            });
            debug_assert_eq!(id, symbol_id)
        }

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
                }
            }
        }
        // Move relocs from duplicate symbols to main symbol, and mark duplicates.
        fn move_dup_symbols(
            symbols: &mut IdVec<SymbolRecord>,
            func_ids: &IdMap<InputFuncId, (SymbolRange, DupIds)>,
        ) {
            for (_input_id, (sym_range, dup_ids)) in func_ids.iter() {
                if dup_ids.len() <= 1 {
                    continue;
                }

                for &dup_id in dup_ids.iter() {
                    if dup_id == sym_range.id {
                        continue;
                    }
                    // take relocs, and mark as duplicate
                    let dup_sym = &mut symbols[dup_id];
                    let relocs = std::mem::take(&mut dup_sym.relocs);
                    dup_sym.kind = SymbolKind::Duplicate(sym_range.id);
                    // extend main symbol with
                    symbols[sym_range.id].relocs.extend(relocs.into_iter());
                }
            }
        }

        // 1. Get ordered relocs (data segments, code).
        // 2. Iterate symbols by function_id, and get pop relocs untill it in range of function body.
        // 3. same for data relocs.
        move_relocs(
            &mut symbols,
            func_ids.iter().map(|(k, (v, _))| (k, v)),
            code_relocs,
        );
        move_relocs(&mut symbols, data_ids.iter(), data_relocs);
        move_dup_symbols(&mut symbols, &func_ids);

        Ok(Self {
            symbols,
            funcs_ids: func_ids.into_iter().map(|(k, (v, _))| (k, v.id)).collect(),
            datas_ids: data_ids.into_iter().map(|(k, _)| k).collect(),
        })
    }

    pub fn clone_owned(&self) -> SymbolMap<'static> {
        let symbols = self
            .symbols
            .iter()
            .map(|(_id, sym)| SymbolRecord {
                name: sym.name.clone().into_owned().into(),
                linking_name: sym.linking_name.clone().map(|n| Cow::Owned(n.into_owned())),
                flags: sym.flags,
                relocs: sym.relocs.clone(),
                kind: sym.kind,
            })
            .collect();
        SymbolMap {
            symbols,
            funcs_ids: self.funcs_ids.clone(),
            datas_ids: self.datas_ids.clone(),
        }
    }

    // TODO: Build remap index instead of handling duplicates here.
    pub fn get(&self, id: SymbolId) -> Option<&SymbolRecord<'src>> {
        self.symbols.get(id).map(|sym| match &sym.kind {
            SymbolKind::Duplicate(original_id) => &self.symbols[*original_id],
            _ => sym,
        })
    }
    pub fn get_function_symbol(&self, func_id: InputFuncId) -> Option<SymbolId> {
        self.funcs_ids.get(func_id).copied()
    }

    // pub fn get_data_symbol(&self, segment_id: DataSegmentId, offset: usize) -> Option<SymbolId> {
    //     self.datas_ids
    //         .get(&DataSymbolKey {
    //             data_segment: segment_id,
    //             offset,
    //         })
    //         .copied()
    // }

    pub fn as_input_function(&self, id: SymbolId) -> Option<InputFuncId> {
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
        input: &'_ InputModule<'src>,
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
            println!("---{id} <{name}>", name = &symbol.name);
            println!("    record: {:?}", symbol);
            for reloc in &symbol.relocs {
                let id = Id::from_index(reloc.index);
                println!(
                    "-->{id} <{name}> reloc{:?}",
                    reloc,
                    name = self.symbols[id].name,
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
    pub fn iter(&self) -> impl Iterator<Item = (SymbolId, &SymbolRecord<'src>)> {
        self.symbols.iter()
    }
}
