//!
//! Layouts representation for data and elements.
//!
//! After creating module and filling it with elements and data entities,
//! we need to assign them to specific offsets in the file and (more importantly) in the virtual address space.
//! This addresses are used in instructions to require needed data chunk or element.
//!
//! The layout can be in two states:
//! - `Sealed` - when each of elements is assigned to a specific offset in the virtual address space.
//!   The bound is indirect and can be represented as tupple `(unit_id, segment_id, offset)`:
//!   1. The full layout is splitted into multiple units (`memory` for data and `table` for elements).
//!      Each unit has isolated address space.
//!   2. Each unit is splitted into segments, which are contiguous ranges of bytes/elements.
//!      The segment might have some constant starting offset, or can offset based on some expression
//!      (currently only got based expressions supported). Additionally, segment can be passive,
//!      which means that it doesn't have init offset, and all relocations should be done dynamically.
//! - `Builder` - when layout is represented as a seperate:
//!   1. list of segments and their information (placement, alignment, etc.)
//!   2. list of elements/data chunks and information about segment they belong to.
//!

use std::borrow::Cow;

use cranelift_entity::packed_option::ReservedValue;
use smallvec::smallvec;

use crate::{
    SVec,
    emit::modify::wasm_emitter,
    typed::{
        DefinedEntity, EntityBody, ExportNames, IterBytes, MemoryRef, TableRef,
        data::{DataSymbolRef, SpecificLocation},
        elements::ElementItemId,
    },
};

impl_entity_index! {
    #[display = "segment"]
    pub struct SegmentId;
    #[display = "vs"]
    pub struct VirtualSpaceId;
}

mod builder;
mod hexdump;
mod sealed;

use builder::*;
use sealed::*;

type DefinedDataChunk<'src> = DefinedEntity<'src, ItemType>;

pub type MemLayoutBuilder<'src> =
    LayoutBuilder<'src, MemoryRef, DataSymbolRef, DefinedDataChunk<'src>>;
pub type MemLayoutSealed<'src> =
    SealedLayout<'src, MemoryRef, DataSymbolRef, DefinedDataChunk<'src>>;

pub type ElementLayoutBuilder<'src, T> = LayoutBuilder<'src, TableRef, ElementItemId, T>;
pub type ElementLayoutSealed<'src, T> = SealedLayout<'src, TableRef, ElementItemId, T>;

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

impl ReservedValue for ItemOffsets {
    fn reserved_value() -> Self {
        Self {
            offsets: ReservedValue::reserved_value(),
            segment_id: SegmentId::reserved_value(),
        }
    }
    fn is_reserved_value(&self) -> bool {
        self.offsets.is_reserved_value() && self.segment_id.is_reserved_value()
    }
}

#[derive(Debug)]
pub struct ItemType {
    segment_id: SegmentId,
    align: u8,
}

impl ItemType {
    pub fn element_item(segment_id: SegmentId) -> Self {
        Self {
            segment_id,
            align: 0,
        }
    }
    pub fn data_chunk(segment_id: SegmentId, align: u8) -> Self {
        Self { segment_id, align }
    }
}

/// Representation of item layout within segment.
pub trait LayoutItemInfo {
    fn segment_id(&self) -> SegmentId;
    fn pow2align(&self) -> u8;
    // TODO: Allow implementing another type for elements.
    fn iter_chunks(&self) -> IterBytes<'_>;
    fn size(&self) -> usize;
    fn debug_name(&self) -> &str;
    fn padding_symbol(padding: usize, segment_id: SegmentId) -> Self
    where
        Self: Sized;

    // Encode segment header without len of data.
    fn segment_header_start(
        location: Option<SpecificLocation>,
        owner_index: u32,
    ) -> Result<SVec<u8, 32>, std::io::Error>;

    fn segment_header_len(location: Option<SpecificLocation>, owner_index: u32) -> usize {
        Self::segment_header_start(location, owner_index)
            .unwrap()
            .len()
            + 5 // 5 bytes for len of data
    }
}

