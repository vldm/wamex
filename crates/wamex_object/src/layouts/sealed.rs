use std::{borrow::Cow, io::Write};

use cranelift_entity::{EntityRef, PrimaryMap, packed_option::ReservedValue};

use super::{ItemType, Offsets, SegmentId};
use crate::{
    emit::modify::wasm_emitter::{self, EncodeWithRelocOffset, SectionList},
    index::GappedMap,
    layouts::LayoutItemInfo,
    linkage::file_db::FileRelocs,
    typed::{
        ImportedEntity, IterBytes, Module,
        data::{DataSymbolRef, SpecificLocation},
    },
};

/// Either element in table or data chunk in segment.
#[derive(Debug)]
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

#[derive(Debug)]
pub struct SealedSegment<'src, OwnerId, ItemId: EntityRef, DefinedEntity> {
    /// Name of segment.
    pub name: Cow<'src, str>,
    /// Body of segment, containing defined entities.
    pub parts: Vec<SealedItem<DefinedEntity, ItemId>>,

    /// Alignment of segment, represented as power of 2.
    pub pow2align: u8,

    pub owner: OwnerId,
    /// Address in virtual memory or table where segment should be placed.
    pub va_address: Option<SpecificLocation>,
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

#[derive(Debug)]
pub struct SealedLayout<'src, OwnerId, ItemId: EntityRef, DefinedEntity> {
    pub segments: PrimaryMap<SegmentId, SealedSegment<'src, OwnerId, ItemId, DefinedEntity>>,

    /// Imported items that left after sealing.
    pub imports: PrimaryMap<ItemId, ImportedEntity<'src, ItemType>>,
    pub items_place: GappedMap<ItemId, ItemOffsets>,
}

impl<'src, OwnerId, ItemId: EntityRef, DefinedEntity>
    SealedLayout<'src, OwnerId, ItemId, DefinedEntity>
where
    DefinedEntity: LayoutItemInfo,
    OwnerId: EntityRef,
{
    pub fn encode<W>(&self, mut writer: SectionList<W>) -> Result<(), std::io::Error>
    where
        W: std::io::Write,
    {
        writer.item_from_encoder(|e| {
            for segment in self.segments.values() {
                data_segment_adapter(
                    e,
                    segment.va_address,
                    segment.owner.index() as u32,
                    segment.data_stream(),
                )?;
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
}

//
// helpers
//

pub fn data_segment_adapter<W, DefinedEntity, ItemId>(
    encoder: &mut wasm_emitter::Encoder<W>,
    location: Option<SpecificLocation>,
    owner_index: u32,
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

    let encoded = <DefinedEntity as LayoutItemInfo>::segment_header_start(location, owner_index)?;
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
