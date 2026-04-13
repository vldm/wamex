use std::{borrow::Cow, fmt::Debug, io::Write};

use anyhow::Result;
use cranelift_entity::PrimaryMap;

use super::{DataKind, DataSymbolRef, Offsets};
use crate::{
    emit::modify::wasm_emitter::{self, EncodeWithRelocOffset, SectionList},
    index::{GappedMap, WithStart},
    layouts::PartId,
    raw::SegmentId,
    typed::{DefinedDataChunk, ImportedEntity, IterBytes, MemoryRef},
};

/// Either element in table or data chunk in segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedDataItem<'src> {
    /// Body of item.
    pub defined_entity: DefinedDataChunk<'src>,
    ///
    /// Id that was used in builder to refer to this item.
    /// (debug purpose)
    ///
    /// None if technical item (padding)
    pub item_id: Option<DataSymbolRef>,
}

#[derive(Copy, Debug, Clone, Eq, PartialEq)]
pub struct DataItemPlace {
    pub offsets: Offsets,
    pub segment_id: SegmentId,
    pub part_id: PartId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedDataSegment<'src> {
    /// Body of segment, containing defined entities.
    pub parts: PrimaryMap<PartId, SealedDataItem<'src>>,

    /// Name of segment.
    pub name: Cow<'src, str>,

    /// Alignment of segment, represented as power of 2.
    pub pow2align: u8,

    /// Address in virtual memory or table where segment should be placed.
    pub va_address: DataKind<MemoryRef>,

    /// Offset of segment in the file.
    pub file_offset: usize,
}

impl<'src> SealedDataSegment<'src> {
    #[must_use]
    pub fn data_stream(&self) -> DataStream<'_> {
        let total_size: usize = self
            .parts
            .values()
            .map(|chunk| chunk.defined_entity.body.len())
            .sum();

        let iter: FlatIter<'_> = self
            .parts
            .values()
            .flat_map(|chunk| chunk.defined_entity.body.iter_chunks());
        DataStream { iter, total_size }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemLayoutSealed<'src> {
    pub segments: PrimaryMap<SegmentId, SealedDataSegment<'src>>,

    /// Imported items that left after sealing.
    pub(crate) external: WithStart<DataSymbolRef, ImportedEntity<'src, ()>>,
    /// Can have gaps when recover from object file (e.g. overlapping items).
    pub(crate) defined: GappedMap<DataSymbolRef, DataItemPlace>,
}

impl<'src> MemLayoutSealed<'src> {
    #[must_use]
    pub fn item_places(&self) -> &GappedMap<DataSymbolRef, DataItemPlace> {
        &self.defined
    }
    #[must_use]
    pub fn external(&self) -> &WithStart<DataSymbolRef, ImportedEntity<'src, ()>> {
        &self.external
    }
    #[must_use]
    pub fn segments(&self) -> &PrimaryMap<SegmentId, SealedDataSegment<'src>> {
        &self.segments
    }
}
impl<'src> MemLayoutSealed<'src> {
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
// Encode helpers
//

fn data_segment_adapter<W>(
    encoder: &mut wasm_emitter::Encoder<W>,
    location: DataKind<MemoryRef>,
    data_stream: DataStream,
) -> Result<(), std::io::Error>
where
    W: Write,
{
    // where segment:
    // - header (mode/offset)
    // - len of data
    // - data bytes

    let header = super::data_segment_header_start(location)?;
    encoder.push_bytes(&header)?;
    // len + data
    data_stream.encode(encoder)?;
    Ok(())
}

type FlatIter<'a> = std::iter::FlatMap<
    std::slice::Iter<'a, SealedDataItem<'a>>,
    IterBytes<'a>,
    for<'b> fn(&'b SealedDataItem<'a>) -> IterBytes<'b>,
>;

#[derive(Debug)]
pub struct DataStream<'a> {
    pub(crate) iter: FlatIter<'a>,
    pub(crate) total_size: usize,
}

impl<'a> Clone for DataStream<'a> {
    fn clone(&self) -> Self {
        Self {
            iter: self.iter.clone(),
            total_size: self.total_size,
        }
    }
}

impl<'a> DataStream<'a> {
    #[must_use]
    pub fn bytes_len(&self) -> usize {
        self.total_size
    }
}

impl<'a> Iterator for DataStream<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next()
    }
}

impl<'a> wasm_emitter::EncodeWithRelocOffset for DataStream<'a> {
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
