use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    fmt::Debug,
    io::IsTerminal,
    mem, usize,
};

use anyhow::Result;

use crate::{
    helpers::RangeExt,
    index::{GappedMap, NonDefault, ReservedValue},
    linkage::file_db::FileRelocs,
    raw::DataSegmentId,
    typed::{
        Module,
        common_index::EntityKind,
        data::{DataLocation, DataSymbolRef, RawDataChunk},
    },
};

mod hexdump;

// Default is reserved value
type Str<'a> = NonDefault<Cow<'a, str>>;

#[derive(Clone, Debug)]
pub struct SegmentLayout<'a> {
    data_parts: Vec<(RawDataChunk<'a>, DataSymbolRef)>,
    segment_name: Str<'a>,
    location: DataLocation,
    pow2align: usize,
}

impl ReservedValue for SegmentLayout<'_> {
    fn reserved_value() -> Self {
        Self {
            pow2align: usize::MAX,
            segment_name: Str::reserved_value(),
            data_parts: Vec::new(),
            location: DataLocation::Passive,
        }
    }
    fn is_reserved_value(&self) -> bool {
        self.data_parts.is_empty()
            && self.pow2align == usize::MAX
            && matches!(self.location, DataLocation::Passive)
    }
}

impl<'src> SegmentLayout<'src> {
    pub fn new_from_module(
        module: &Module<'src>,
        name: Cow<'src, str>,
        pow2alignment: u8,
        data_segment_id: DataSegmentId,
        mem_location: DataLocation,
    ) -> Result<SegmentLayout<'src>> {
        // Original segment info
        let alignment = (2usize).pow(pow2alignment as u32);

        log::debug!("Mem location is {:?}", mem_location);

        let data_parts = module
            .data
            .iter()
            .filter(|(id, _)| module.data[*id].segment_id == data_segment_id)
            .map(|(symbol_index, chunk)| (chunk.clone(), symbol_index))
            .collect::<Vec<_>>();

