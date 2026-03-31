use std::{borrow::Cow, collections::BTreeMap, fmt::Debug, io::Write, ops::Range};

use anyhow::Result;
use cranelift_bitset::CompoundBitSet;
use cranelift_entity::{EntityRef, PrimaryMap, packed_option::ReservedValue};

use super::{DataSymbolRef, ItemType, Offsets, SpecificLocation};
use crate::{
    emit::modify::wasm_emitter::{self, EncodeWithRelocOffset, SectionList},
    helpers::cmp_range,
    index::{GappedMap, Temp, WithStart},
    layouts::{
        DefinedDataChunk, LayoutItemInfo, MemLayoutBuilder, PartId, SegmentFlags, VirtualSpaceId,
        builder::{SegmentSpec, VirtualSpaceKind},
        guess_data_alignment,
    },
    linkage::{
        LinkageInfo,
        file_db::{FileRelocs, FileSymbolDb},
    },
    raw::SegmentId,
    typed::{
        DefinedEntity, EntityBody, EntityBodyCopy, ExportNames, ImportedEntity, IterBytes,
        MemoryRef, Module,
    },
};

/// Either element in table or data chunk in segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedItem<DefinedEntity, ItemId> {
    /// Body of item.
    pub defined_entity: DefinedEntity,
    ///
    /// Id that was used in builder to refer to this item.
    /// (debug purpose)
    ///
    /// None if technical item (padding)
    pub item_id: Option<ItemId>,
}

#[derive(Copy, Debug, Clone, Eq, PartialEq)]
pub struct ItemPlace {
    pub offsets: Offsets,
    pub segment_id: SegmentId,
    pub part_id: PartId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedSegment<'src, OwnerId, ItemId: EntityRef, DefinedEntity> {
    /// Body of segment, containing defined entities.
    pub parts: PrimaryMap<PartId, SealedItem<DefinedEntity, ItemId>>,

    /// Name of segment.
    pub name: Cow<'src, str>,

    /// Alignment of segment, represented as power of 2.
    pub pow2align: u8,

    /// Address in virtual memory or table where segment should be placed.
    pub va_address: VirtualSpaceKind<OwnerId>,

    /// Offset of segment in the file.
    pub file_offset: usize,
}

impl<'src, OwnerId, ItemId: EntityRef, DefinedEntity>
    SealedSegment<'src, OwnerId, ItemId, DefinedEntity>
where
    DefinedEntity: LayoutItemInfo,
{
    pub fn data_stream(&self) -> DataStream<'_, DefinedEntity, ItemId> {
        let total_size: usize = self
            .parts
            .values()
            .map(|chunk| chunk.defined_entity.size())
            .sum();

        let iter: FlatIter<'_, DefinedEntity, ItemId> = self
            .parts
            .values()
            .flat_map(|chunk| chunk.defined_entity.iter_chunks());
        DataStream { iter, total_size }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedLayout<'src, OwnerId, ItemId: EntityRef, DefinedEntity> {
    pub segments: PrimaryMap<SegmentId, SealedSegment<'src, OwnerId, ItemId, DefinedEntity>>,

    /// Imported items that left after sealing.
    pub(crate) external: WithStart<ItemId, ImportedEntity<'src, ()>>,
    /// Can have gaps when recover from object file (e.g. overlapping items).
    pub(crate) defined: GappedMap<ItemId, ItemPlace>,
}

impl<'src, OwnerId, ItemId: EntityRef, DefinedEntity>
    SealedLayout<'src, OwnerId, ItemId, DefinedEntity>
{
    pub fn item_places(&self) -> &GappedMap<ItemId, ItemPlace> {
        &self.defined
    }
    pub fn external(&self) -> &WithStart<ItemId, ImportedEntity<'src, ()>> {
        &self.external
    }
    pub fn segments(
        &self,
    ) -> &PrimaryMap<SegmentId, SealedSegment<'src, OwnerId, ItemId, DefinedEntity>> {
        &self.segments
    }
}
impl<'src, OwnerId, ItemId: EntityRef, DefinedEntity>
    SealedLayout<'src, OwnerId, ItemId, DefinedEntity>
where
    DefinedEntity: LayoutItemInfo<OwnerId = OwnerId> + Debug,
    OwnerId: EntityRef + Debug,
    ItemId: Debug,
{
    pub fn encode<W>(&self, writer: &mut SectionList<W>) -> Result<(), std::io::Error>
    where
        W: std::io::Write,
    {
        for segment in self.segments.values() {
            writer.item_from_encoder(|e| {
                data_segment_adapter(e, segment.va_address, segment.data_stream())
            })?;
        }

        Ok(())
    }
}

//
// Virtual space recover helpers
//
#[derive(Debug, Clone, PartialEq, Eq, Ord, PartialOrd)]
pub(super) struct VsKey {
    vs_location: VirtualSpaceKind<MemoryRef>,
    // Bump number to distinguish different virtual spaces with same base and got (e.g. multiple passive segments)
    // If segments cannot be merged (intersects) - try to keep their original offsets.
    bump_num: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VsState {
    range: Range<usize>,
    segments: Vec<SegmentId>,
    vs_id: VirtualSpaceId,
}

impl VsState {
    fn try_merge(&self, other: &Range<usize>, segment: SegmentId) -> Option<VsState> {
        let range = if self.range.end == other.start {
            self.range.start..other.end
        } else if self.range.start == other.end {
            other.start..self.range.end
        }
        // if intersects - panic
        else if cmp_range(&self.range, other).is_intersecting() {
            panic!(
                "Data segments with same base {:?} have intersecting ranges: {:?} and {:?}",
                self.vs_id, self.range, other
            );
        } else {
            return None;
        };
        let mut segments = self.segments.clone();
        segments.push(segment);
        Some(VsState {
            segments,
            range,
            vs_id: self.vs_id,
        })
    }
}

pub(super) struct VsRecover {
    // Map from virtual space key to its state.
    virtual_spaces: BTreeMap<VsKey, VsState>,
    next_vs_id: VirtualSpaceId,
}
impl VsRecover {
    pub fn new() -> Self {
        Self {
            virtual_spaces: BTreeMap::new(),
            next_vs_id: VirtualSpaceId::new(0),
        }
    }

