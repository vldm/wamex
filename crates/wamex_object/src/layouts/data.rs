use std::borrow::Cow;

use anyhow::{Result, bail, ensure};
use cranelift_bitset::CompoundBitSet;
use cranelift_entity::{EntityRef, PrimaryMap, packed_option::ReservedValue};
use wasmparser::Table;

use super::MemLayoutSealed;
use crate::{
    index::{GappedMap, Temp, WithStart},
    layouts::{
        DefinedDataChunk, ItemType, LayoutItemInfo, MemLayoutBuilder, Offsets, PartId,
        SegmentFlags, SegmentSpec, SpecificLocation, VirtualSpaceKind, VsRecover,
        guess_data_alignment,
        sealed::{ItemPlace, SealedItem, SealedSegment},
    },
    linkage::{
        LinkageInfo,
        file_db::{FileRelocs, FileSymbolDb},
    },
    raw::SegmentId,
    typed::{
        DefinedEntity, EntityBody, EntityBodyCopy, ExportNames, GlobalRef, ImportOrDefined,
        ImportedDataChunk, MemoryRef, Module, TableRef,
    },
};

impl_entity_index! {
    #[display="data"]
    pub struct DataSymbolRef;
}

// Align base of memory to 16 bytes, if it wasn't already aligned.
pub const BASE_ALIGNMENT: u8 = u8::trailing_zeros(16) as u8;

pub const DEFAULT_HEAP_START: SpecificLocation = SpecificLocation::ConstantOffset(0x100000);

//
// Impl for sealed
//
impl<'src> MemLayoutSealed<'src> {
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

    pub fn len(&self) -> usize {
        self.defined_len() + self.external.len()
    }
    pub fn defined_len(&self) -> usize {
        self.defined
            // get last defined in case of gapps (symbols that was pushed, but later was merged into another)
            .last_key()
            .map(|r| r.index() + 1)
            .unwrap_or_default()
    }

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
                            va_address: va_address.location().map_or(0, |v| v.offset() as usize), // TODO
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
            let location = match vs_location {
                VirtualSpaceKind::Active { owner_id, location } => VirtualSpaceKind::Active {
                    owner_id: import_mem(owner_id),
                    location,
                },
                VirtualSpaceKind::Passive => VirtualSpaceKind::Passive,
                VirtualSpaceKind::Declared => VirtualSpaceKind::Declared,
            };
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
    pub fn push_import(&mut self, import: ImportedDataChunk<'src>) -> Temp<DataSymbolRef> {
        self.items.push_import(import)
    }
    pub fn push_entity(
        &mut self,
        entity: ImportOrDefined<ImportedDataChunk<'src>, DefinedDataChunk<'src>>,
    ) -> Temp<DataSymbolRef> {
        self.items.push_entity(entity)
    }
    pub fn dry_push_entity(
        &self,
        entity: &ImportOrDefined<ImportedDataChunk<'src>, DefinedDataChunk<'src>>,
    ) -> Temp<DataSymbolRef> {
        self.items.dry_push_entity(entity)
    }
    ///
    /// Return main active virtual space.
    ///
    pub fn main_vs(&self) -> Option<VirtualSpaceKind<Temp<MemoryRef>>> {
        self.virtual_spaces
            .values()
            .copied()
            .find(VirtualSpaceKind::is_active)
    }
}

pub type DataSymbolsOffsets = GappedMap<DataSymbolRef, ItemPlace>;

fn element_kind_to_location(element_kind: &wasmparser::ElementKind) -> VirtualSpaceKind<TableRef> {
    match element_kind {
        wasmparser::ElementKind::Passive => VirtualSpaceKind::Passive,
        wasmparser::ElementKind::Declared => VirtualSpaceKind::Declared,
        wasmparser::ElementKind::Active {
            table_index,
            offset_expr,
        } => VirtualSpaceKind::Active {
            owner_id: TableRef::from_u32(table_index.unwrap_or_default()),
            location: SpecificLocation::try_from_const_expr(offset_expr)
                .expect("Only const offset supported for active data segments"),
        },
    }
}

fn data_kind_to_location(data_kind: &wasmparser::DataKind) -> VirtualSpaceKind<MemoryRef> {
    match data_kind {
        wasmparser::DataKind::Passive => VirtualSpaceKind::Passive,
        wasmparser::DataKind::Active {
            memory_index,
            offset_expr,
        } => {
            let location = SpecificLocation::try_from_const_expr(offset_expr)
                .expect("Only const offset supported for active data segments");
            VirtualSpaceKind::Active {
                owner_id: MemoryRef::from_u32(*memory_index),
                location,
            }
        }
    }
}

#[cfg(test)]
mod tests {

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