        Ok(SegmentLayout {
            segment_name: name.into(),
            pow2align: alignment,
            data_parts,
            location: mem_location,
        })
    }

    pub fn debug_layout(
        file_relocs: &FileRelocs,
        module: &Module<'_>,
        module_name: String,
        data_segments: &GappedMap<DataSegmentId, SegmentLayout<'_>>,
        print_data_format: &mut impl std::fmt::Write,
        color: bool, // std::io::stdout().is_terminal()
    ) {
        writeln!(print_data_format, "<Module {module_name}>").unwrap();

        let mut base = 0;
        for (segment_id, segment) in data_segments.iter() {
            for (symbol, symbol_index) in segment.data_parts.iter() {
                writeln!(
                    print_data_format,
                    "[{segment}:{symbol_index}] {name}",
                    segment = module.data_segments[segment_id].name,
                    name = module.get_name(EntityKind::DataSymbol(*symbol_index))
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

    pub fn memory_location(&self) -> DataLocation {
        self.location
    }

    // Keeps only symbols with id is in `indexes`.
    pub fn new_with_whitelist(mut self, indexes: &BTreeSet<DataSymbolRef>) -> Self {
        let mut result = vec![];
        {
            for (item, symbol_index) in mem::take(&mut self.data_parts) {
                let remove = !indexes.contains(&symbol_index);
                if remove {
                    continue;
                }
                result.push((item, symbol_index));
            }
        }

        self.data_parts = result;
        self
    }

    /// Compute data init offset.
    /// Returns (offset_expr, segment_offset)
    fn segment_header(
        &self,
        mem_start: usize,
        mut segment_offset: usize,
        lib_base_global_id: Option<u32>,
    ) -> (Option<wasm_encoder::ConstExpr>, usize) {
        match self.location {
            DataLocation::Passive => (None, 0),
            DataLocation::ActiveOffset(_) => {
                let offset_expr = match lib_base_global_id {
                    None => {
                        segment_offset +=
                            Self::calculate_padding(mem_start + segment_offset, self.pow2align);
                        wasm_encoder::ConstExpr::i32_const(
                            (mem_start + segment_offset).try_into().unwrap(),
                        )
                    }
                    Some(lib_base_global_id) => {
                        // submodules use lib_base_id
                        {
                            segment_offset +=
                                Self::calculate_padding(segment_offset, self.pow2align);
                            wasm_encoder::ConstExpr::global_get(lib_base_global_id)
                                .with_i32_const(segment_offset.try_into().unwrap())
                                .with_i32_add()
                        }
                    }
                };
                (Some(offset_expr), segment_offset)
            }
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

    /// Compute data init offset and alligned segment_offset.
    pub fn to_segment_output(
        &self,
        lib_base_global_id: Option<u32>,
        mem_start: usize,
        segment_offset: usize,
        //TODO: move segment_offset padding outside
    ) -> (usize, DataSegmentOutput) {
        const BYTE_FILLER: u8 = 0;

        let mut data = Vec::new();

        log::debug!("Segment offset before is {}", mem_start + segment_offset);
        let (data_init, segment_offset) =
            self.segment_header(mem_start, segment_offset, lib_base_global_id);

        log::debug!("Segment offset is {}", mem_start + segment_offset);
        let mut globals = BTreeMap::new();

        let segment_in_mem_start = mem_start + segment_offset;
        for (symbol, symbol_index) in self.data_parts.iter() {
            let chunk = symbol.data;
            let field_alignment = 1 << symbol.pow2align;

            let total_offset = data.len() + segment_in_mem_start;

            // add padding to align data
            {
                let padding = Self::calculate_padding(total_offset, field_alignment);
                if padding > 0 {
                    log::debug!(
                        "Add padding before data symbol {}: {padding} bytes",
                        symbol_index
                    );

                    data.resize(data.len() + padding, BYTE_FILLER);
                }
            }
            log::trace!(
                "Data symbol {}: offset: {}, size: {}, aligned: {}",
                symbol_index,
                data.len(),
                chunk.len(),
                field_alignment
            );

            globals.insert(
                *symbol_index,
                DataSymbolOffset {
                    data_mem_offset: data.len(),
                },
            );
            data.extend_from_slice(chunk);
        }

        (
            segment_offset,
            DataSegmentOutput {
                data_init: data_init.expect("Active data segment should have offset"),
                data,
                segment_name: self.segment_name.to_string().into(),
                data_symbols: globals,
                memory_offset: mem_start + segment_offset,
            },
        )
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DataSymbolOffset {
    // Relative to lib_base for submodules
    pub data_mem_offset: usize,
}

/// Representation of calculated data segment for output module.
/// Contain data chunk
#[derive(Debug, Clone)]
pub struct DataSegmentOutput {
    // only for active segments
    data_init: wasm_encoder::ConstExpr,
    memory_offset: usize,
    segment_name: Cow<'static, str>,

    data: Vec<u8>,
    data_symbols: BTreeMap<DataSymbolRef, DataSymbolOffset>,
}
// impl ReservedValue for DataSegmentOutput {
//     fn reserved_value() -> Self {
//         Self {
//             data_init: wasm_encoder::ConstExpr::empty(),
//             memory_offset: usize::MAX,
//             data: Vec::new(),
//             data_symbols: BTreeMap::new(),
//         }
//     }
//     fn is_reserved_value(&self) -> bool {
//         self.memory_offset == usize::MAX && self.data.is_empty() && self.data_symbols.is_empty()
//     }
// }

impl DataSegmentOutput {
    pub fn data_segment<'a>(&'a self, memory_index: u32) -> wasm_encoder::DataSegment<'a, Vec<u8>> {
        wasm_encoder::DataSegment {
            mode: wasm_encoder::DataSegmentMode::Active {
                memory_index,
                offset: &self.data_init,
            },

            data: self.data.clone(),
        }
    }
    pub fn as_raw(&self) -> &[u8] {
        &self.data
    }
    pub fn memory_offset(&self) -> usize {
        self.memory_offset
    }
    pub fn symbols(&self) -> &BTreeMap<DataSymbolRef, DataSymbolOffset> {
        &self.data_symbols
    }
}

#[cfg(test)]
mod tests {
    use cranelift_entity::EntityRef;
    use nom::bytes;

    use super::*;
    use crate::typed::LinkingFile;
    const WASM_BYTES: &[u8] = crate::testfiles::SIMPLE_GRAPH;

    #[test]
    fn test_layouts() {
        assert_layout_same("simpl_graph", crate::testfiles::SIMPLE_GRAPH);
        assert_layout_same("example", crate::testfiles::EXAMPLE_WASM);
        assert_layout_same("lazy_routes", crate::testfiles::LAZY_ROUTES);
    }
    fn assert_layout_same(file_name: &str, bytes: &[u8]) {
        let file = LinkingFile::from_wasm_bytes(bytes).unwrap();
        let mut segments = GappedMap::new();
        for segment in 0..file.wasm_reader.data.data_segments.len() {
            let segment_id = DataSegmentId::new(segment);
            let layout = SegmentLayout::new_from_module(
                &file.module,
                format!("segment_{segment}").into(),
                file.wasm_reader.linking.segments_info[segment].alignment as u8,
                segment_id,
                DataLocation::from_data_kind(&file.wasm_reader.data.data_segments[segment_id].kind)
                    .unwrap(),
            );
            segments.insert(segment_id, layout.unwrap());
        }
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
