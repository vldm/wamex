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
        builder::{SegmentSpec, VirtualSpaceLocation},
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
    /// Id that was used in builder to refer to this item.
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
    /// Name of segment.
    pub name: Cow<'src, str>,
    /// Body of segment, containing defined entities.
    pub parts: PrimaryMap<PartId, SealedItem<DefinedEntity, ItemId>>,

    /// Alignment of segment, represented as power of 2.
    pub pow2align: u8,

    /// Address in virtual memory or table where segment should be placed.
    pub va_address: Option<VirtualSpaceLocation<OwnerId>>,
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

impl<'src> super::MemLayoutSealed<'src> {
    pub fn debug_layout(
        &self,
        file_relocs: &FileRelocs,
        module: &Module<'_>,
        module_name: String,
        print_data_format: &mut impl std::fmt::Write,
        color: bool, // std::io::stdout().is_terminal()
    ) {
        use super::hexdump::SymbolDebugExt;
        writeln!(print_data_format, "<Module {module_name}>").unwrap();

        let mut base = 0;
        for (_, segment) in self.segments.iter() {
            for (_, chunk) in segment.parts.iter() {
                let segment = &segment.name;
                let name = &chunk.defined_entity.debug_name();
                let symbol_index = chunk.item_id.unwrap_or(DataSymbolRef::reserved_value());

                let db = super::hexdump::SymbolDebug {
                    module,
                    file_relocs,
                    segment,
                    symbol_name: name,
                    symbol_index,
                    body: &chunk.defined_entity.body,
                };
                db.debug_symbol_ext(&mut *print_data_format, &mut base, color);
            }
        }
    }

    fn create_from_segments(
        segments: &PrimaryMap<SegmentId, wasmparser::Data<'src>>,
    ) -> Result<Self> {
        let mut defined_items = GappedMap::new();

        let sealed_segments = segments
            .iter()
            .map(|(id, data)| {
                let name: Cow<'src, str> = format!("segment_{id}").into();
                let pow2align = guess_data_alignment(0, 0);
                let va_address = data_kind_to_location(&data.kind);

                let mut parts: PrimaryMap<
                    PartId,
                    SealedItem<DefinedDataChunk<'src>, DataSymbolRef>,
                > = PrimaryMap::new();

                let part_id = parts.push(SealedItem {
                    item_id: None,
                    defined_entity: DefinedEntity {
                        body: EntityBody::Copied(EntityBodyCopy {
                            bytes: data.data,
                            original_range: data.range.clone(),
                            fixups: Vec::new(),
                            filtered_relocs: CompoundBitSet::new(),
                        }),
                        export_as: ExportNames::new(),
                        entity_type: ItemType {
                            segment_id: id,
                            alignment: pow2align,
                        },
                        name: Some(name.clone()),
                    },
                });
                defined_items.insert(
                    DataSymbolRef::new(id.index()),
                    ItemPlace {
                        offsets: Offsets {
                            section_offset: data.range.start,
                            va_address: va_address
                                .as_ref()
                                .map_or(0, |v| v.location.offset() as usize), // TODO
                        },
                        segment_id: id,
                        part_id,
                    },
                );

                let file_offset = data.range.start - data.data.len();
                SealedSegment::<'src> {
                    name,
                    parts,
                    pow2align,
                    va_address,
                    file_offset,
                }
            })
            .collect();

