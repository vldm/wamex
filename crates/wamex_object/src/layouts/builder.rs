use std::{
    borrow::Cow,
    fmt::{Debug, Display},
};

use cranelift_entity::{PrimaryMap, packed_option::ReservedValue};
use itertools::Itertools;

use super::{
    ItemType, LayoutItemInfo, Offsets, SealedLayout, SealedSegment, SegmentId, VirtualSpaceId,
};
use crate::{
    index::{GappedMap, Temp, TempIndex},
    layouts::{
        calculate_padding,
        sealed::{ItemOffsets, SealedItem},
    },
    typed::{Builder, EntityCollection, ImportedEntity, data::SpecificLocation},
};

//
// Builder api
//

/// Type of segment.
#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    // No modifications of data expected after initialization (.rodata / static)
    Readonly,
    // Arbitrary data that can be modified at runtime (.data / static mut)
    Writable,
    // Zero initialized (.bss)
    ZeroInit,
    // TLS segment, which should be copied to TLS memory at startup
    Tls,
}
///
/// Information about segment, either data or element.
///
pub struct SegmentSpec<'src> {
    /// Reference to virtual space in which this segment is located.
    pub vs_id: VirtualSpaceId,
    /// Name of segment,
    pub name: Cow<'src, str>,
    /// Alignment of segment, represented as power of 2.
    /// Only valid for data segments.
    pub align: u8,
    /// Segment characteristics used for linker.
    pub kind: SegmentKind,
}

pub struct VirtualSpaceSpec<OwnerId> {
    /// Reference to owner entity (Either memory or table).
    pub owner_id: Temp<OwnerId>,
    ///
    /// Information about segment placement in owner unit.
    /// The segment placement is an virtual address in owner unit.
    ///
    /// Can be:
    /// - GotBased - means that segments will have offsets relative to value of global reference.
    /// - Constant - means that segments will have constant offsets.
    /// - None - means that all segments within this virtual space are passive.
    pub location: Option<SpecificLocation>,
}

/// A builder for layout of data or element segments, that can be used to construct `BackedLayout`.
pub struct LayoutBuilder<'src, OwnerId, ItemId: TempIndex, DefinedEntity> {
    pub virtual_spaces: PrimaryMap<VirtualSpaceId, VirtualSpaceSpec<OwnerId>>,
    pub segments: PrimaryMap<SegmentId, SegmentSpec<'src>>,
    pub items: EntityCollection<ItemId, ImportedEntity<'src, ItemType>, DefinedEntity, Builder>,
}

