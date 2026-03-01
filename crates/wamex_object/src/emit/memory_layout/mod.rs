use std::{borrow::Cow, fmt::Debug};

use anyhow::Result;
use cranelift_entity::PrimaryMap;
use itertools::Itertools;

use crate::{
    SVec,
    emit::modify::wasm_emitter,
    helpers::RangeExt,
    index::{GappedMap, NonDefault, ReservedValue},
    linkage::file_db::FileRelocs,
    raw::DataSegmentId,
    typed::{
        Module,
        data::{DataSymbolRef, RawDataChunk, SegmentPlacement, SpecificLocation},
    },
};

mod hexdump;

// Default is reserved value
type Str<'a> = NonDefault<Cow<'a, str>>;

#[derive(Clone, Debug)]
pub struct SegmentLayout<'a> {
    segment_name: Str<'a>,
    mem_location: Option<SpecificLocation>,
    pow2align: u32,
    data_parts: Vec<(RawDataChunk<'a>, DataSymbolRef)>,
}

impl ReservedValue for SegmentLayout<'_> {
    fn reserved_value() -> Self {
        Self {
            pow2align: u32::MAX,
            segment_name: Str::reserved_value(),
            data_parts: Vec::new(),
            mem_location: None,
        }
    }
    fn is_reserved_value(&self) -> bool {
        self.data_parts.is_empty() && self.pow2align == u32::MAX && self.mem_location.is_none()
    }
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
        let (mut results, mut mapping) = (PrimaryMap::new(), DataSymbolsOffsets::new());

        let (mut segment_offset, mut mem_offset) = (0, 0);

        debug_assert!(
            module.data.values().map(|v| v.segment_id).is_sorted(),
            "Data symbols should be grouped by segment id"
        );

        let chunks = module.data.iter().chunk_by(|(_, v)| v.segment_id);
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
            let location =
                Self::calculate_location(info.location, module.mem_spec.mem_start, mem_offset);

            segment_offset += Self::segment_header(location)?.len() as u32;

            let mut data_parts = Vec::new();
            for (symbol_index, symbol) in grp {
                let field_alignment = 1 << symbol.pow2align;
                // add padding for alignment
                if let Some(padding_symbol) =
                    Self::padding_symbol(segment_offset, field_alignment, segment_id)
                {
                    let padding = padding_symbol.data.len() as u32;
                    log::trace!(
                        "Add padding placeholder before symbol {}: {padding} bytes",
                        symbol_index
                    );

                    data_parts.push((padding_symbol, DataSymbolRef::reserved_value()));

                    segment_offset += padding;
                    mem_offset += padding;
                }

                data_parts.push((symbol.clone(), symbol_index));
                mapping.insert(
                    symbol_index,
                    DataSymbolOffset {
                        addr_of_symbol: mem_offset as usize,
                        data_section_offset: segment_offset as usize,
                    },
                );

                segment_offset += symbol.data.len() as u32;
                mem_offset += symbol.data.len() as u32;
            }

            let layout = SegmentLayout {
                segment_name: info.name.clone().into(),
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
        let total_size: usize = self
            .data_parts
            .iter()
            .map(|(symbol, _)| symbol.data.len())
            .sum();

        DataStream {
            iter: self
                .data_parts
                .iter()
                .flat_map(|(symbol, _)| symbol.data.iter().copied()),
            total_size,
        }
    }

    // Keeps only symbols with id is in `indexes`.
    // pub fn new_with_whitelist(mut self, indexes: &BTreeSet<DataSymbolRef>) -> Self {
    //     let mut result = vec![];
    //     {
    //         for (item, symbol_index) in mem::take(&mut self.data_parts) {
    //             let remove = !indexes.contains(&symbol_index);
    //             if remove {
    //                 continue;
    //             }
    //             result.push((item, symbol_index));
    //         }
    //     }

    //     self.data_parts = result;
    //     self
    // }

    pub fn debug_layout(
        file_relocs: &FileRelocs,
        module: &Module<'_>,
        module_name: String,
        data_segments: &PrimaryMap<DataSegmentId, SegmentLayout<'_>>,
        print_data_format: &mut impl std::fmt::Write,
        color: bool, // std::io::stdout().is_terminal()
    ) {
        writeln!(print_data_format, "<Module {module_name}>").unwrap();

        let mut base = 0;
        for (_, segment) in data_segments.iter() {
            for (symbol, symbol_index) in segment.data_parts.iter() {
                if symbol_index.is_reserved_value() {
                    writeln!(
                        print_data_format,
                        "[{segment}:{symbol_index}] <padding> (size: {})",
                        symbol.data.len(),
                        segment = segment.segment_name,
                    )
                    .unwrap();
                    base += symbol.data.len();
                    continue;
                }
                writeln!(
                    print_data_format,
                    "[{segment}:{symbol_index}] {name}",
                    segment = segment.segment_name,
                    name = symbol.name
                )
                .unwrap();
                let chunk = symbol.data;

                let input_symbol = file_relocs
                    .get_data_relocs(*symbol_index)
                    .unwrap_or_default();
                let refs = input_symbol
                    .iter()
                    .map(|reloc| {
                        let id = reloc.symbol_id.combine(reloc.symbol_type);
                        let name = module.get_name(id);
                        hexdump::Ref {
                            range: reloc.relocation_range().shift_left(symbol.original_offset),
                            name,
                        }
                    })
                    .collect();
                let part = hexdump::DataPart { bytes: chunk, refs };
                hexdump::render_part(&mut *print_data_format, base, &part, color);
                // TODO: add padding
                base += chunk.len();
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
    ) -> Option<RawDataChunk<'src>> {
        let padding = Self::calculate_padding(segment_offset, alignment);
        if padding > 0 {
            Some(RawDataChunk {
                data: &[0; 64][..padding as usize],
                pow2align: 0,
                original_offset: 0,
                segment_id,
                name: Cow::Borrowed("padding"),
            })
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
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
