use std::borrow::Cow;

use anyhow::Result;
use cranelift_bitset::CompoundBitSet;
use cranelift_entity::{EntityRef, PrimaryMap, packed_option::ReservedValue};
use smallvec::smallvec;

pub use self::{builder::*, sealed::*};
use crate::{
    SVec,
    emit::modify::wasm_emitter,
    index::{GappedMap, Temp, WithStart},
    layouts::{PartId, SegmentPlacement, recover::VsRecover},
    linkage::{
        LinkageInfo,
        file_db::{FileRelocs, FileSymbolDb},
    },
    raw::SegmentId,
    typed::{
        DefinedDataChunk, DefinedEntity, EntityBody, EntityBodyCopy, ExportNames, ImportOrDefined,
        ImportedEntity, MemoryRef, Module,
    },
};

impl_entity_index! {
    #[display="data"]
    pub struct DataSymbolRef;
}

mod builder;
pub mod hexdump;
mod sealed;

// Align base of memory to 16 bytes, if it wasn't already aligned.
pub const BASE_ALIGNMENT: u8 = u8::trailing_zeros(16) as u8;

pub const DEFAULT_HEAP_START: SegmentPlacement = SegmentPlacement::ConstantOffset(0x100000);

pub type ImportedDataChunk<'src> = ImportedEntity<'src, ()>;

impl DefinedDataChunk<'_> {
    #[must_use]
    pub fn debug_name(&self) -> &str {
        self.name.as_deref().unwrap_or("<unnamed>")
    }
    fn padding_symbol(padding: usize, segment_id: SegmentId) -> Self {
        Self {
            entity_type: ItemType::data_chunk(segment_id, 0),
            name: Some(Cow::Borrowed("padding")),
            export_as: ExportNames::new(),
            body: EntityBody::new_empty(smallvec![0; padding]),
        }
    }
}

// LayoutBuilder<'src, MemoryRef, DataSymbolRef, DefinedDataChunk<'src>>;

//
// Impl for sealed
//
impl<'src> MemLayoutSealed<'src> {
    #[must_use]
    pub fn get_entity(
        &self,
        data_ref: DataSymbolRef,
    ) -> ImportOrDefined<&ImportedDataChunk<'src>, &DefinedDataChunk<'src>> {
        if let Some(imp) = self.external.get(data_ref) {
            return ImportOrDefined::External(imp);
        }

        let defined_place = self.defined.get(data_ref).expect("data should exist");
        let sealed = &self.segments[defined_place.segment_id].parts[defined_place.part_id];
        ImportOrDefined::Defined(&sealed.defined_entity)
    }

    pub fn defined_iter(&self) -> impl Iterator<Item = (DataSymbolRef, &DefinedDataChunk<'src>)> {
        self.defined.iter().map(|(id, place)| {
            (
                id,
                &self.segments[place.segment_id].parts[place.part_id].defined_entity,
            )
        })
    }

    pub fn iter(
        &self,
    ) -> impl Iterator<
        Item = (
            DataSymbolRef,
            ImportOrDefined<&ImportedDataChunk<'src>, &DefinedDataChunk<'src>>,
        ),
    > {
        let external = self
            .external
            .iter()
            .map(|(id, external)| (id, ImportOrDefined::External(external)));

        let defined = self
            .defined_iter()
            .map(|(id, v)| (id, ImportOrDefined::Defined(v)));
        defined.chain(external)
    }

