use std::{
    borrow::Cow,
    collections::{BTreeMap, VecDeque},
    ops::Range,
};

use anyhow::{ensure, Result};

use crate::{
    helpers::{RangeComp, RangeExt},
    index::{DataSegmentId, Id, IdMap, IdVec, InputFuncId, InputGlobalId, SymbolId, TableId},
    InputModule,
};

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
}

#[derive(PartialEq, Eq, Debug, Clone)]
pub struct SymbolRecord<'a> {
    /// From Name + linking section, fail in case of conflicts.
    /// undefined and non-named
    pub name: Cow<'a, str>,
    pub flags: wasmparser::SymbolFlags,
    /// Relocation entries inside this symbol
    pub relocs: Vec<wasmparser::RelocationEntry>,
    pub kind: SymbolKind,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct DataSymbolKey {
    data_segment: DataSegmentId,
    start: usize,
}

#[derive(Debug)]
pub struct SymbolMap<'src> {
    symbols: IdVec<SymbolRecord<'src>>,
    funcs_ids: IdMap<InputFuncId, SymbolId>,
    datas_ids: BTreeMap<DataSymbolKey, SymbolId>,
}

impl<'src> SymbolMap<'src> {
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
            symbol_kind: SymbolKind,
        }

        let (code_relocs, data_relocs) = Self::collect_ordered_relocs(wasm)?;

        let mut func_ids = IdMap::<InputFuncId, SymbolRange>::new();
        // TODO: Convert to Vec(DataSegmentId, offset, SymbolId) ?
        let mut data_ids = BTreeMap::<DataSymbolKey, SymbolRange>::new();

        let mut symbols = IdVec::new();

        let data_section_start = wasm.data.starting_offset;

        for (symbol_id, symbol) in wasm.linking.linking_symbols.symbols.iter().enumerate() {
            let symbol_id = Id::from_index(symbol_id);

            let sym = match *symbol {
                SymbolInfo::Func { index, flags, name } => {
                    let input_function_id = Id::from_index(index);
                    let name = Self::get_or_create_name(
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

                    func_ids.insert(
                        input_function_id,
                        SymbolRange {
                            id: symbol_id,
                            range: fn_range,
                        },
                    );

                    SymIm {
                        flags,
                        name,
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
                            start,
                        },
                        SymbolRange {
                            id: symbol_id,
                            range: data_range,
                        },
                    );

                    SymIm {
                        flags,
                        name: name.into(),
                        symbol_kind: SymbolKind::DataDefined {
                            segment_id,
                            offset: defined.offset as usize,
                            length: defined.size as usize,
                        },
                    }
                }
                SymbolInfo::Global { flags, name, index } => {
                    let global_id = Id::from_index(index);
                    let name = Self::get_or_create_name(
                        name,
                        wasm.names.globals.get(global_id).map(|n| *n),
                        || format!("global_{index}"),
                    );
                    SymIm {
                        flags,
                        name,
                        symbol_kind: SymbolKind::Global(global_id),
                    }
                }
                SymbolInfo::Table { index, name, flags } => {
                    let table_id = Id::from_index(index);
                    let name = Self::get_or_create_name(
                        name,
                        wasm.names.tables.get(table_id).map(|n| *n),
                        || format!("table_{}", table_id),
                    );
                    SymIm {
                        flags,
                        name,
                        symbol_kind: SymbolKind::Table(table_id),
                    }
                }
                _ => continue, // skip unsupported symbols
            };

            let id = symbols.push(SymbolRecord {
                name: sym.name,
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
                        cmp => panic!(
                            "Symbol {symbol:?} partially overlap with relocation entry {reloc:?} cmp_result:{cmp:?}"
                        ),
                    }
                    symbols[symbol.id]
                        .relocs
                        .push(relocations.pop_front().unwrap())
                }
            }
        }

        // 1. Get ordered relocs (data segments, code).
        // 2. Iterate symbols by function_id, and get pop relocs untill it in range of function body.
        // 3. same for data relocs.
        move_relocs(&mut symbols, func_ids.iter(), code_relocs);
        move_relocs(&mut symbols, data_ids.iter(), data_relocs);

        Ok(Self {
            symbols,
            funcs_ids: func_ids.into_iter().map(|(k, v)| (k, v.id)).collect(),
            datas_ids: data_ids.into_iter().map(|(k, v)| (k, v.id)).collect(),
        })
    }

    pub fn get(&self, id: SymbolId) -> Option<&SymbolRecord<'src>> {
        self.symbols.get(id)
    }
    pub fn get_function_symbol(&self, func_id: InputFuncId) -> Option<SymbolId> {
        self.funcs_ids.get(func_id).copied()
    }

    pub fn get_data_symbol(&self, segment_id: DataSegmentId, offset: usize) -> Option<SymbolId> {
        self.datas_ids
            .get(&DataSymbolKey {
                data_segment: segment_id,
                start: offset,
            })
            .copied()
    }
    pub fn is_function(&self, id: SymbolId) -> bool {
        matches!(self.symbols[id].kind, SymbolKind::Func { .. })
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
                    log::trace!("Conflicting names for symbol: linking section name '{linking}', name section name '{name_section}'");
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

    pub fn iter_data_symbols(&self) -> impl Iterator<Item = (SymbolId, &SymbolRecord<'src>)> {
        self.symbols.iter().filter_map(|(id, sym)| match sym.kind {
            SymbolKind::DataDefined { .. } => Some((id, sym)),
            _ => None,
        })
    }
    pub fn iter(&self) -> impl Iterator<Item = (SymbolId, &SymbolRecord<'src>)> {
        self.symbols.iter()
    }
}
