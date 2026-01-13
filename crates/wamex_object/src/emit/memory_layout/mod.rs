use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    fmt::Debug,
    io::IsTerminal,
    usize,
};

use anyhow::Result;
use cranelift_entity::EntityRef;
use wasmparser::{Data, DataKind, SymbolFlags};

use crate::{
    helpers::{RangeComp, RangeExt},
    index::{GappedMap, NonDefault, ReservedValue},
    read::{
        raw::DataSegmentId,
        typed::data::{DataLocation, RawDataChunk as DataChunk},
    },
    symbols::{self, SymbolId, SymbolKind, reloc::AnyRelocationEntry},
};
impl_entity_index! {

    #[display = "segment"]
    pub struct BuilderSegmentId(for<'a> SegmentLayout<'a>);
}

mod hexdump;

// Default is reserved value
type Str<'a> = NonDefault<Cow<'a, str>>;

#[derive(Clone, Debug)]
pub struct SegmentLayout<'a> {
    data_parts: Vec<DataChunk<'a>>,
    segment_name: Str<'a>,
    location: DataLocation,
    pow2align: usize,
}

impl ReservedValue for SegmentLayout<'_> {
    fn reserved_value() -> Self {
        Self {
            pow2align: usize::MAX,
            segment_name: Str::reserved_value(),
            data_parts: Vec::new(),
            location: DataLocation::Passive,
        }
    }
    fn is_reserved_value(&self) -> bool {
        self.data_parts.is_empty()
            && self.pow2align == usize::MAX
            && matches!(self.location, DataLocation::Passive)
    }
}

impl<'src> SegmentLayout<'src> {
    pub fn new_from_module(
        module: &crate::InputObject<'src>,
        data_segment_id: DataSegmentId,
    ) -> Result<SegmentLayout<'src>> {
        // Original segment info
        let segment = &module.wasm_reader.data.data_segments[data_segment_id];
        let segment_info = &module.wasm_reader.linking.segments_info[data_segment_id.index()];
        let cow_name: Cow<'_, str> = segment_info.name.into();
        let alignment = (2usize).pow(segment_info.alignment);

        let mem_location = DataLocation::from_data_kind(&segment.kind)?;

        log::debug!("Mem location is {:?}", mem_location);

        let data_parts = module
            .data
            .iter()
            .filter(|(_, data)| data.segment_id == data_segment_id)
            .map(|(_, chunk)| chunk.clone())
            .collect::<Vec<_>>();