    pub fn modify_bodies(
        &mut self,
        mut op: impl FnMut(DataSymbolRef, &mut DefinedDataChunk<'src>),
    ) {
        for (data_ref, place) in self.defined.iter() {
            op(
                data_ref,
                &mut self.segments[place.segment_id].parts[place.part_id].defined_entity,
            );
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.defined_len() + self.external.len()
    }
    #[must_use]
    pub fn defined_len(&self) -> usize {
        self.defined
            // get last defined in case of gapps (symbols that was pushed, but later was merged into another)
            .last_key()
            .map(|r| r.index() + 1)
            .unwrap_or_default()
    }

    #[must_use]
    pub fn stable_id(&self, id: Temp<DataSymbolRef>) -> DataSymbolRef {
        id.to_stable(0, self.defined.len())
    }

    pub fn debug_layout(
        &self,
        file_relocs: &FileRelocs,
        module: &Module<'_>,
        module_name: String,
        print_data_format: &mut impl std::fmt::Write,
        color: bool, // std::io::stdout().is_terminal()
    ) {
        use hexdump::SymbolDebugExt;
        writeln!(print_data_format, "<Module {module_name}>").unwrap();

        let mut base = 0;
        for (_, segment) in self.segments.iter() {
            for (_, chunk) in segment.parts.iter() {
                let segment = &segment.name;
                let name = &chunk.defined_entity.debug_name();
                let symbol_index = chunk.item_id.unwrap_or(DataSymbolRef::reserved_value());

                let db = hexdump::SymbolDebug {
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

                let mut parts: PrimaryMap<PartId, SealedDataItem> = PrimaryMap::new();

                let part_id = parts.push(SealedDataItem {
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
                    DataItemPlace {
                        offsets: Offsets {
                            section_offset: data.range.start,
                            va_address: va_address.location().map_or(0, |v| v.offset() as usize), // TODO
                        },
                        segment_id: id,
                        part_id,
                    },
                );

                let file_offset = data.range.start - data.data.len();
                SealedDataSegment::<'src> {
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

    pub fn from_reader(
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
            segments.push(SealedDataSegment {
                name,
                parts: PrimaryMap::<_, SealedDataItem>::new(),
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
                let item = SealedDataItem {
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
            let item = SealedDataItem {
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
                DataItemPlace {
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
    /// Returns `MemLayoutBuilder` without items.
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
            let location = match vs_location {
                DataKind::Active {
                    memory_ref,
                    location,
                } => DataKind::Active {
                    memory_ref: import_mem(memory_ref),
                    location,
                },
                DataKind::Passive => DataKind::Passive,
            };
            let vs_id = builder.virtual_spaces.push(location);
            for segment in segments {
                let _ = builder.segments.push(DataSegmentSpec {
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
// Impl for builder
//

impl<'src> MemLayoutBuilder<'src> {
    /// Create default virtual space, inited as active with one segment.
    ///
    /// Return id of this segment
    pub fn try_create_base_segment(&mut self, owner_id: Temp<MemoryRef>) -> SegmentId {
        let vs = self.try_create_base_vs(owner_id, DEFAULT_HEAP_START);
        self.try_create_segment(vs)
    }
    pub fn push_defined(&mut self, defined: DefinedDataChunk<'src>) -> Temp<DataSymbolRef> {
        self.items.push_defined(defined)
    }
    /// Only external imports is allowed.
    pub fn push_import(&mut self, import: ImportedDataChunk<'src>) -> Temp<DataSymbolRef> {
        self.items.push_external(import)
    }
    pub fn push_entity(
        &mut self,
        entity: ImportOrDefined<ImportedDataChunk<'src>, DefinedDataChunk<'src>>,
    ) -> Temp<DataSymbolRef> {
        self.items.push_entity(entity)
    }
    #[must_use]
    pub fn dry_push_entity(
        &self,
        entity: &ImportOrDefined<ImportedDataChunk<'src>, DefinedDataChunk<'src>>,
    ) -> Temp<DataSymbolRef> {
        self.items.dry_push_entity(entity)
    }
    ///
    /// Return main active virtual space.
    ///
    pub fn main_vs(&self) -> Option<DataKind<Temp<MemoryRef>>> {
        self.virtual_spaces
            .values()
            .copied()
            .find(DataKind::is_active)
    }
}

pub type DataSymbolsOffsets = GappedMap<DataSymbolRef, DataItemPlace>;

fn data_kind_to_location(data_kind: &wasmparser::DataKind) -> DataKind<MemoryRef> {
    match data_kind {
        wasmparser::DataKind::Passive => DataKind::Passive,
        wasmparser::DataKind::Active {
            memory_index,
            offset_expr,
        } => {
            let location = SegmentPlacement::try_from_const_expr(offset_expr)
                .expect("Only const offset supported for active data segments");
            DataKind::Active {
                memory_ref: MemoryRef::from_u32(*memory_index),
                location,
            }
        }
    }
}

#[derive(Copy, Debug, Clone, Eq, PartialEq)]
pub struct Offsets {
    /// Offset of symbol in wasm file relative to section start.
    pub section_offset: usize,
    /// Offset of symbol in virtual address space.
    /// Either offset in memory, or index in table - both related to `segment_location`.
    pub va_address: usize,
}

impl ReservedValue for Offsets {
    fn reserved_value() -> Self {
        Self {
            section_offset: usize::MAX,
            va_address: usize::MAX,
        }
    }
    fn is_reserved_value(&self) -> bool {
        self.section_offset == usize::MAX && self.va_address == usize::MAX
    }
}

impl ReservedValue for DataItemPlace {
    fn reserved_value() -> Self {
        Self {
            offsets: ReservedValue::reserved_value(),
            segment_id: SegmentId::reserved_value(),
            part_id: PartId::reserved_value(),
        }
    }
    fn is_reserved_value(&self) -> bool {
        self.offsets.is_reserved_value()
            && self.segment_id.is_reserved_value()
            && self.part_id.is_reserved_value()
    }
}

#[derive(Default, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ItemType {
    pub(crate) segment_id: SegmentId,
    pub(crate) alignment: u8,
}

impl ItemType {
    #[must_use]
    pub fn element_item(segment_id: SegmentId) -> Self {
        Self {
            segment_id,
            alignment: 0,
        }
    }
    #[must_use]
    pub fn data_chunk(segment_id: SegmentId, align: u8) -> Self {
        Self {
            segment_id,
            alignment: align,
        }
    }
}

// impl<'src> LayoutItemInfo for DefinedEntity<'src, ItemType> {
//     type OwnerId = MemoryRef;
//     fn debug_name(&self) -> &str {
//         self.name.as_deref().unwrap_or("<unnamed>")
//     }
//     fn segment_id(&self) -> SegmentId {
//         self.entity_type.segment_id
//     }
//     fn iter_chunks(&self) -> IterBytes<'_> {
//         self.body.iter_chunks()
//     }
//     fn pow2align(&self) -> u8 {
//         self.entity_type.alignment
//     }
//     fn size(&self) -> usize {
//         self.body.len()
//     }
//     fn padding_symbol(padding: usize, segment_id: SegmentId) -> Self {
//         Self {
//             entity_type: ItemType::data_chunk(segment_id, 0),
//             name: Some(Cow::Borrowed("padding")),
//             export_as: ExportNames::new(),
//             body: EntityBody::new_empty(smallvec![0; padding]),
//         }
//     }
//     fn segment_header_start(
//         location: VirtualSpaceKind<Self::OwnerId>,
//     ) -> Result<SVec<u8, 32>, std::io::Error> {
//         data_segment_header(location)
//     }
// }

fn guess_data_alignment(segment_pow2align: u8, start_mem_offset: usize) -> u8 {
    let start_align = start_mem_offset
        .trailing_zeros()
        .try_into()
        .unwrap_or(u8::MAX);

    // Can't exceed the segment's alignment
    segment_pow2align.min(start_align)
}

fn calculate_padding(starting_point: usize, alignment: usize) -> usize {
    let misalignment = starting_point % alignment;
    if misalignment == 0 {
        0
    } else {
        alignment - misalignment
    }
}

fn data_segment_header_len(location: DataKind<MemoryRef>) -> usize {
    data_segment_header_start(location).unwrap().len() + 5
}
fn data_segment_header_start(
    location: DataKind<MemoryRef>,
) -> Result<SVec<u8, 32>, std::io::Error> {
    log::error!("data_segment_header_start: {location:?}");
    Ok(match location {
        DataKind::Passive => {
            // passive segment
            let mut v = SVec::new();
            v.push(0x01); // flag for passive segment
            v
        }
        DataKind::Active {
            memory_ref,
            location,
        } => {
            let mut result = SVec::new();
            let mut encoder = wasm_emitter::Encoder::new(&mut result, 0);
            let memory_index = memory_ref.as_u32();
            if memory_index == 0 {
                encoder.push_byte(0x00)?; // active segment in default memory
            } else {
                encoder.push_byte(0x02)?; // active segment with explicit memory index
                encoder.encode_leb_5byte(memory_index)?;
            }
            let offset = location.to_init_expr();
            encoder.encode_const_expr(&offset)?;
            result
        }
    })
}

#[cfg(test)]
mod tests {
    use smallvec::smallvec;

    use super::*;
    use crate::{
        index::Temp,
        typed::{DefinedEntity, EntityBody, ExportNames},
    };

    // Create a data from scratch and try to seal it.
    #[test]
    fn create_and_seal() {
        let mem_id = Temp::from_import(0);

        let mut builder = MemLayoutBuilder::new();
        let vs_id = builder.virtual_spaces.push(DataKind::Active {
            memory_ref: mem_id,
            location: SegmentPlacement::ConstantOffset(3), // some unaligned offset
        });

        let segment_id = builder.segments.push(DataSegmentSpec {
            vs_id,
            name: "segment1".into(),
            align: 2,
            segment_flags: SegmentFlags::Writable,
        });

        let item1_id = builder.items.push_defined(DefinedEntity {
            entity_type: ItemType::data_chunk(segment_id, 1),
            name: Some("data1".into()),
            export_as: ExportNames::new(),
            body: EntityBody::new_empty(smallvec![3, 4, 5]),
        });

        let _item2_id = builder.items.push_defined(DefinedEntity {
            entity_type: ItemType::data_chunk(segment_id, 2),
            name: Some("data2".into()),
            export_as: ExportNames::new(),
            body: EntityBody::new_empty(smallvec![1, 2, 3, 4]),
        });

        let sealed = builder.seal_at(0, |temp| temp.to_stable(0, 0));

        dbg!(&sealed);
        let output = sealed.segments[segment_id]
            .data_stream()
            .flatten()
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(
            output,
            vec![
                3, 4, 5, // first item
                0, // padding
                1, 2, 3, 4 // data
            ]
        );
        let data_ref = item1_id.to_stable(0, 0);
        let item = sealed.defined.get(data_ref).unwrap();
        assert_eq!(item.segment_id, segment_id);
        assert_eq!(item.offsets.va_address, 4); // offset of first item in VA <- 3 byte offset of storage + padding of 1 byte
        assert_eq!(item.offsets.section_offset, 9); // offset of first item in file 9 byte header
        let segment_offset = sealed.segments[segment_id]
            .va_address
            .location()
            .unwrap()
            .offset();

        assert_eq!(segment_offset, 4); // offset of segment in virtual memory ( 3 byte offset of storage + padding of 1 byte)
    }

    #[test]
    fn multiple_segments_padding() {
        let mem_id = Temp::from_import(0);

        let mut builder = MemLayoutBuilder::new();
        let vs_id = builder.virtual_spaces.push(DataKind::Active {
            memory_ref: mem_id,
            location: SegmentPlacement::ConstantOffset(3), // some unaligned offset
        });

        let segment1_id = builder.segments.push(DataSegmentSpec {
            vs_id,
            name: "segment1".into(),
            align: 2,
            segment_flags: SegmentFlags::Writable,
        });

        let segment2_id = builder.segments.push(DataSegmentSpec {
            vs_id,
            name: "segment2".into(),
            align: 4,
            segment_flags: SegmentFlags::Writable,
        });

        let first_data = builder.items.push_defined(DefinedEntity {
            entity_type: ItemType::data_chunk(segment1_id, 1),
            name: Some("data1".into()),
            export_as: ExportNames::new(),
            body: EntityBody::new_empty(smallvec![3, 4, 5]),
        });

        let second_data = builder.items.push_defined(DefinedEntity {
            entity_type: ItemType::data_chunk(segment2_id, 2),
            name: Some("data2".into()),
            export_as: ExportNames::new(),
            body: EntityBody::new_empty(smallvec![1, 2, 3, 4]),
        });

        let sealed = builder.seal_at(0, |temp| temp.to_stable(0, 0));

        dbg!(&sealed);

        assert!(
            sealed.segments[segment1_id]
                .va_address
                .location()
                .unwrap()
                .offset()
                .is_multiple_of(1 << sealed.segments[segment1_id].pow2align)
        ); // check alignment of first segment
        assert!(
            sealed.segments[segment2_id]
                .va_address
                .location()
                .unwrap()
                .offset()
                .is_multiple_of(1 << sealed.segments[segment2_id].pow2align)
        ); // check alignment of second segment

        assert_eq!(sealed.segments[segment1_id].file_offset, 0);
        assert_eq!(sealed.segments[segment2_id].file_offset, 9 + 3); // header of first segment + data len
        let output = sealed.segments[segment1_id]
            .data_stream()
            .flatten()
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(
            output,
            vec![
                3, 4, 5, // first item
            ]
        );
        let output = sealed.segments[segment2_id]
            .data_stream()
            .flatten()
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(
            output,
            vec![
                1, 2, 3, 4 // data
            ]
        );

        let item = sealed.defined.get(first_data.to_stable(0, 0)).unwrap();
        assert_eq!(item.segment_id, segment1_id);
        assert_eq!(item.offsets.va_address, 4); // offset of first item in VA <- 3 byte offset of storage + padding of 1 byte
        assert_eq!(item.offsets.section_offset, 9); // offset of first item in file 9 byte header

        let item = sealed.defined.get(second_data.to_stable(0, 0)).unwrap();
        assert_eq!(item.segment_id, segment2_id);
        assert_eq!(item.offsets.va_address, 16); // offset of second item in (allign of segment 2)
        assert_eq!(item.offsets.section_offset, 21); // 1st segment header (9) + 1st segment data (3) + 2nd segment header (9)
    }

    use crate::typed::LoadedFile;

    #[test]
    fn test_layouts() {
        env_logger::try_init().ok();
        // assert_layout_same("simpl_graph", crate::testfiles::SIMPLE_GRAPH);
        assert_layout_same("example", crate::testfiles::EXAMPLE_WASM);
        assert_layout_same("lazy_routes", crate::testfiles::LAZY_ROUTES);
    }
    fn assert_layout_same(file_name: &str, bytes: &[u8]) {
        let file = LoadedFile::from_wasm_bytes(bytes).unwrap();

        // dbg!(&layout);
        let mut print_data_format = String::new();
        file.module.extra.mem_layout.debug_layout(
            &file.relocs,
            &file.module,
            String::from("test"),
            &mut print_data_format,
            false,
        );
        insta::assert_snapshot!(file_name, print_data_format);
    }
}
