use std::{borrow::Cow, fmt::Debug};

use anyhow::Result;
use cranelift_entity::PrimaryMap;
use itertools::Itertools;

use crate::{
    SVec,
    emit::modify::wasm_emitter,
    index::{GappedMap, ReservedValue},
    linkage::file_db::FileRelocs,
    raw::DataSegmentId,
    typed::{
        DefinedDataChunk, Module,
        data::{BASE_ALIGNMENT, DataSymbolRef, RawDataChunk, SegmentPlacement, SpecificLocation},
    },
};

pub mod hexdump;

#[derive(Clone, Debug)]
struct ChunkRepr<'a>(DefinedDataChunk<'a>, Cow<'a, str>, DataSymbolRef);

#[derive(Clone, Debug)]
pub struct SegmentLayout<'a> {
    segment_name: Cow<'a, str>,
    mem_location: Option<SpecificLocation>,
    pow2align: u32,
    data_parts: Vec<ChunkRepr<'a>>,
}
pub type Segments<'a> = PrimaryMap<DataSegmentId, SegmentLayout<'a>>;
pub type DataSymbolsOffsets = GappedMap<DataSymbolRef, DataSymbolOffset>;

impl<'src> SegmentLayout<'src> {
    /// Build segments layout for module.
    /// Returns segments layout and mapping from data symbol to its offset in memory.
    ///
    /// `mem_start` is the offset in memory where the first segment will be placed.
    /// The next segments will be shifted by the size of previous segments with padding for alignment.
    ///
    /// Symbols from passive segments will not be present in the mapping.
    ///
    pub fn build_for_module(module: &Module<'src>) -> Result<(Segments<'src>, DataSymbolsOffsets)> {
        let mem_start = module.mem_spec.mem_start;
        let padding = Self::calculate_padding(mem_start.offset(), 2 << BASE_ALIGNMENT);
        let mem_start = mem_start.add_offset(padding);

        let (mut results, mut mapping) = (PrimaryMap::new(), DataSymbolsOffsets::new());

        let (mut segment_offset, mut mem_offset) = (0, 0);

        debug_assert!(
            module
                .data
                .defined_iter()
                .map(|(_, v)| v.entity_type.segment_id)
                .is_sorted(),
            "Data symbols should be grouped by segment id"
        );

        let chunks = module
            .data
            .defined_iter()
            .chunk_by(|(_, v)| v.entity_type.segment_id);

        let symbols_iter = chunks
            .into_iter()
            .zip(module.mem_spec.data_segments.iter())
            .map(|((grp_sid, grp), (sid, info))| {
                debug_assert_eq!(grp_sid, sid, "Data symbols should be grouped by segment id");
                (sid, info, grp)
            });

        for (segment_id, info, grp) in symbols_iter {
            log::debug!(
                "Segment {}, Mem location is {:?}",
                segment_id,
                info.location
            );
            // Original segment info
            let alignment = (2u32).pow(info.pow2align as u32);

            // Align memory offset of segment to its alignment requirement.
            let padding = Self::calculate_padding(mem_offset, alignment);
            mem_offset += padding;
            let location = Self::calculate_location(info.location, mem_start, mem_offset);

            // and shift segment offset by len of header.
            segment_offset += Self::segment_header(location)?.len() as u32;

            let mut data_parts: Vec<ChunkRepr<'src>> = Vec::new();
            for (symbol_index, symbol) in grp {
                let field_alignment = 1 << symbol.entity_type.pow2align;
                // add padding for alignment
                if let Some((padding_symbol, name)) =
                    Self::padding_symbol(segment_offset, field_alignment, segment_id)
                {
                    let padding = padding_symbol.body.len() as u32;
                    log::trace!(
                        "Add padding placeholder before symbol {}: {padding} bytes",
                        symbol_index
                    );

                    data_parts.push(ChunkRepr(
                        padding_symbol,
                        name,
                        DataSymbolRef::reserved_value(),
                    ));

                    segment_offset += padding;
                    mem_offset += padding;
                }
                let name = module.get_name(symbol_index.into());

                data_parts.push(ChunkRepr(symbol.clone(), name, symbol_index));
                mapping.insert(
                    symbol_index,
                    DataSymbolOffset {
                        addr_of_symbol: mem_offset as usize,
                        data_section_offset: segment_offset as usize,
                    },
                );

                segment_offset += symbol.body.len() as u32;
                mem_offset += symbol.body.len() as u32;
            }

            let layout = SegmentLayout {
                segment_name: info.name.clone(),
                pow2align: alignment,
                data_parts,
                mem_location: location,
            };
            results.push(layout);
        }
        Ok((results, mapping))
    }

    pub fn is_empty(&self) -> bool {
        self.data_parts.is_empty()
    }

    /// Convert segment layout to data segment output, which can be encoded into wasm. (excluding segment header)
    pub fn data_stream(&self) -> impl ExactSizeIterator<Item = u8> {
        let total_size: usize = self.data_parts.iter().map(|chunk| chunk.0.body.len()).sum();

        DataStream {
            iter: self
                .data_parts
                .iter()
                .flat_map(|chunk| chunk.0.body.iter_bytes()),
            total_size,
        }
    }

