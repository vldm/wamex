use std::{borrow::Cow, collections::BTreeMap, io::Write};

use anyhow::Result;
use cranelift_bitset::CompoundBitSet;
use cranelift_entity::{EntityRef, PrimaryMap, packed_option::ReservedValue};

use super::{ItemType, Offsets};
use crate::{
    emit::modify::wasm_emitter::{self, EncodeWithRelocOffset, SectionList},
    index::GappedMap,
    layouts::{
        DefinedDataChunk, LayoutItemInfo, VirtualSpaceId,
        builder::{SegmentSpec, VirtualSpaceLocation},
        guess_data_alignment,
    },
    linkage::{LinkageInfo, file_db::FileRelocs},
    raw::SegmentId,
    typed::{
        DefinedEntity, EntityBody, EntityBodyCopy, ExportNames, GlobalRef, ImportedEntity,
        IterBytes, MemoryRef, Module,
        data::{DataSymbolRef, SpecificLocation},
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
pub struct ItemOffsets {
    pub offsets: Offsets,
    pub segment_id: SegmentId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedSegment<'src, OwnerId, ItemId: EntityRef, DefinedEntity> {
    /// Name of segment.
    pub name: Cow<'src, str>,
    /// Body of segment, containing defined entities.
    pub parts: Vec<SealedItem<DefinedEntity, ItemId>>,

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
            .iter()
            .map(|chunk| chunk.defined_entity.size())
            .sum();

        let iter: std::iter::FlatMap<
            std::slice::Iter<'_, SealedItem<DefinedEntity, ItemId>>,
            IterBytes<'_>,
            for<'a> fn(&'a SealedItem<DefinedEntity, ItemId>) -> IterBytes<'a>,
        > = self
            .parts
            .iter()
            .flat_map(|chunk| chunk.defined_entity.iter_chunks());
        DataStream { iter, total_size }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedLayout<'src, OwnerId, ItemId: EntityRef, DefinedEntity> {
    pub segments: PrimaryMap<SegmentId, SealedSegment<'src, OwnerId, ItemId, DefinedEntity>>,

    /// Imported items that left after sealing.
    pub imports: PrimaryMap<ItemId, ImportedEntity<'src, ()>>,
    pub items_place: GappedMap<ItemId, ItemOffsets>,
}

impl<'src, OwnerId, ItemId: EntityRef, DefinedEntity>
    SealedLayout<'src, OwnerId, ItemId, DefinedEntity>
where
    DefinedEntity: LayoutItemInfo<OwnerId = OwnerId>,
    OwnerId: EntityRef,
{
    pub fn encode<W>(&self, mut writer: SectionList<W>) -> Result<(), std::io::Error>
    where
        W: std::io::Write,
    {
        writer.item_from_encoder(|e| {
            for segment in self.segments.values() {
                data_segment_adapter(e, segment.va_address, segment.data_stream())?;
            }
            Ok(())
        })
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
            for chunk in segment.parts.iter() {
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

    pub fn recover_from_reader(reader: &crate::raw::ObjectReader<'src>) -> anyhow::Result<Self> {
        let g = tracing::info_span!("processing_extra_linkage").entered();
        // TODO: add undefined data symbols as well.
        let LinkageInfo::<'src> {
            mut file_symbol_db,
            defined_data_symbols,
        } = LinkageInfo::from_reader(reader);

        drop(g);

        let mut segments = PrimaryMap::new();
        let mut items_place = GappedMap::new();

        // Fill segments first
        for (segment_id, segment_info) in reader.linking.segments_info.iter().enumerate() {
            let segment_id = SegmentId::new(segment_id);
            let name = segment_info.name.into();
            let pow2align = segment_info.alignment.try_into().unwrap();
            let data = &reader.data.data_segments[segment_id];
            segments.push(SealedSegment {
                name,
                parts: Vec::<SealedItem<DefinedDataChunk<'src>, DataSymbolRef>>::new(),
                pow2align,
                va_address: data_kind_to_location(&data.kind),
                file_offset: data.range.start,
            });
        }

        let mut last_range = 0..0;
        let mut prev_symbol_id = None;
        // TODO: Copy symbols as is (without correcting indexes)
        for (def_id, (symbol_id, symbol_info)) in defined_data_symbols.into_iter().enumerate() {
            let segment_id = symbol_info.segment_id;
            let data_symbol_id = DataSymbolRef::new(def_id);

            let name = symbol_info.name.clone();
            let symbol_in_data = symbol_info.range.start as usize..symbol_info.range.end as usize;
            let segment_data = &reader.data.data_segments[segment_id];
            let chunk = &segment_data.data[symbol_in_data.clone()];

            let segment_align = reader.linking.segments_info[segment_id.index()].alignment;

            // In bound symbol
            if last_range.end < symbol_in_data.start {
                log::warn!(
                    "Detected overlapping data symbol {}, overlaps with {prev_name}. Patching symbol db.",
                    name,
                    prev_name = segments[segment_id]
                        .parts
                        .last()
                        .map(|item: &SealedItem::<_, _>| item.defined_entity.debug_name())
                        .unwrap_or("<unknown>"),
                );
                let mut item = file_symbol_db.symbols[prev_symbol_id.unwrap()];
                item.offset_in_entity = last_range.start as u32 - symbol_in_data.start as u32;
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
            let file_offset = segment_data.range.end - segment_data.data.len();

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

            log::trace!(
                "Data symbol: offset: {}, size: {}, alignment: {}",
                symbol_in_data.start,
                symbol_in_data.len(),
                field_alignment
            );

            last_range = symbol_in_data.clone();
            prev_symbol_id = Some(symbol_id);

            let item = SealedItem {
                item_id: Some(data_symbol_id),
                defined_entity: DefinedEntity {
                    body: EntityBody::Copied(EntityBodyCopy {
                        bytes: chunk,
                        original_range: symbol_in_data.clone(),
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
            segments[segment_id].parts.push(item);
            items_place.insert(
                data_symbol_id,
                ItemOffsets {
                    offsets: Offsets {
                        section_offset: file_offset,
                        va_address: mem_offset, // TODO + segment offset?
                    },
                    segment_id,
                },
            );
        }

        Ok(Self {
            segments,
            items_place,
            // TODO: take from linkage info.
            imports: PrimaryMap::new(),
        })
    }
}
g
//
// Virtual space recover helpers
//
#[derive(Debug, Clone, PartialEq, Eq, Ord, PartialOrd)]
struct VsKey {
    // None mean that segment is passive, otherwise - active with memory reference.
    base: Option<MemoryRef>,
    got_base: Option<GlobalRef>,
    // If segments cannot be merged (intersects) - try to keep their original offsets.
    bump_num: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Ord, PartialOrd)]
struct VsState {
    va_start: Option<SpecificLocation>,
    len: usize,
    vs_id: VirtualSpaceId,
}
/// Recover virtual address spaces that was used:
/// - merge segments that ends just after another starts
/// - split passive and active
/// - detect got based
/// - detect multiple memories.
/// Note: will can reorder segments in output

struct VsRecover {
    // Map from virtual space key to its state.
    virtual_spaces: BTreeMap<VsKey, VsState>,

    next_vs_id: VirtualSpaceId,
}
impl VsRecover {
    fn new() -> Self {
        Self {
            virtual_spaces: BTreeMap::new(),
            next_vs_id: VirtualSpaceId::new(0),
        }
    }

    fn add_segment(
        &mut self,
        segment_id: SegmentId,
        segment_info: &SegmentSpec,
        data: &wasmparser::Data,
    ) {
        let (key, offset) = Self::get_vs_key_base(data);
        todo!()
    }

    // Get VsKey with bump = 0.
    fn get_vs_key_base(data: &wasmparser::Data) -> (VsKey, usize) {
        match &data.kind {
            wasmparser::DataKind::Passive => (
                VsKey {
                    base: None,
                    got_base: None,
                    bump_num: 0,
                },
                0,
            ),
            wasmparser::DataKind::Active {
                memory_index,
                offset_expr,
            } => {
                let memory_ref = MemoryRef::from_u32(*memory_index);
                let location = SpecificLocation::try_from_const_expr(&offset_expr)
                    .expect("Only const offset supported for active data segments");
                (
                    VsKey {
                        base: Some(memory_ref),
                        got_base: location.global_ref(),
                        bump_num: 0,
                    },
                    location.offset() as usize,
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

pub fn data_segment_adapter<W, DefinedEntity, ItemId>(
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

    let encoded = <DefinedEntity as LayoutItemInfo>::segment_header_start(location)?;
    encoder.push_bytes(&encoded)?;
    data_stream.encode(encoder)?;
    Ok(())
}

#[derive(Debug)]
pub struct DataStream<'a, DefinedEntity, ItemId> {
    pub(crate) iter: std::iter::FlatMap<
        std::slice::Iter<'a, SealedItem<DefinedEntity, ItemId>>,
        IterBytes<'a>,
        for<'b> fn(&'b SealedItem<DefinedEntity, ItemId>) -> IterBytes<'b>,
    >,
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
