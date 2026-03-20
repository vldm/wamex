//!
//! Data chunks, can be either mapped from data segments.
//! Or in case of linkage info provded can be part of a segment.
//!

use std::{borrow::Cow, ops::Range};

use cranelift_entity::{EntityRef, PrimaryMap, packed_option::ReservedValue};
use wasmparser::DataKind;

use crate::{
    ObjectReader, Result,
    helpers::{RangeComp, cmp_range},
    linkage::file_db::{FileSymbolDb, SymbolOffset},
    raw::DataSegmentId,
    typed::{GlobalRef, MemoryRef, Module, SymbolId},
};

// Align base of memory to 16 bytes, if it wasn't already aligned.
pub const BASE_ALIGNMENT: u8 = u8::trailing_zeros(16) as u8;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MemSpec<'src> {
    pub mem_id: MemoryRef,
    /// before - is a space for stack
    pub mem_start: SpecificLocation,
    pub data_segments: PrimaryMap<DataSegmentId, DataSegmentInfo<'src>>,
}

impl Default for MemSpec<'_> {
    fn default() -> Self {
        Self {
            mem_id: MemoryRef::reserved_value(),
            mem_start: Self::DEFAULT_HEAP_SIZE,
            data_segments: PrimaryMap::new(),
        }
    }
}
pub fn default_segment_info<'src>() -> (Cow<'src, str>, u8) {
    ("data".into(), BASE_ALIGNMENT)
}

impl<'src> MemSpec<'src> {
    const DEFAULT_HEAP_SIZE: SpecificLocation = SpecificLocation::ConstantOffset(0x100000);