impl<'src> LayoutItemInfo for DefinedEntity<'src, ItemType> {
    fn debug_name(&self) -> &str {
        self.name.as_deref().unwrap_or("<unnamed>")
    }
    fn segment_id(&self) -> SegmentId {
        self.entity_type.segment_id
    }
    fn iter_chunks(&self) -> IterBytes<'_> {
        self.body.iter_chunks()
    }
    fn pow2align(&self) -> u8 {
        self.entity_type.align
    }
    fn size(&self) -> usize {
        self.body.len()
    }
    fn padding_symbol(padding: usize, segment_id: SegmentId) -> Self {
        Self {
            entity_type: ItemType::data_chunk(segment_id, 0),
            name: Some(Cow::Borrowed("padding")),
            export_as: ExportNames::new(),
            body: EntityBody::new_empty(smallvec![0; padding]),
        }
    }
    fn segment_header_start(
        location: Option<SpecificLocation>,
        owner_index: u32,
    ) -> Result<SVec<u8, 32>, std::io::Error> {
        data_segment_header(location, owner_index)
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

fn data_segment_header(
    location: Option<SpecificLocation>,
    memory_index: u32,
) -> Result<SVec<u8, 32>, std::io::Error> {
    Ok(match location {
        None => {
            // passive segment
            let mut v = SVec::new();
            v.push(0x01); // flag for passive segment
            v
        }
        Some(location) => {
            let mut result = SVec::new();
            let mut encoder = wasm_emitter::Encoder::new(&mut result, 0);
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
        let vs_id = builder.virtual_spaces.push(VirtualSpaceSpec {
            owner_id: mem_id,
            location: Some(SpecificLocation::ConstantOffset(3)), // some unaligned offset
        });

        let segment_id = builder.segments.push(SegmentSpec {
            vs_id,
            name: "segment1".into(),
            align: 2,
            kind: SegmentKind::Writable,
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

        let sealed = builder.seal_at(0, |temp| temp.to_stable(0));

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
        let data_ref = item1_id.to_stable(0);
        let item = sealed.items_place.get(data_ref).unwrap();
        assert_eq!(item.segment_id, segment_id);
        assert_eq!(item.offsets.va_address, 4); // offset of first item in VA <- 3 byte offset of storage + padding of 1 byte
        assert_eq!(item.offsets.section_offset, 9); // offset of first item in file 9 byte header
        let segment_offset = sealed.segments[segment_id].va_address.unwrap().offset();

        assert_eq!(segment_offset, 4); // offset of segment in virtual memory ( 3 byte offset of storage + padding of 1 byte)
    }

    #[test]
    fn multiple_segments_padding() {
        let mem_id = Temp::from_import(0);

        let mut builder = MemLayoutBuilder::new();
        let vs_id = builder.virtual_spaces.push(VirtualSpaceSpec {
            owner_id: mem_id,
            location: Some(SpecificLocation::ConstantOffset(3)), // some unaligned offset
        });

        let segment1_id = builder.segments.push(SegmentSpec {
            vs_id,
            name: "segment1".into(),
            align: 2,
            kind: SegmentKind::Writable,
        });

        let segment2_id = builder.segments.push(SegmentSpec {
            vs_id,
            name: "segment2".into(),
            align: 4,
            kind: SegmentKind::Writable,
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

        let sealed = builder.seal_at(0, |temp| temp.to_stable(0));

        dbg!(&sealed);

        assert!(
            sealed.segments[segment1_id]
                .va_address
                .unwrap()
                .offset()
                .is_multiple_of(1 << sealed.segments[segment1_id].pow2align)
        ); // check alignment of first segment
        assert!(
            sealed.segments[segment2_id]
                .va_address
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

        let item = sealed.items_place.get(first_data.to_stable(0)).unwrap();
        assert_eq!(item.segment_id, segment1_id);
        assert_eq!(item.offsets.va_address, 4); // offset of first item in VA <- 3 byte offset of storage + padding of 1 byte
        assert_eq!(item.offsets.section_offset, 9); // offset of first item in file 9 byte header

        let item = sealed.items_place.get(second_data.to_stable(0)).unwrap();
        assert_eq!(item.segment_id, segment2_id);
        assert_eq!(item.offsets.va_address, 16); // offset of second item in (allign of segment 2)
        assert_eq!(item.offsets.section_offset, 21); // 1st segment header (9) + 1st segment data (3) + 2nd segment header (9)
    }
}
