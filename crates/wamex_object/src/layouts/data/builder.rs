use std::{borrow::Cow, fmt::Debug};

use cranelift_entity::PrimaryMap;
use itertools::Itertools;

use super::*;
use crate::{
    index::{GappedMap, Temp},
    layouts::VirtualSpaceId,
    raw::SegmentId,
    typed::{BuilderState, EntityCollection, ImportedEntity},
};

//
// Builder api
//

/// Type of segment.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SegmentFlags {
    // No modifications of data expected after initialization (.rodata / static)
    Readonly,
    // Arbitrary data that can be modified at runtime (.data / static mut)
    Writable,
    // Zero initialized (.bss)
    ZeroInit,
    // TLS segment, which should be copied to TLS memory at startup
    Tls,
}

impl SegmentFlags {
    pub fn from_name(name: &str) -> Self {
        if name.contains(".bss") {
            Self::ZeroInit
        } else if name.contains(".rodata") {
            Self::Readonly
        } else if name.contains(".tls") {
            Self::Tls
        } else {
            Self::Writable
        }
    }
}
///
/// Information about segment, either data or element.
///
#[derive(Clone, Debug, PartialEq, Eq, Hash)]

pub struct DataSegmentSpec<'src> {
    /// Reference to virtual space in which this segment is located.
    pub vs_id: VirtualSpaceId,
    /// Name of segment,
    pub name: Cow<'src, str>,
    /// Alignment of segment, represented as power of 2.
    /// Only valid for data segments.
    pub align: u8,
    /// Segment characteristics used for linker.
    pub segment_flags: SegmentFlags,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub enum DataKind<OwnerId> {
    Active {
        /// Reference to owner memory.
        memory_ref: OwnerId,
        ///
        /// Information about segment placement in owner unit.
        /// The segment placement is an virtual address in owner unit.
        ///
        /// Can be:
        /// - GotBased - means that segments will have offsets relative to value of global reference.
        /// - Constant - means that segments will have constant offsets.
        location: SegmentPlacement,
    },
    Passive,
}
impl<OwnerId> DataKind<OwnerId> {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Active { .. })
    }
    pub fn location(&self) -> Option<SegmentPlacement> {
        match self {
            Self::Active { location, .. } => Some(*location),
            _ => None,
        }
    }
    pub fn memory(&self) -> Option<OwnerId>
    where
        OwnerId: Copy,
    {
        match self {
            Self::Active { memory_ref, .. } => Some(*memory_ref),
            _ => None,
        }
    }
}

/// A builder for layout of data or element segments, that can be used to construct `BackedLayout`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemLayoutBuilder<'src> {
    /// Virtual spaces allows to group different segments within one address space.
    pub virtual_spaces: PrimaryMap<VirtualSpaceId, DataKind<Temp<MemoryRef>>>,
    pub segments: PrimaryMap<SegmentId, DataSegmentSpec<'src>>,
    // TODO: Can we add any info for ImportedEntity?
    pub items: EntityCollection<
        DataSymbolRef,
        ImportedEntity<'src, ()>,
        DefinedDataChunk<'src>,
        BuilderState,
    >,
}