        Ok(SegmentLayout {
            segment_name: cow_name.into(),
            pow2align: alignment,
            data_parts,
            location: mem_location,
        })
    }

    pub fn debug_layout(
        symbol_table: &symbols::Symbols,
        module_name: String,
        data_segments: &GappedMap<DataSegmentId, SegmentLayout<'_>>,
    ) {
        use std::fmt::Write;
        let mut print_data_format = String::new();
        writeln!(print_data_format, "<Module {module_name}>").unwrap();

        let mut base = 0;
        for (i, segment) in data_segments.iter() {
            for symbol in segment.data_parts.iter() {
                writeln!(
                    print_data_format,
                    "Data symbol [{i}.{index}]: {name}",
                    i = i,
                    index = symbol.symbol_index,
                    name = symbol.name,
                )
                .unwrap();
                let chunk = symbol.data;

                let input_symbol = symbol_table.get(symbol.symbol_index).unwrap();
                let refs = input_symbol
                    .relocs
                    .iter()
                    .map(|reloc| {
                        let name = match reloc {
                            AnyRelocationEntry::Linkage(r) => {
                                let reloc_symbol = symbol_table.get(r.symbol_id).unwrap();
                                &reloc_symbol.debug_name
                            }
                            AnyRelocationEntry::Type(_) => "<type>",
                        };
                        hexdump::Ref {
                            range: reloc.relocation_range(),
                            name: name,
                        }
                    })
                    .collect();
                let part = hexdump::DataPart { bytes: chunk, refs };
                hexdump::render_part(
                    &mut print_data_format,
                    base,
                    &part,
                    std::io::stderr().is_terminal(),
                );
                // TODO: add padding
                base += chunk.len();
            }
        }
        println!("Data segments {print_data_format}");
    }

    pub fn memory_location(&self) -> DataLocation {
        self.location
    }

    // Keeps only symbols with id is in `indexes`.
    pub fn new_with_whitelist(mut self, indexes: &BTreeSet<SymbolId>) -> Self {
        let mut result = vec![];

        {
            let mut parts_iter = self.data_parts.drain(..).peekable();
            for item in &mut parts_iter {
                let remove = !indexes.contains(&item.symbol_index);
                if remove {
                    continue;
                }
                result.push(item);
            }
        }

        self.data_parts = result;
        self
    }

    /// Compute data init offset.
    /// Returns (offset_expr, segment_offset)
    fn segment_header(
        &self,
        mem_start: usize,
        mut segment_offset: usize,
        lib_base_global_id: Option<u32>,
    ) -> (Option<wasm_encoder::ConstExpr>, usize) {
        match self.location {
            DataLocation::Passive => (None, 0),
            DataLocation::ActiveOffset(_) => {
                let offset_expr = match lib_base_global_id {
                    None => {
                        segment_offset +=
                            Self::calculate_padding(mem_start + segment_offset, self.pow2align);
                        wasm_encoder::ConstExpr::i32_const(
                            (mem_start + segment_offset).try_into().unwrap(),
                        )
                    }
                    Some(lib_base_global_id) => {
                        // submodules use lib_base_id
                        {
                            segment_offset +=
                                Self::calculate_padding(segment_offset, self.pow2align);
                            wasm_encoder::ConstExpr::global_get(lib_base_global_id)
                                .with_i32_const(segment_offset.try_into().unwrap())
                                .with_i32_add()
                        }
                    }
                };
                (Some(offset_expr), segment_offset)
            }
        }
    }

    fn calculate_padding(starting_point: usize, alignment: usize) -> usize {
        let misalignment = starting_point % alignment;
        if misalignment == 0 {
            0
        } else {
            alignment - misalignment
        }
    }

    /// Compute data init offset and alligned segment_offset.
    pub fn to_segment_output(
        &self,
        lib_base_global_id: Option<u32>,
        mem_start: usize,
        segment_offset: usize,
        //TODO: move segment_offset padding outside
    ) -> (usize, DataSegmentOutput) {
        const BYTE_FILLER: u8 = 0;

        let mut data = Vec::new();

        log::debug!("Segment offset before is {}", mem_start + segment_offset);
        let (data_init, segment_offset) =
            self.segment_header(mem_start, segment_offset, lib_base_global_id);

        log::debug!("Segment offset is {}", mem_start + segment_offset);
        let mut globals = BTreeMap::new();
        let segment_in_mem_start = mem_start + segment_offset;
        for symbol in self.data_parts.iter() {
            let chunk = symbol.data;
            let field_alignment = 1 << symbol.pow2align;

            let total_offset = data.len() + segment_in_mem_start;

            // add padding to align data
            {
                let padding = Self::calculate_padding(total_offset, field_alignment);
                if padding > 0 {
                    log::debug!(
                        "Add padding before data symbol {}: {padding} bytes",
                        symbol.name
                    );

                    data.resize(data.len() + padding, BYTE_FILLER);
                }
            }
            log::trace!(
                "Data symbol {}: offset: {}, size: {}, aligned: {}",
                symbol.name,
                data.len(),
                chunk.len(),
                field_alignment
            );

            globals.insert(
                symbol.symbol_index,
                DataSymbolRefs {
                    data_mem_offset: data.len(),
                },
            );
            data.extend_from_slice(chunk);
        }

        (
            segment_offset,
            DataSegmentOutput {
                data_init: data_init.expect("Active data segment should have offset"),
                data,
                data_symbols: globals,
                memory_offset: mem_start + segment_offset,
            },
        )
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DataSymbolRefs {
    // Relative to lib_base for submodules
    pub data_mem_offset: usize,
}

// generate data segment and global initializers
/// Representation of calculated data segment for output module.
/// Contain data chunk
#[derive(Debug, Clone)]
pub struct DataSegmentOutput {
    // only for active segments
    data_init: wasm_encoder::ConstExpr,
    memory_offset: usize,

    data: Vec<u8>,
    data_symbols: BTreeMap<SymbolId, DataSymbolRefs>,
}
impl ReservedValue for DataSegmentOutput {
    fn reserved_value() -> Self {
        Self {
            data_init: wasm_encoder::ConstExpr::empty(),
            memory_offset: usize::MAX,
            data: Vec::new(),
            data_symbols: BTreeMap::new(),
        }
    }
    fn is_reserved_value(&self) -> bool {
        self.memory_offset == usize::MAX && self.data.is_empty() && self.data_symbols.is_empty()
    }
}

impl DataSegmentOutput {
    pub fn data_segment<'a>(&'a self, memory_index: u32) -> wasm_encoder::DataSegment<'a, Vec<u8>> {
        wasm_encoder::DataSegment {
            mode: wasm_encoder::DataSegmentMode::Active {
                memory_index,
                offset: &self.data_init,
            },

            data: self.data.clone(),
        }
    }
    pub fn as_raw(&self) -> &[u8] {
        &self.data
    }
    pub fn memory_offset(&self) -> usize {
        self.memory_offset
    }
    pub fn symbols(&self) -> &BTreeMap<SymbolId, DataSymbolRefs> {
        &self.data_symbols
    }
}
