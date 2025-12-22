//!
//! Data chunks, can be either mapped from data segments.
//! Or in case of linkage info provded can be part of a segment.
//!

use std::{borrow::Cow, collections::BTreeSet, ops::Range};

use cranelift_entity::{EntityRef, packed_option::ReservedValue};
use wasmparser::{DataKind, SymbolFlags};

use crate::{
    InputObject, ObjectReader, Result,
    helpers::{RangeComp, RangeExt},
    index::{IdVec, NonDefault},
    read::DataSegmentId,
    symbols::{SymbolId, SymbolKind},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    ActiveOffset(u32),
    Passive,
}
impl Location {
    pub fn from_data_kind(kind: &DataKind) -> Result<Self> {
        match kind {
            // TODO: add support of multiple memories, and calculated (GOT based) offsets
            DataKind::Active {
                offset_expr,
                memory_index: _,
            } => {
                let offset = InputObject::read_const_expr(&offset_expr)?;
                offset
                    .try_into()
                    .map(Location::ActiveOffset)
                    .map_err(|_| anyhow::anyhow!("Negative offset in active data segment"))
            }
            DataKind::Passive => Ok(Location::Passive),
        }
    }
}

type Str<'a> = NonDefault<Cow<'a, str>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataChunk<'a, D: 'a> {
    pub segment_id: DataSegmentId,
    pub location: Location,
    // optional name from symbol info
    // Handles "" as reserved value
    pub name: Str<'a>,
    pub flags: SymbolFlags,
    pub data: D,
    // offset related to this symbol
    pub relocations: Vec<wasmparser::RelocationEntry>,
    // associated symbol index
    // Can be `reserved_value` if created from data segment without symbol info
    pub symbol_index: SymbolId,
    pub pow2align: u8,
}

/// Describes how a data symbol relates to its neighboring symbols within a segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SymbolRelation<'a> {
    /// A standalone symbol with no binding constraints.
    Regular { bytes: &'a [u8] },

    /// A symbol that must stay adjacent to the previous symbol
    /// and cannot be moved or removed independently.
    BoundToPrevious {
        /// offset from start of previous regular symbol
        offset: usize,
    },
}

impl<'a> DataChunk<'a, &'a [u8]> {
    pub fn from_segment(reader: ObjectReader<'a>, segment_id: DataSegmentId) -> Result<Self> {
        let segment = reader
            .data
            .data_segments
            .get(segment_id)
            .ok_or_else(|| anyhow::anyhow!("Data segment ID {segment_id} not found"))?;
        let info = reader
            .linking
            .segments_info
            .get(segment_id.index())
            .ok_or_else(|| {
                anyhow::anyhow!("Linking segment info for data segment ID {segment_id} not found")
            })?;
        Ok(Self {
            segment_id,
            data: segment.data.into(),
            flags: SymbolFlags::empty(),
            pow2align: info.alignment.try_into().map_err(|_| {
                anyhow::anyhow!("Data segment alignment {} is too large", info.alignment)
            })?,
            relocations: Vec::new(),
            location: Location::from_data_kind(&segment.kind)?,
            name: NonDefault::reserved_value(),
            symbol_index: SymbolId::reserved_value(),
        })
    }
    /// Extracts data chunks defined in linking table as separate chunks.
    ///
    /// Expects that self is a chunk extracted directly from data segment usign `from_segment`.
    ///
    /// Returns list of data chunks sliced from segment.
    pub fn slice_segment(
        self,
        symbol_table: &crate::symbols::Symbols<'a>,
    ) -> IdVec<DataChunk<'a, SymbolRelation<'a>>> {
        let segment_align = 1 << self.pow2align;

        let mem_offset = match &self.location {
            Location::Passive => panic!("Passive data is not currently supported"),
            Location::ActiveOffset(offset) => *offset,
        };

        log::debug!(
            "Slicing data segment with offset:{}, alignment: {}",
            mem_offset,
            segment_align
        );

        let mut data_parts = IdVec::new();
        let mut last_regular = 0..0;
        for (sym_id, sym) in symbol_table.iter_data_symbols_for_segment(self.segment_id) {
            let SymbolKind::DataDefined { offset, length, .. } = &sym.kind else {
                unreachable!("Expected DataDefined symbol kind for data segment symbol");
            };

            let symbol_in_data = offset.clone()..(offset + length);
            let field_alignment =
                Self::data_symbol_alignment(self.pow2align.into(), symbol_in_data.clone());

            let relation = if last_regular.end > symbol_in_data.start {
                log::warn!(
                    "DataSymbol intersects with previous, this is currently in testing: {:?} range:{:?} prev_range: {:?}",
                    sym,
                    symbol_in_data,
                    last_regular
                );
                // Don't allow intersecting ranges
                assert!(matches!(
                    last_regular.cmp_range(&symbol_in_data),
                    RangeComp::Overlap | RangeComp::Equal
                ));
                let offset = last_regular.start - symbol_in_data.start;
                SymbolRelation::BoundToPrevious { offset }
            } else {
                if last_regular.end < symbol_in_data.start {
                    let gap_range = last_regular.end..symbol_in_data.start;

                    if gap_range.len() >= segment_align {
                        log::error!(
                            "Data segment has gap larger than segment alignment: {:?} > {} before {}",
                            gap_range,
                            segment_align,
                            sym.debug_name
                        );
                    } else {
                        // debug alignment
                        log::trace!(
                            "Data segment has gap: {:?} ({} bytes) before {}",
                            gap_range,
                            gap_range.len(),
                            sym.debug_name
                        );
                    }
                }
                log::trace!(
                    "Data symbol {}: offset: {}, size: {}, alignment: {}",
                    sym.debug_name,
                    symbol_in_data.start,
                    symbol_in_data.len(),
                    field_alignment
                );
                last_regular = symbol_in_data.clone();
                SymbolRelation::Regular {
                    bytes: self.data.get(symbol_in_data.clone()).unwrap(),
                }
            };

            let part = DataChunk {
                segment_id: self.segment_id,
                pow2align: field_alignment,
                location: self.location.add_offset(symbol_in_data.start as u32),
                name: sym.debug_name.clone().into(),
                flags: sym.flags,
                relocations: sym.relocs.clone(),
                symbol_index: sym_id,
                data: relation,
            };
            log::trace!("Data part: {part:?}");
            data_parts.push(part);
        }

        data_parts
    }

    fn data_symbol_alignment(segment_alignment: u8, chunk_range: Range<usize>) -> u8 {
        let start_align = chunk_range
            .start
            .trailing_zeros()
            .try_into()
            .unwrap_or(u8::MAX);

        // Can't exceed the segment's alignment
        segment_alignment.min(start_align)
    }
}