impl<'src> MemLayoutBuilder<'src> {
    pub fn new() -> Self {
        Self {
            virtual_spaces: PrimaryMap::new(),
            segments: PrimaryMap::new(),
            items: EntityCollection::empty(),
        }
    }
    ///
    /// Assign each item to a concrete position within its segment and owner unit.
    ///
    /// num_owner_imports is used to convert Temp<MemoryRef> to actual MemoryRef.
    pub fn seal_at(
        self,
        section_offset: usize,
        to_stable: impl Fn(Temp<MemoryRef>) -> MemoryRef,
    ) -> MemLayoutSealed<'src> {
        self.seal_at_unchecked(section_offset, false, to_stable)
    }
    /// Inner method that allow sealing without sorting items.
    /// The caller guarantee that defined items are ordered by segment id.
    ///
    /// If this ivariant is not met, the behaviour is udefined:
    /// - may cause panic,
    /// - may skip some items.
    ///
    pub fn seal_at_unchecked(
        self,
        mut section_offset: usize,
        skip_sort: bool,
        to_stable: impl Fn(Temp<MemoryRef>) -> MemoryRef,
    ) -> MemLayoutSealed<'src> {
        let items = self.items.into_finished();
        let (imports, mut defined, external) = items.into_parts();

        if !imports.is_empty() {
            for import in imports.iter() {
                log::error!("Import {}: {:?}", import.0, import.1);
            }

            // Imports have ids < defined and should apear in output entity
            panic!("Imports in data entries is not allowed, use external entries instead");
        }

        if !skip_sort {
            defined
                .as_mut_slice()
                .sort_unstable_by_key(|v| v.entity_type.segment_id);
        } else {
            debug_assert!(
                defined
                    .as_mut_slice()
                    .iter()
                    .map(|v| v.entity_type.segment_id)
                    .is_sorted(),
                "Defined items should be ordered by segment id when skip_sort is true"
            );
        }

        let mut mapping = GappedMap::new();
        let mut segments: PrimaryMap<SegmentId, SealedDataSegment> = PrimaryMap::new();

        let chunks = defined
            .into_iter()
            .chunk_by(|(_, item)| item.entity_type.segment_id);
        let mut chunks_iter = chunks.into_iter().peekable();

        let mut vs_state = self
            .virtual_spaces
            .into_iter()
            .map(|(_, spec)| VsState::new(spec, &to_stable))
            .collect::<PrimaryMap<_, _>>();

        for (segment_id, segment) in self.segments {
            let vs_state = &mut vs_state[segment.vs_id];

            // Original segment info
            let alignment = (2usize).pow(segment.align as u32);
            let segment_start = section_offset;

            // Align memory offset of segment to its alignment requirement.
            let padding = calculate_padding(vs_state.offset, alignment);
            vs_state.offset += padding;

            let mut mem_offset = vs_state.offset;

            // and shift segment offset by len of header.
            section_offset += super::data_segment_header_len(vs_state.base_spec);

            let grp = match chunks_iter.peek() {
                Some((sid, ..)) if segment_id == *sid => Some(chunks_iter.next().unwrap().1),
                Some((sid, ..)) if segment_id > *sid => {
                    panic!(
                        "Found items for segment {sid} but no spec for it found. Where to place these items?"
                    );
                }
                _ => None,
            };

            let mut parts = PrimaryMap::new();
            for (symbol_index, symbol) in grp.into_iter().flatten() {
                let field_alignment = 1 << symbol.entity_type.alignment;

                // add padding for alignment
                if let Some((padding_symbol, _)) =
                    Self::try_padding_symbol(mem_offset, field_alignment, segment_id)
                {
                    let padding = padding_symbol.body.len();
                    log::trace!(
                        "Add padding placeholder before symbol {}: {padding} bytes",
                        symbol_index
                    );

                    parts.push(SealedDataItem {
                        defined_entity: padding_symbol,
                        item_id: None,
                    });

                    section_offset += padding;
                    mem_offset += padding;
                }
                let symbol_len = symbol.body.len();
                let part_id = parts.push(SealedDataItem {
                    defined_entity: symbol,
                    item_id: Some(symbol_index),
                });
                mapping.insert(
                    symbol_index,
                    DataItemPlace {
                        offsets: Offsets {
                            section_offset,
                            va_address: mem_offset + vs_state.va_space_start(),
                        },
                        part_id,
                        segment_id,
                    },
                );

                section_offset += symbol_len;
                mem_offset += symbol_len;
            }

            segments.push(SealedDataSegment {
                name: segment.name,
                parts,
                pow2align: segment.align,
                va_address: vs_state.spec(),
                file_offset: segment_start,
            });
            vs_state.offset = mem_offset;
        }
        assert!(
            chunks_iter.next().is_none(),
            "Data symbols without segments?"
        );

        MemLayoutSealed {
            segments,
            external,
            defined: mapping,
        }
    }

    fn try_padding_symbol(
        segment_offset: usize,
        alignment: usize,
        segment_id: SegmentId,
    ) -> Option<(DefinedDataChunk<'src>, Cow<'src, str>)> {
        let name = Cow::Borrowed("padding");
        let padding = calculate_padding(segment_offset, alignment);
        if padding > 0 {
            Some((DefinedDataChunk::padding_symbol(padding, segment_id), name))
        } else {
            None
        }
    }

    /// Crate virtual default active virtual spac, with one segment (if no vs created).
    /// Return id of this segment
    pub fn try_create_base_vs(
        &mut self,
        owner: Temp<MemoryRef>,
        location: SegmentPlacement,
    ) -> VirtualSpaceId {
        if let Some((vs, _)) = self.virtual_spaces.iter().find(|(_, d)| d.is_active()) {
            vs
        } else {
            self.virtual_spaces.push(DataKind::Active {
                memory_ref: owner,
                location,
            })
        }
    }

    pub fn try_create_segment(&mut self, vs_id: VirtualSpaceId) -> SegmentId {
        self.segments.push(DataSegmentSpec {
            vs_id,
            name: ".rodata".into(),
            align: 3,
            segment_flags: SegmentFlags::Readonly,
        })
    }
}

impl<'src> Default for MemLayoutBuilder<'src> {
    fn default() -> Self {
        Self::new()
    }
}

/// Intermediate representation of `VirtualSpaceLocation`.
/// That allow storing offset of Passive/Declared segments (this is needed for padding + relocs).
struct VsState {
    // current size of virtual space.
    // Used as separate field instead of modifying spec.location because of passive segments.
    offset: usize,
    // Base spec with offset set to 0
    base_spec: DataKind<MemoryRef>,
}
impl VsState {
    fn new(
        tmp_spec: DataKind<Temp<MemoryRef>>,
        to_stable: impl Fn(Temp<MemoryRef>) -> MemoryRef,
    ) -> Self {
        let mut offset = 0;
        let spec = match tmp_spec {
            DataKind::Active {
                memory_ref,
                location,
            } => {
                let memory_ref = to_stable(memory_ref);
                let loc = location;
                offset = loc.offset() as usize;
                DataKind::Active {
                    memory_ref,
                    location: SegmentPlacement::with_zero_offset(&loc),
                }
            }
            // map to other generic
            DataKind::Passive => DataKind::Passive,
        };
        VsState {
            offset,
            base_spec: spec,
        }
    }
    /// Recover spec from offset and base part.
    fn spec(&self) -> DataKind<MemoryRef> {
        match self.base_spec {
            DataKind::Active {
                memory_ref,
                location,
            } => DataKind::Active {
                memory_ref,
                location: location.add_offset(self.offset as u32),
            },
            other => other,
        }
    }
    fn va_space_start(&self) -> usize {
        match &self.base_spec {
            DataKind::Active { location, .. } => location.offset() as usize,
            _ => 0,
        }
    }
}