impl<'src, OwnerId, ItemId: TempIndex, DefinedEntity>
    LayoutBuilder<'src, OwnerId, ItemId, DefinedEntity>
{
    pub fn new() -> Self {
        Self {
            virtual_spaces: PrimaryMap::new(),
            segments: PrimaryMap::new(),
            items: EntityCollection::empty(),
        }
    }
    ///
    /// Assign each item to a concrete position within its segment and owner unit.
    ///
    /// num_owner_imports is used to convert Temp<OwnerId> to actual OwnerId.
    pub fn seal_at(
        self,
        section_offset: usize,
        to_stable: impl Fn(Temp<OwnerId>) -> OwnerId,
    ) -> SealedLayout<'src, OwnerId, ItemId, DefinedEntity>
    where
        OwnerId: Copy + ReservedValue + TempIndex,
        DefinedEntity: LayoutItemInfo,
        ItemId: Display,
    {
        self.seal_at_unchecked(section_offset, false, to_stable)
    }
    /// Inner method that allow sealing without sorting items.
    /// The caller guarantee that defined items are ordered by segment id.
    ///
    /// If this ivariant is not met, the behaviour is udefined:
    /// - may cause panic,
    /// - may skip some items.
    ///
    pub fn seal_at_unchecked(
        self,
        mut section_offset: usize,
        skip_sort: bool,
        to_stable: impl Fn(Temp<OwnerId>) -> OwnerId,
    ) -> SealedLayout<'src, OwnerId, ItemId, DefinedEntity>
    where
        OwnerId: Copy + ReservedValue + TempIndex,
        DefinedEntity: LayoutItemInfo,
        ItemId: Display,
    {
        let items = self.items.into_finished();
        let (imports, mut defined) = items.into_parts();

        if !skip_sort {
            defined
                .as_mut_slice()
                .sort_unstable_by_key(|v| v.segment_id());
        } else {
            debug_assert!(
                defined
                    .as_mut_slice()
                    .iter()
                    .map(|v| v.segment_id())
                    .is_sorted(),
                "Defined items should be ordered by segment id when skip_sort is true"
            );
        }

        let mut mapping = GappedMap::new();
        let mut segments: PrimaryMap<SegmentId, SealedSegment<OwnerId, ItemId, DefinedEntity>> =
            PrimaryMap::new();

        let chunks = defined.into_iter().chunk_by(|(_, item)| item.segment_id());
        let mut chunks_iter = chunks.into_iter().peekable();

        let mut vs_state = self
            .virtual_spaces
            .into_iter()
            .map(|(_, spec)| VsState::from(spec))
            .collect::<PrimaryMap<_, _>>();

        for (segment_id, segment) in self.segments {
            let vs_spec = &mut vs_state[segment.vs_id];
            let owner_id = to_stable(vs_spec.spec.owner_id);

            // Original segment info
            let alignment = (2usize).pow(segment.align as u32);
            let segment_start = section_offset;

            // Align memory offset of segment to its alignment requirement.
            let padding = calculate_padding(vs_spec.offset, alignment);
            dbg!(padding, alignment, vs_spec.offset);
            vs_spec.offset += padding;

            let mut mem_offset = vs_spec.offset;

            // and shift segment offset by len of header.
            section_offset += <DefinedEntity as LayoutItemInfo>::segment_header_len(
                vs_spec.location(),
                owner_id.as_u32(),
            );

            let grp = match chunks_iter.peek() {
                Some((sid, ..)) if segment_id == *sid => Some(chunks_iter.next().unwrap().1),
                Some((sid, ..)) if segment_id > *sid => {
                    panic!(
                        "Found items for segment {sid} but no spec for it found. Where to place these items?"
                    );
                }
                _ => None,
            };

            let mut parts = Vec::new();
            for (symbol_index, symbol) in grp.into_iter().flatten() {
                let field_alignment = 1 << symbol.pow2align();

                // add padding for alignment
                if let Some((padding_symbol, _)) =
                    Self::try_padding_symbol(mem_offset, field_alignment, segment_id)
                {
                    let padding = padding_symbol.size();
                    log::trace!(
                        "Add padding placeholder before symbol {}: {padding} bytes",
                        symbol_index
                    );

                    parts.push(SealedItem {
                        defined_entity: padding_symbol,
                        item_id: None,
                    });

                    section_offset += padding;
                    mem_offset += padding;
                }
                let symbol_len = symbol.size();
                parts.push(SealedItem {
                    defined_entity: symbol,
                    item_id: Some(symbol_index),
                });
                mapping.insert(
                    symbol_index,
                    ItemOffsets {
                        offsets: Offsets {
                            section_offset,
                            va_address: mem_offset + vs_spec.va_space_start(),
                        },
                        segment_id,
                    },
                );

                section_offset += symbol_len;
                mem_offset += symbol_len;
            }

            segments.push(SealedSegment {
                name: segment.name,
                parts,
                pow2align: segment.align,
                owner: owner_id,
                va_address: vs_spec.location(),
                file_offset: segment_start,
            });
            vs_spec.offset = mem_offset;
        }

        SealedLayout {
            segments,
            imports,
            items_place: mapping,
        }
    }

    fn try_padding_symbol(
        segment_offset: usize,
        alignment: usize,
        segment_id: SegmentId,
    ) -> Option<(DefinedEntity, Cow<'src, str>)>
    where
        DefinedEntity: LayoutItemInfo,
    {
        let name = Cow::Borrowed("padding");
        let padding = calculate_padding(segment_offset, alignment);
        if padding > 0 {
            Some((DefinedEntity::padding_symbol(padding, segment_id), name))
        } else {
            None
        }
    }
}

impl<'src, OwnerId, ItemId: TempIndex, DefinedEntity> Default
    for LayoutBuilder<'src, OwnerId, ItemId, DefinedEntity>
{
    fn default() -> Self {
        Self::new()
    }
}

struct VsState<OwnerId> {
    // current size of virtual space.
    // Used as separate field instead of modifying spec.location because of passive segments.
    offset: usize,
    spec: VirtualSpaceSpec<OwnerId>,
}

impl<OwnerId> From<VirtualSpaceSpec<OwnerId>> for VsState<OwnerId> {
    fn from(mut spec: VirtualSpaceSpec<OwnerId>) -> Self {
        let (offset, location) = match spec.location {
            Some(loc) => (loc.offset() as usize, Some(loc.with_zero_offset())),
            None => (0, None),
        };
        spec.location = location;
        VsState { offset, spec }
    }
}
impl<OwnerId> VsState<OwnerId> {
    fn location(&self) -> Option<SpecificLocation> {
        self.spec
            .location
            .map(|loc| loc.add_offset(self.offset as u32))
    }
    fn va_space_start(&self) -> usize {
        match self.spec.location {
            Some(loc) => loc.offset() as usize,
            None => 0,
        }
    }
}