impl<'a> DataChunk<'a, SymbolRelation<'a>> {
    /// Removes bound symbols from the list, calling cleanup handler for each removed symbol.
    pub fn filter_bound_symbols(
        this: IdVec<Self>,
        // external cleanup handler
        // return map of RemovedSymbol to -> (BoundSymbol, offset_within_symbol)
        mut cleanup_symbol: impl FnMut(SymbolId, (SymbolId, u32)),
    ) -> IdVec<DataChunk<'a, &'a [u8]>> {
        let mut result = IdVec::new();
        let mut last_regular_data = (&[] as &[u8], SymbolId::reserved_value());
        for (_, chunk) in this.into_inner().into_iter() {
            match chunk.data {
                SymbolRelation::Regular { bytes } => {
                    last_regular_data = (bytes, chunk.symbol_index);
                    result.push(DataChunk {
                        segment_id: chunk.segment_id,
                        location: chunk.location.clone(),
                        name: chunk.name.clone(),
                        flags: chunk.flags,
                        data: bytes,
                        relocations: chunk.relocations.clone(),
                        symbol_index: chunk.symbol_index,
                        pow2align: chunk.pow2align,
                    });
                }
                SymbolRelation::BoundToPrevious { offset } => {
                    let prev_symbol = last_regular_data.1;
                    cleanup_symbol(prev_symbol, (chunk.symbol_index, offset as u32));
                    // added as part of previous symbol, so skip
                    continue;
                }
            }
        }
        result
    }

    // Looks similar to dedup in symbols table
    // TODO: unify logic?
    pub fn filter_bounds_in_table(
        this: IdVec<Self>,
        table: &mut crate::symbols::Symbols<'a>,
    ) -> IdVec<DataChunk<'a, &'a [u8]>> {
        let mut to_be_removed = BTreeSet::new();
        Self::filter_bound_symbols(this, |removed, (real_id, offset)| {
            // replace usage of this symbol
            table.replace_usage(removed, real_id, offset.into());
            // take relocations of this symbol
            let sym_record = table.get_mut(removed).expect("Symbol must exist");
            let mut relocs = std::mem::take(&mut sym_record.relocs);
            // modify their offsets
            for reloc in &mut relocs {
                reloc.offset += offset;
            }

            // Add relocs to real symbol
            let real_sym = table.get_mut(real_id).expect("Symbol must exist");
            real_sym.relocs.extend(relocs);
            // order relocs by offset
            real_sym.relocs.sort_by_key(|r| r.offset);

            to_be_removed.insert(removed);
        })
    }
}

impl_entity_index! {
    pub struct DataSymbolRef;
}

// impl generic over `D`
impl<'lf, D> crate::index::PrimaryKey for DataChunk<'lf, D> {
    type EntityRef = DataSymbolRef;
}

impl Location {
    pub fn add_offset(&self, offset: u32) -> Self {
        match self {
            Location::ActiveOffset(base) => Location::ActiveOffset(base + offset),
            Location::Passive => Location::Passive,
        }
    }
}