    pub fn iter_vs(
        &self,
    ) -> impl Iterator<Item = (VirtualSpaceId, Vec<SegmentId>, VirtualSpaceKind<MemoryRef>)> + '_
    {
        self.virtual_spaces
            .iter()
            .map(|(key, state)| (state.vs_id, state.segments.clone(), key.vs_location))
    }

    pub fn add_segment(
        &mut self,
        segment_id: SegmentId,
        segment: &SealedSegment<'_, MemoryRef, DataSymbolRef, DefinedDataChunk<'_>>,
    ) {
        let (mut key, offset) = Self::get_vs_key_base(segment);
        let range = offset..offset + segment.data_stream().bytes_len();
        let (key, state) = 'push: {
            for (existing, state) in self.iter_range(key.clone()) {
                if !existing.vs_location.is_active() {
                    break 'push (existing.clone(), state.clone());
                }
                // bump tmp key
                key.bump_num += 1;
                if let Some(merged) = state.try_merge(&range, segment_id) {
                    let existing = existing.clone();
                    break 'push (existing, merged);
                }
            }

            // not found - bump next_id and insert
            let temp_state = VsState {
                range,
                segments: vec![segment_id],
                vs_id: self.next_vs_id,
            };
            self.next_vs_id = self.next_vs_id.next();
            (key, temp_state)
        };

        self.virtual_spaces.insert(key, state);
    }

    fn iter_range(&self, base: VsKey) -> impl Iterator<Item = (&VsKey, &VsState)> + '_ {
        self.virtual_spaces
            .range(base.clone()..)
            .take_while(move |(k, _)| k.vs_location == base.vs_location)
    }

    // Get VsKey with bump = 0.
    fn get_vs_key_base(
        segment: &SealedSegment<'_, MemoryRef, DataSymbolRef, DefinedDataChunk<'_>>,
    ) -> (VsKey, usize) {
        match segment.va_address {
            VirtualSpaceKind::Active {
                mut owner_id,
                location,
            } => {
                let offset = location.offset() as usize;
                let location = location.with_zero_offset();
                (
                    VsKey {
                        vs_location: VirtualSpaceKind::Active { owner_id, location },
                        bump_num: 0,
                    },
                    offset,
                )
            }
            other => (
                VsKey {
                    vs_location: other,
                    bump_num: 0,
                },
                0,
            ),
        }
    }
}

//
// Encode helpers
//

fn data_segment_adapter<W, DefinedEntity, ItemId>(
    encoder: &mut wasm_emitter::Encoder<W>,
    location: VirtualSpaceKind<DefinedEntity::OwnerId>,
    data_stream: DataStream<DefinedEntity, ItemId>,
) -> Result<(), std::io::Error>
where
    W: Write,
    DefinedEntity: LayoutItemInfo,
{
    // where segment:
    // - header (mode/offset)
    // - len of data
    // - data bytes

    let header = <DefinedEntity as LayoutItemInfo>::segment_header_start(location)?;
    encoder.push_bytes(&header)?;
    // len + data
    data_stream.encode(encoder)?;
    Ok(())
}

type FlatIter<'a, DefinedEntity, ItemId> = std::iter::FlatMap<
    std::slice::Iter<'a, SealedItem<DefinedEntity, ItemId>>,
    IterBytes<'a>,
    for<'b> fn(&'b SealedItem<DefinedEntity, ItemId>) -> IterBytes<'b>,
>;

#[derive(Debug)]
pub struct DataStream<'a, DefinedEntity, ItemId> {
    pub(crate) iter: FlatIter<'a, DefinedEntity, ItemId>,
    pub(crate) total_size: usize,
}

impl<'a, DefinedEntity, ItemId> Clone for DataStream<'a, DefinedEntity, ItemId> {
    fn clone(&self) -> Self {
        Self {
            iter: self.iter.clone(),
            total_size: self.total_size,
        }
    }
}

impl<'a, DefinedEntity, ItemId> DataStream<'a, DefinedEntity, ItemId> {
    fn bytes_len(&self) -> usize {
        self.total_size
    }
}

impl<'a, DefinedEntity, ItemId> Iterator for DataStream<'a, DefinedEntity, ItemId> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next()
    }
}

impl<'a, DefinedEntity, ItemId> wasm_emitter::EncodeWithRelocOffset
    for DataStream<'a, DefinedEntity, ItemId>
{
    type Offsets = u32; // offset to bytes stream start
    fn encode<W>(
        &self,
        encoder: &mut wasm_emitter::Encoder<W>,
    ) -> std::result::Result<Self::Offsets, std::io::Error>
    where
        W: std::io::Write,
    {
        encoder.encode_leb_5byte(self.bytes_len() as u32)?;

        let offset = encoder.offset();
        for bytes in self.clone() {
            encoder.push_bytes(bytes)?
        }

        Ok(offset)
    }
}
