//!
//! Data chunks, can be either mapped from data segments.
//! Or in case of linkage info provded can be part of a segment.
//!

use std::{borrow::Cow, collections::BTreeSet, ops::Range};

use cranelift_entity::{EntityRef, packed_option::ReservedValue};
use wasmparser::{DataKind, SymbolFlags};

use crate::{
    ObjectReader, Result,
    helpers::{RangeComp, RangeExt, cmp_range},
    index::{GappedMap, IdVec, NonDefault},
    linkage::{
        file_db::{FileRelocs, FileSymbolDb},
        reloc::AnyRelocationEntry,
    },
    raw::DataSegmentId,
    typed::{Module, SymbolId},
};

#[derive(Debug)]
pub struct DataDefined {
    pub segment_id: DataSegmentId,
    // Range of bytes in data segment related to this symbol
    pub range: Range<u32>,
}
impl From<&wasmparser::DefinedDataSymbol> for DataDefined {
    fn from(value: &wasmparser::DefinedDataSymbol) -> Self {
        Self {
            segment_id: DataSegmentId::from_u32(value.index),
            range: value.offset..(value.offset + value.size),
        }
    }
}

/// Memory location of data chunk
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DataLocation {
    /// Place chunk at offset (starting from mem_start) in active memory.
    ActiveOffset(u32),
    /// Don't place chunk in memory automatically.
    Passive,
}
impl DataLocation {
    pub fn from_data_kind(kind: &DataKind) -> Result<Self> {
        match kind {
            // TODO: add support of multiple memories, and calculated (GOT based) offsets
            DataKind::Active {
                offset_expr,
                memory_index: _,
            } => {
                let offset = Module::read_const_expr(&offset_expr)?;
                offset
                    .try_into()
                    .map(DataLocation::ActiveOffset)
                    .map_err(|_| anyhow::anyhow!("Negative offset in active data segment"))
            }
            DataKind::Passive => Ok(DataLocation::Passive),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataChunk<D> {
    /// offset of this chunk in wasm file
    pub original_offset: usize,
    pub data: D,
    pub pow2align: u8,
}

pub type RawDataChunk<'a> = DataChunk<&'a [u8]>;

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
    pub fn from_segment(
        segment_id: DataSegmentId,
        segment_data: &'a [u8],
        pow2align: u8,
        original_offset: usize,
    ) -> Self {
        Self {
            data: segment_data,
            pow2align,
            original_offset,
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
        defined_data_symbols: impl IntoIterator<Item = (SymbolId, &'o DataDefined)>,
    ) -> IdVec<DataChunk<SymbolRelation<'a>>> {
        let pow2align = self.pow2align;
        let segment_align = 1 << pow2align;

        #[cfg(debug_assertions)]
        let mut segment_id = None;

        let segment_offset = self.original_offset;
        let mut data_parts = IdVec::new();
        let mut last_regular = 0..0;
        for (symbol_id, d) in defined_data_symbols.into_iter() {
            #[cfg(debug_assertions)]
            {
                if let Some(id) = segment_id {
                    assert_eq!(
                        id, d.segment_id,
                        "All data symbols should belong to the same segment"
                    );
                } else {
                    segment_id = Some(d.segment_id);
                }
            }

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

impl<'a> DataChunk<SymbolRelation<'a>> {
    /// Removes bound symbols from the list, calling cleanup handler for each removed symbol.
    fn filter_bounds_with_cleanup(
        this: IdVec<Self>,
        // external cleanup handler
        // return map of RemovedSymbol to -> (BoundSymbol, offset_within_symbol)
        mut cleanup_symbol: impl FnMut(SymbolId, (SymbolId, u32)),
    ) -> IdVec<DataChunk<&'a [u8]>> {
        let mut result = IdVec::new();
        let mut last_regular_data = (&[] as &[u8], SymbolId::reserved_value());
        for (_, chunk) in this.into_inner().into_iter() {
            match chunk.data {
                SymbolRelation::Regular { bytes, symbol_id } => {
                    last_regular_data = (bytes, symbol_id);
                    result.push(DataChunk {
                        data: bytes,
                        pow2align: chunk.pow2align,
                        original_offset: chunk.original_offset,
                    });
                }
                SymbolRelation::BoundToPrevious { offset, symbol_id } => {
                    let prev_symbol = last_regular_data.1;
                    cleanup_symbol(prev_symbol, (symbol_id, offset as u32));
                    // added as part of previous symbol, so skip
                    continue;
                }
            }
        }
        result
    }

    /// Removes bound symbols from the list, updating symbol table accordingly.
    pub fn filter_bounds_in_table(
        this: IdVec<Self>,
        table: &mut FileSymbolDb,
    ) -> IdVec<DataChunk<&'a [u8]>> {
        Self::filter_bounds_with_cleanup(this, |real_id, (removed, offset)| {
            // Replace entity_ref in table.
            let mut new_entry = table.symbols[real_id].clone();

            assert!(new_entry.offset_in_entity == 0, "should be regular symbol");
            new_entry.offset_in_entity = offset;

            table.symbols[removed] = new_entry;
        })
    }
}

impl_entity_index! {
    #[display="data"]
    pub struct DataSymbolRef;
}

// impl generic over `D`
impl<D> crate::index::PrimaryKey for DataChunk<D> {
    type EntityRef = DataSymbolRef;
}

impl DataLocation {
    pub fn add_offset(&self, offset: u32) -> Self {
        match self {
            DataLocation::ActiveOffset(base) => DataLocation::ActiveOffset(base + offset),
            DataLocation::Passive => DataLocation::Passive,
        }
    }
}