        let num_defined = defined_items.len();
        Ok(Self {
            segments: sealed_segments,
            defined: defined_items,
            external: WithStart::new(DataSymbolRef::new(num_defined), Vec::new()),
        })
    }

    pub fn recover_from_reader(
        reader: &crate::raw::ObjectReader<'src>,
    ) -> anyhow::Result<(Self, FileSymbolDb)> {
        let g = tracing::info_span!("processing_extra_linkage").entered();
        // TODO: add undefined data symbols as well.
        let LinkageInfo::<'src> {
            mut file_symbol_db,
            defined_data_symbols,
        } = LinkageInfo::from_reader(reader);

        drop(g);

        let mut segments = PrimaryMap::new();
        let mut items_place = GappedMap::new();

        if defined_data_symbols.is_empty() && reader.linking.segments_info.is_empty() {
            return Ok((
                Self::create_from_segments(&reader.data.data_segments)?,
                file_symbol_db,
            ));
        }

        // Fill segments first
        for (segment_id, segment_info) in reader.linking.segments_info.iter().enumerate() {
            let segment_id = SegmentId::new(segment_id);
            let name = segment_info.name.into();
            let pow2align = segment_info.alignment.try_into().unwrap();
            let data = &reader.data.data_segments[segment_id];
            segments.push(SealedSegment {
                name,
                parts: PrimaryMap::<_, SealedItem<DefinedDataChunk<'src>, DataSymbolRef>>::new(),
                pow2align,
                va_address: data_kind_to_location(&data.kind),
                file_offset: data.range.start,
            });
        }

        let mut last_range = 0..0;
        let mut prev_symbol_id = None;
        let mut last_segment_id = SegmentId::from_u32(0);
        // TODO: Copy symbols as is (without correcting indexes)
        for (symbol_id, symbol_info, data_symbol_id) in defined_data_symbols.into_iter() {
            let segment_id = symbol_info.segment_id;
            // cleanup prev segment state
            if last_segment_id != segment_id {
                last_range = 0..0;
                prev_symbol_id = None;
            }
            last_segment_id = segment_id;

            let name = symbol_info.name.clone();
            let symbol_in_data = symbol_info.range.start as usize..symbol_info.range.end as usize;
            let segment_data = &reader.data.data_segments[segment_id];
            let chunk = &segment_data.data[symbol_in_data.clone()];

            let segment_align = reader.linking.segments_info[segment_id.index()].alignment;

            #[cfg(debug_assertions)]
            {
                assert_eq!(
                    file_symbol_db.symbols[symbol_id].entity,
                    data_symbol_id.into()
                );
            }
            // In bound symbol
            if last_range.end >= symbol_in_data.end {
                log::warn!(
                    "Detected overlapping data symbol {}, overlaps with {prev_name}. Patching symbol db.",
                    name,
                    prev_name = segments[segment_id]
                        .parts
                        .last()
                        .map(|(_, item)| item.defined_entity.debug_name())
                        .unwrap_or("<unknown>"),
                );
                let mut item = file_symbol_db.symbols[prev_symbol_id.unwrap()];

                item.offset_in_entity = symbol_in_data.start as u32 - last_range.start as u32;
                file_symbol_db.symbols[symbol_id] = item;
                continue;
            }

            debug_assert!(
                last_range.end <= symbol_in_data.start,
                "Data symbols are expected to be sorted by their offset in segment, but symbol {:?} has range {:?} that intersects with previous symbol range  {:?}",
                symbol_id,
                symbol_in_data,
                last_range
            );

            // Offset in file of section segment data buffer start
            let segment_file_offset = segment_data.range.end - segment_data.data.len();
            let file_offset = segment_file_offset + symbol_in_data.start;

            // Offset of symbol in VA space.
            let mem_offset = symbol_in_data.start;

            let field_alignment = guess_data_alignment(segment_align as u8, symbol_in_data.start);

            if last_range.end < symbol_in_data.start {
                let gap_range = last_range.end..symbol_in_data.start;

                if gap_range.len() >= (1 << field_alignment) {
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
                // add padding item;
                let padding = DefinedEntity::padding_symbol(gap_range.len(), segment_id);
                let item = SealedItem {
                    item_id: None,
                    defined_entity: padding,
                };
                segments[segment_id].parts.push(item);
            }

            last_range = symbol_in_data.clone();
            prev_symbol_id = Some(symbol_id);

            let file_range = file_offset..file_offset + chunk.len();
            assert_eq!(&reader.tmp_src[file_range.clone()], chunk);

            log::trace!(
                "Data symbol: {data_symbol_id} at {segment_id} offset: {}, size: {}, alignment: {field_alignment}, file_location:{:?}",
                symbol_in_data.start,
                symbol_in_data.len(),
                file_offset,
            );
            let item = SealedItem {
                item_id: Some(data_symbol_id),
                defined_entity: DefinedEntity {
                    body: EntityBody::Copied(EntityBodyCopy {
                        bytes: chunk,
                        original_range: file_range.clone(),
                        fixups: Vec::new(),
                        filtered_relocs: CompoundBitSet::new(),
                    }),
                    export_as: ExportNames::new(),
                    entity_type: ItemType {
                        segment_id,
                        alignment: field_alignment,
                    },
                    name: Some(name.clone()),
                },
            };
            let part_id = segments[segment_id].parts.push(item);
            items_place.insert(
                data_symbol_id,
                ItemPlace {
                    offsets: Offsets {
                        section_offset: file_range.start,
                        va_address: mem_offset, // TODO + segment offset?
                    },
                    segment_id,
                    part_id,
                },
            );
        }

        let num_defined = items_place.len();
        Ok((
            Self {
                segments,
                defined: items_place,
                // TODO: take from linkage info.
                external: WithStart::new(DataSymbolRef::new(num_defined), Vec::new()),
            },
            file_symbol_db,
        ))
    }

    /// Recover virtual address spaces that was used:
    /// - merge segments that ends just after another starts
    /// - split passive and active
    /// - detect got based
    /// - detect multiple memories.
    ///
    /// Note: can reorder segments in output
    ///
    /// Returns MemLayoutBuilder without items.
    pub fn recover_vs_segments(
        &self,
        mut import_mem: impl FnMut(MemoryRef) -> Temp<MemoryRef>,
    ) -> MemLayoutBuilder<'src> {
        let mut builder = MemLayoutBuilder::new();

        let mut vs_recover = VsRecover::new();

        for (id, segment) in &self.segments {
            vs_recover.add_segment(id, segment);
        }
        for (_, segments, vs_location) in vs_recover.iter_vs() {
            let location = vs_location.map(|loc| VirtualSpaceLocation {
                owner_id: import_mem(loc.owner_id),
                location: loc.location,
            });
            let vs_id = builder.virtual_spaces.push(location);
            for segment in segments {
                let _ = builder.segments.push(SegmentSpec {
                    vs_id,
                    name: self.segments[segment].name.clone(),
                    align: self.segments[segment].pow2align,
                    segment_flags: SegmentFlags::from_name(&self.segments[segment].name),
                });
            }
        }

        builder
    }
}