    pub fn from_reader(reader: &ObjectReader<'src>, memory_id: MemoryRef) -> Result<Self> {
        let mut mem_start = None;
        let mut data_segments: PrimaryMap<DataSegmentId, DataSegmentInfo<'src>> = PrimaryMap::new();

        for (id, segment) in reader.data.data_segments.iter() {
            let (name, pow2align) = if let Some(info) = reader.linking.segments_info.get(id.index())
            {
                (info.name.into(), info.alignment as u8)
            } else {
                default_segment_info()
            };

            let mut segment_info = DataSegmentInfo::from_parts(&segment.kind, name, pow2align)?;
            match segment_info.location {
                SegmentPlacement::Passive | SegmentPlacement::ContinuesMemory => {}
                SegmentPlacement::Specific(v) => {
                    //if mem_start exist find lower offset between it and v
                    let new = if let Some(mem_start) = mem_start {
                        match (mem_start, v) {
                            (
                                SpecificLocation::ConstantOffset(m),
                                SpecificLocation::ConstantOffset(v),
                            ) => SpecificLocation::ConstantOffset(m.min(v)),
                            (
                                SpecificLocation::GotBased {
                                    global: mg,
                                    offset: mo,
                                },
                                SpecificLocation::GotBased {
                                    global: vg,
                                    offset: vo,
                                },
                            ) if mg == vg => SpecificLocation::GotBased {
                                global: mg,
                                offset: mo.min(vo),
                            },
                            _ => {
                                panic!(
                                    "Incompatible segment placements: {:?} and {:?}, using the first one",
                                    mem_start, v
                                );
                            }
                        }
                    } else {
                        v
                    };
                    if segment_info.location != SegmentPlacement::ContinuesMemory {
                        log::trace!(
                            "Replace data location to continues memory: {:?} -> {:?}",
                            segment_info.location,
                            new
                        );
                        segment_info.location = SegmentPlacement::ContinuesMemory;
                    }

                    mem_start = Some(new);
                }
            }
            assert_eq!(data_segments.push(segment_info), id);
        }

        Ok(Self {
            mem_id: memory_id,
            mem_start: mem_start.unwrap_or(Self::DEFAULT_HEAP_SIZE),
            data_segments,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DataSegmentInfo<'a> {
    pub name: Cow<'a, str>,
    pub location: SegmentPlacement,
    pub pow2align: u8,
}
impl<'a> DataSegmentInfo<'a> {
    pub fn from_parts(kind: &DataKind, name: Cow<'a, str>, pow2align: u8) -> Result<Self> {
        Ok(Self {
            name,
            location: SegmentPlacement::from_data_kind(kind)?,
            pow2align,
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DataDefined<'a> {
    pub segment_id: DataSegmentId,
    pub name: Cow<'a, str>,
    // Range of bytes in data segment related to this symbol
    pub range: Range<u32>,
}
impl<'a> DataDefined<'a> {
    pub fn from_defined(value: &wasmparser::DefinedDataSymbol, name: Cow<'a, str>) -> Self {
        Self {
            segment_id: DataSegmentId::from_u32(value.index),
            range: value.offset..(value.offset + value.size),
            name,
        }
    }
}

/// Location of segment
/// - Can be passive: mean that init function should explicitly place it
///   in memory using `memory.init` (relocation cannot be applied to
///   passive segments, since their offset is determined at runtime).
///   Passive segments are represented as None in outer Option<SegmentPlacement>.
/// - Can be constant offset: mean that data segment will be placed
///   at some specific offset in memory by wasm loader.
/// - Can be GOT based: mean that data segment will be placed at offset dependent on value of some global (e.g. module base) by wasm loader.
/// - Or if it in build phase it can be not determined yet.
///
/// Same logic applies to element segments, but instead of offsets in memory they represent offset in table.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SegmentPlacement {
    /// Place chunk at offset (starting from mem_start) in active memory, where offset is calculated as value of global + offset.
    Passive,
    /// Some specific location in memory, cannot be moved.
    Specific(SpecificLocation),
    /// Location is not determined yet, but should be placed somewhere in active memory.
    ContinuesMemory,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SpecificLocation {
    /// Place chunk at offset (starting from mem_start) in active memory, where offset is calculated as value of global + offset.
    GotBased { global: GlobalRef, offset: u32 },
    /// Place chunk at offset (starting from mem_start) in active memory.
    ConstantOffset(u32),
}
impl SpecificLocation {
    pub fn offset(&self) -> u32 {
        match self {
            SpecificLocation::GotBased { offset, .. } => *offset,
            SpecificLocation::ConstantOffset(offset) => *offset,
        }
    }
}

impl SegmentPlacement {
    pub fn from_data_kind(kind: &DataKind) -> Result<Self> {
        Ok(match kind {
            // TODO: add support of multiple memories, and calculated (GOT based) offsets
            DataKind::Active {
                offset_expr,
                memory_index: _,
            } => {
                let offset = Module::read_const_expr(offset_expr)?;
                let location = offset
                    .try_into()
                    .map(SpecificLocation::ConstantOffset)
                    .map_err(|_| anyhow::anyhow!("Negative offset in active data segment"))?;

                SegmentPlacement::Specific(location)
            }
            DataKind::Passive => SegmentPlacement::Passive,
        })
    }
}

/// Data chunk information.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DataChunkType {
    pub segment_id: DataSegmentId,
    pub pow2align: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataChunk<'src, D> {
    /// offset of this chunk in wasm file
    pub original_offset: usize,
    pub data: D,
    pub pow2align: u8,
    pub segment_id: DataSegmentId,
    pub name: Cow<'src, str>,
}

pub type RawDataChunk<'src> = DataChunk<'src, &'src [u8]>;

/// Describes how a data symbol relates to its neighboring symbols within a segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SymbolRelation<'a> {
    /// A standalone symbol with no binding constraints.
    Regular {
        bytes: &'a [u8],
        symbol_id: SymbolId,
    },

    /// A symbol that must stay adjacent to the previous symbol
    /// and cannot be moved or removed independently.
    BoundToPrevious {
        /// offset from start of previous regular symbol
        offset: u32,
        symbol_id: SymbolId,
    },
}

impl<'a> RawDataChunk<'a> {
    /// Hack: Imported data symbol for future resolution.
    pub fn new_imported() -> Self {
        todo!()
    }

    pub fn from_segment(
        segment_id: DataSegmentId,
        segment_data: &'a [u8],
        segment_name: Cow<'a, str>,
        pow2align: u8,
        original_offset: usize,
    ) -> Self {
        Self {
            data: segment_data,
            pow2align,
            original_offset,
            segment_id,
            name: segment_name,
        }
    }
    /// Extracts data chunks defined in linking table as separate symbol.
    ///
    /// Expects that self is a chunk extracted directly from data segment usign `from_segment`.
    ///
    /// Returns list of data chunks sliced from segment.
    /// Some returned chunks can be marked as `BoundToPrevious` if they offset overlaps with previous symbol.
    /// To filter these symbols use `filter_bound_symbols` method.
    pub fn slice_segment<'o>(
        self,
        // Iterator over defined data symbols in this segment
        // symbol_id is used for debugging and later cleanup of bound symbols
        defined_data_symbols: impl IntoIterator<Item = (SymbolId, &'o DataDefined<'a>)>,
    ) -> PrimaryMap<DataSymbolRef, DataChunk<'a, SymbolRelation<'a>>>
    where
        'a: 'o,
    {
        let pow2align = self.pow2align;
        let segment_align = 1 << pow2align;

        let segment_offset = self.original_offset;
        let mut data_parts = PrimaryMap::new();
        let mut last_regular = 0..0;
        for (symbol_id, d) in defined_data_symbols.into_iter() {
            debug_assert_eq!(
                self.segment_id, d.segment_id,
                "All data symbols should belong to the same segment"
            );

            let symbol_in_data = d.range.clone();
            let field_alignment = Self::data_symbol_alignment(pow2align, symbol_in_data.clone());

            let relation = if last_regular.end > symbol_in_data.start {
                log::warn!(
                    "DataSymbol intersects with previous, this is currently in testing: {:?} range:{:?} prev_range: {:?}",
                    d,
                    symbol_in_data,
                    last_regular
                );
                // Don't allow intersecting ranges
                assert!(matches!(
                    cmp_range(&last_regular, &symbol_in_data),
                    RangeComp::Overlap | RangeComp::Equal
                ));
                let offset = symbol_in_data.start - last_regular.start;
                SymbolRelation::BoundToPrevious { offset, symbol_id }
            } else {
                if last_regular.end < symbol_in_data.start {
                    let gap_range = last_regular.end..symbol_in_data.start;

                    if gap_range.len() >= segment_align {
                        log::error!(
                            "Data segment has gap larger than segment alignment: {:?} > {}",
                            gap_range,
                            segment_align,
                        );
                    } else {
                        // debug alignment
                        log::trace!(
                            "Data segment has gap: {:?} ({} bytes) ",
                            gap_range,
                            gap_range.len(),
                        );
                    }
                }
                log::trace!(
                    "Data symbol: offset: {}, size: {}, alignment: {}",
                    symbol_in_data.start,
                    symbol_in_data.len(),
                    field_alignment
                );
                last_regular = symbol_in_data.clone();
                SymbolRelation::Regular {
                    bytes: self
                        .data
                        .get(symbol_in_data.start as usize..symbol_in_data.end as usize)
                        .unwrap(),
                    symbol_id,
                }
            };

            let part = DataChunk {
                pow2align: field_alignment,
                data: relation,
                original_offset: segment_offset + symbol_in_data.start as usize,
                segment_id: d.segment_id,
                name: d.name.clone(),
            };
            log::trace!("Data part: {part:?}");
            data_parts.push(part);
        }

        data_parts
    }

    fn data_symbol_alignment(pow2align: u8, chunk_range: Range<u32>) -> u8 {
        let start_align = chunk_range
            .start
            .trailing_zeros()
            .try_into()
            .unwrap_or(u8::MAX);

        // Can't exceed the segment's alignment
        pow2align.min(start_align)
    }
}

#[derive(Debug)]
enum FilterEvent {
    RemoveBound {
        bound_to_symbol: SymbolId,
        bound_to_data: DataSymbolRef,
        removed_symbol: SymbolId,
        offset_in_bound: u32,
    },
    ShiftRegular {
        symbol_id: SymbolId,
        new_ref: DataSymbolRef,
    },
}

impl<'a> DataChunk<'a, SymbolRelation<'a>> {
    /// Removes bound symbols from the list,
    /// and normalize indexes based on order
    /// - call `filter_event` for each update.
    fn filter_bounds_with_cleanup(
        mut next_id: DataSymbolRef,
        this: PrimaryMap<DataSymbolRef, Self>,
        mut filter_event: impl FnMut(FilterEvent),
    ) -> Vec<RawDataChunk<'a>> {
        let mut result = Vec::new();
        let mut last_regular_data = (
            &[] as &[u8],
            SymbolId::reserved_value(),
            DataSymbolRef::reserved_value(),
        );
        for (_, chunk) in this.into_iter() {
            match chunk.data {
                SymbolRelation::Regular { bytes, symbol_id } => {
                    let new = next_id;
                    next_id = next_id.next();

                    result.push(DataChunk {
                        data: bytes,
                        pow2align: chunk.pow2align,
                        original_offset: chunk.original_offset,
                        segment_id: chunk.segment_id,
                        name: chunk.name,
                    });
                    filter_event(FilterEvent::ShiftRegular {
                        symbol_id,
                        new_ref: new,
                    });
                    last_regular_data = (bytes, symbol_id, new);
                }
                SymbolRelation::BoundToPrevious { offset, symbol_id } => {
                    let prev_symbol = last_regular_data.1;
                    let prev_symbol_ref = last_regular_data.2;
                    filter_event(FilterEvent::RemoveBound {
                        bound_to_symbol: prev_symbol,
                        bound_to_data: prev_symbol_ref,
                        removed_symbol: symbol_id,
                        offset_in_bound: offset,
                    });
                    // added as part of previous symbol, so skip
                    continue;
                }
            }
        }
        result
    }

    /// Removes bound symbols from the list,
    /// and normalize indexes based on order
    /// - updating symbol table accordingly.
    pub fn canonicalize_data_symbols(
        next_id: DataSymbolRef,
        this: PrimaryMap<DataSymbolRef, Self>,
        table: &mut FileSymbolDb,
    ) -> impl Iterator<Item = RawDataChunk<'a>> {
        Self::filter_bounds_with_cleanup(next_id, this, |event| match event {
            FilterEvent::RemoveBound {
                bound_to_symbol,
                bound_to_data,
                removed_symbol,
                offset_in_bound: offset,
            } => {
                let entity = bound_to_data.into();
                if cfg!(debug_assertions) {
                    let new_entry = table.symbols[bound_to_symbol];
                    assert!(new_entry.offset_in_entity == 0, "should be regular symbol");
                };

                table.symbols[removed_symbol] = SymbolOffset {
                    entity,
                    offset_in_entity: offset,
                    used_definition: true,
                };
            }
            FilterEvent::ShiftRegular { symbol_id, new_ref } => {
                let entry = &mut table.symbols[symbol_id];
                entry.entity = new_ref.into();
            }
        })
        .into_iter()
    }
}

impl_entity_index! {
    #[display="data"]
    pub struct DataSymbolRef;
}

impl SpecificLocation {
    pub fn add_offset(&self, offset: u32) -> Self {
        match self {
            SpecificLocation::GotBased {
                global,
                offset: base,
            } => SpecificLocation::GotBased {
                global: *global,
                offset: base + offset,
            },
            SpecificLocation::ConstantOffset(base) => {
                SpecificLocation::ConstantOffset(base + offset)
            }
        }
    }
    pub fn to_init_expr(&self) -> wasm_encoder::ConstExpr {
        match self {
            SpecificLocation::GotBased { global, offset } => {
                wasm_encoder::ConstExpr::global_get(global.as_u32())
                    .with_i32_const((*offset).try_into().unwrap())
                    .with_i32_add()
            }
            SpecificLocation::ConstantOffset(base) => {
                wasm_encoder::ConstExpr::i32_const((*base).try_into().unwrap())
            }
        }
    }
}