    pub fn debug_layout(
        file_relocs: &FileRelocs,
        module: &Module<'_>,
        module_name: String,
        data_segments: &PrimaryMap<DataSegmentId, SegmentLayout<'_>>,
        print_data_format: &mut impl std::fmt::Write,
        color: bool, // std::io::stdout().is_terminal()
    ) {
        use hexdump::SymbolDebugExt;
        writeln!(print_data_format, "<Module {module_name}>").unwrap();

        let mut base = 0;
        for (_, segment) in data_segments.iter() {
            for chunk in segment.data_parts.iter() {
                let segment = &segment.segment_name;
                let name = &chunk.1;
                let symbol_index = chunk.2;

                let db = hexdump::SymbolDebug {
                    module,
                    file_relocs,
                    segment,
                    symbol_name: name,
                    symbol_index,
                    body: &chunk.0.body,
                };
                db.debug_symbol_ext(&mut *print_data_format, &mut base, color);
            }
        }
    }

    pub fn memory_location(&self) -> Option<SpecificLocation> {
        self.mem_location
    }
    pub fn memory_index(&self) -> u32 {
        0 // TODO: support multiple memories
    }

    fn calculate_location(
        segment_placement: SegmentPlacement,
        mem_start: SpecificLocation,
        mem_offset: u32,
    ) -> Option<SpecificLocation> {
        match segment_placement {
            SegmentPlacement::Passive => None,
            SegmentPlacement::ContinuesMemory => Some(mem_start.add_offset(mem_offset)),
            SegmentPlacement::Specific(location) => {
                panic!("Segment specify fixed location, which is not supported: {location:?}")
            }
        }
    }

    fn segment_header(location: Option<SpecificLocation>) -> Result<SVec<u8, 32>> {
        Ok(match location {
            None => {
                // passive segment
                let mut v = SVec::new();
                v.push(0x01); // flag for passive segment
                v
            }
            // TODO: support multiple memories?
            Some(location) => {
                let mut result = SVec::new();
                let mut encoder = wasm_emitter::Encoder::new(&mut result, 0);
                encoder.push_byte(0x00)?; // mem index + flag
                let offset = location.to_init_expr();
                encoder.encode_const_expr(&offset)?;
                encoder.push_byte(0x0B)?; // end of instruction
                result
            }
        })
    }

    fn calculate_padding(starting_point: u32, alignment: u32) -> u32 {
        let misalignment = starting_point % alignment;
        if misalignment == 0 {
            0
        } else {
            alignment - misalignment
        }
    }

    fn padding_symbol(
        segment_offset: u32,
        alignment: u32,
        segment_id: DataSegmentId,
    ) -> Option<(DefinedDataChunk<'src>, Cow<'src, str>)> {
        let name = Cow::Borrowed("padding");
        let padding = Self::calculate_padding(segment_offset, alignment);
        if padding > 0 {
            Some((
                RawDataChunk {
                    data: &[0; 64][..padding as usize],
                    pow2align: 0,
                    original_offset: 0,
                    segment_id,
                    name: name.clone(),
                }
                .into(),
                name,
            ))
        } else {
            None
        }
    }
}

#[derive(Copy, Debug, Clone, Eq, PartialEq)]
pub struct DataSymbolOffset {
    /// Offset of symbol in memory.
    pub addr_of_symbol: usize,
    /// Offset of symbol in wasm file relative to data section start.
    pub data_section_offset: usize,
}

impl ReservedValue for DataSymbolOffset {
    fn reserved_value() -> Self {
        Self {
            addr_of_symbol: usize::MAX,
            data_section_offset: usize::MAX,
        }
    }
    fn is_reserved_value(&self) -> bool {
        self.addr_of_symbol == usize::MAX && self.data_section_offset == usize::MAX
    }
}

struct DataStream<I> {
    iter: I,
    total_size: usize,
}
impl<I> Iterator for DataStream<I>
where
    I: Iterator<Item = u8>,
{
    type Item = u8;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next()
    }
}

impl<I> ExactSizeIterator for DataStream<I>
where
    I: Iterator<Item = u8>,
{
    fn len(&self) -> usize {
        self.total_size
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::typed::LoadedFile;

    #[test]
    fn test_layouts() {
        assert_layout_same("simpl_graph", crate::testfiles::SIMPLE_GRAPH);
        assert_layout_same("example", crate::testfiles::EXAMPLE_WASM);
        assert_layout_same("lazy_routes", crate::testfiles::LAZY_ROUTES);
    }
    fn assert_layout_same(file_name: &str, bytes: &[u8]) {
        let file = LoadedFile::from_wasm_bytes(bytes).unwrap();
        let (segments, _) = SegmentLayout::build_for_module(&file.module).unwrap();
        let mut print_data_format = String::new();
        SegmentLayout::debug_layout(
            &file.relocs,
            &file.module,
            String::from("test"),
            &segments,
            &mut print_data_format,
            false,
        );
        insta::assert_snapshot!(file_name, print_data_format);
    }
}