//
// Virtual space recover helpers
//
#[derive(Debug, Clone, PartialEq, Eq, Ord, PartialOrd)]
struct VsKey {
    vs_location: Option<VirtualSpaceLocation<MemoryRef>>,
    // Bump number to distinguish different virtual spaces with same base and got (e.g. multiple passive segments)
    // If segments cannot be merged (intersects) - try to keep their original offsets.
    bump_num: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VsState {
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

struct VsRecover {
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
    ) -> impl Iterator<
        Item = (
            VirtualSpaceId,
            Vec<SegmentId>,
            Option<VirtualSpaceLocation<MemoryRef>>,
        ),
    > + '_ {
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
                if existing.vs_location.is_none() {
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
            None => (
                VsKey {
                    vs_location: None,
                    bump_num: 0,
                },
                0,
            ),
            Some(mut s) => {
                let offset = s.location.offset() as usize;
                s.location = s.location.with_zero_offset();
                (
                    VsKey {
                        vs_location: Some(s),
                        bump_num: 0,
                    },
                    offset,
                )
            }
        }
    }
}

// fn element_kind_to_location(
//     element_kind: &wasmparser::ElementKind,
// ) -> Option<(SpecificLocation, MemoryRef)> {
//     match element_kind {
//         wasmparser::ElementKind::Passive => None,
//         wasmparser::ElementKind::Active {
//             memory_index: _,
//             offset_expr,
//         } => Some(
//             SpecificLocation::try_from_const_expr(offset_expr)
//                 .expect("Only const offset supported for active data segments"),
//         ),
//     }
// }

fn data_kind_to_location(
    data_kind: &wasmparser::DataKind,
) -> Option<VirtualSpaceLocation<MemoryRef>> {
    match data_kind {
        wasmparser::DataKind::Passive => None,
        wasmparser::DataKind::Active {
            memory_index,
            offset_expr,
        } => {
            let location = SpecificLocation::try_from_const_expr(offset_expr)
                .expect("Only const offset supported for active data segments");
            Some(VirtualSpaceLocation {
                owner_id: MemoryRef::from_u32(*memory_index),
                location,
            })
        }
    }
}
//
// Encode helpers
//

fn data_segment_adapter<W, DefinedEntity, ItemId>(
    encoder: &mut wasm_emitter::Encoder<W>,
    location: Option<VirtualSpaceLocation<DefinedEntity::OwnerId>>,
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
