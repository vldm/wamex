// TODO: Element module currently not support relocation, so don't provide a way to calculate offsets of elements
// neither in data segment, neither in table (by element id).
use std::{borrow::Cow, fmt::Debug, io::Write};

use anyhow::Result;
use cranelift_entity::{PrimaryMap, packed_option::ReservedValue};
use wasmparser::ElementItems;

use crate::{
    emit::modify::wasm_emitter::{self, SectionList},
    index::{GappedMap, Temp, WithStart},
    layouts::{DataSegmentSpec, PartId, SegmentPlacement, VirtualSpaceId},
    raw::{self, SegmentId},
    typed::{FunctionRef, GlobalRef, TableRef},
};

impl_entity_index! {
    #[display ="item"]
    pub struct ElementItemId;
}

// Builder
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElementSegmentSpec<'src> {
    /// Reference to virtual space in which this segment is located.
    pub vs_id: VirtualSpaceId,
    /// Name of segment,
    pub name: Cow<'src, str>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElementInTable<T> {
    pub item: T,
    pub segment_id: SegmentId,
}

/// A builder for layout of data or element segments, that can be used to construct `BackedLayout`.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct ElementLayoutBuilder<'src, T> {
    pub virtual_spaces: PrimaryMap<VirtualSpaceId, ElementKind<Temp<TableRef>, Temp<GlobalRef>>>,

    pub segments: PrimaryMap<SegmentId, ElementSegmentSpec<'src>>,
    // TODO: Can we add any info for ImportedEntity?
    pub items: Vec<ElementInTable<T>>,
}

impl<'src, T> ElementLayoutBuilder<'src, T> {
    pub fn new() -> Self {
        Self {
            virtual_spaces: PrimaryMap::new(),
            segments: PrimaryMap::new(),
            items: Vec::new(),
        }
    }
    pub fn map_elements<U>(self, map_item: impl Fn(T) -> U) -> ElementLayoutBuilder<'src, U> {
        let items = self
            .items
            .into_iter()
            .map(|item| ElementInTable {
                item: map_item(item.item),
                segment_id: item.segment_id,
            })
            .collect();
        ElementLayoutBuilder {
            virtual_spaces: self.virtual_spaces,
            segments: self.segments,
            items,
        }
    }
    pub fn seal(
        self,
        map_table: impl Fn(Temp<TableRef>) -> TableRef,
        map_global: impl Fn(Temp<GlobalRef>) -> GlobalRef,
    ) -> ElementLayoutSealed<'src, T>
    where
        T: ElementType<'src> + ReservedValue + Clone,
    {
        let mut this = ElementLayoutSealed {
            segments: PrimaryMap::new(),
        };
        // push segments
        let vs_state = self
            .virtual_spaces
            .into_iter()
            .map(|(_, spec)| VsState::new(spec, &map_table, &map_global))
            .collect::<PrimaryMap<VirtualSpaceId, _>>();

        for (_id, spec) in self.segments.into_iter() {
            let vs_state = &vs_state[spec.vs_id];
            let segment_spec = vs_state.spec();
            let sealed_segment = SealedElementSegment {
                parts: PrimaryMap::new(),
                kind: segment_spec,
                name: spec.name,
            };
            this.segments.push(sealed_segment);
        }

        for item in self.items.into_iter() {
            let segment = this
                .segments
                .get_mut(item.segment_id)
                .expect("Invalid segment id");
            segment.parts.push(item.item);
        }

        this
    }
}

/// Intermediate representation of `VirtualSpaceLocation`.
/// That allow storing offset of Passive/Declared segments (this is needed for padding + relocs).
struct VsState {
    // current size of virtual space.
    // Used as separate field instead of modifying spec.location because of passive segments.
    offset: usize,
    // Base spec with offset set to 0
    base_spec: ElementKind<TableRef, GlobalRef>,
}
impl VsState {
    fn new(
        tmp_spec: ElementKind<Temp<TableRef>, Temp<GlobalRef>>,
        map_table: impl Fn(Temp<TableRef>) -> TableRef,
        map_global: impl Fn(Temp<GlobalRef>) -> GlobalRef,
    ) -> Self {
        let mut offset = 0;
        let spec = match tmp_spec {
            ElementKind::Active {
                table_ref,
                location,
            } => {
                let loc = location;
                offset = loc.offset() as usize;
                let kind = ElementKind::Active {
                    table_ref,
                    location: SegmentPlacement::with_zero_offset(&loc),
                };
                kind.map_ids(map_table, map_global)
            }
            // map to other generic
            ElementKind::Passive => ElementKind::Passive,
            ElementKind::Declared => ElementKind::Declared,
        };
        VsState {
            offset,
            base_spec: spec,
        }
    }
    /// Recover spec from offset and base part.
    fn spec(&self) -> ElementKind<TableRef, GlobalRef> {
        match self.base_spec {
            ElementKind::Active {
                table_ref,
                location,
            } => ElementKind::Active {
                table_ref,
                location: location.add_offset(self.offset as u32),
            },
            other => other,
        }
    }
    fn va_space_start(&self) -> usize {
        match &self.base_spec {
            ElementKind::Active { location, .. } => location.offset() as usize,
            _ => 0,
        }
    }
}

// Sealed

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedElementSegment<'src, T> {
    /// Body of segment, containing defined entities.
    pub parts: PrimaryMap<PartId, T>,

    pub kind: ElementKind<TableRef, GlobalRef>,
    /// Name of segment.
    pub name: Cow<'src, str>,
}

#[derive(Copy, Debug, Clone, Eq, PartialEq)]
pub struct ElementItemPlace {
    pub segment_id: SegmentId,
    pub part_id: PartId,
}
impl ReservedValue for ElementItemPlace {
    fn reserved_value() -> Self {
        Self {
            segment_id: SegmentId::reserved_value(),
            part_id: PartId::reserved_value(),
        }
    }
    fn is_reserved_value(&self) -> bool {
        self.segment_id.is_reserved_value() && self.part_id.is_reserved_value()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElementLayoutSealed<'src, T> {
    pub segments: PrimaryMap<SegmentId, SealedElementSegment<'src, T>>,
}

pub trait ElementType<'a>: Debug {
    fn decode(items: ElementItems<'a>) -> impl Iterator<Item = Result<Self>> + 'a
    where
        Self: Sized;

    fn encode_segment_start<W>(
        kind: ElementKind<TableRef, GlobalRef>,
        encoder: &mut wasm_emitter::Encoder<W>,
    ) -> std::result::Result<(), std::io::Error>
    where
        Self: Sized,
        W: Write;

    fn encode_item<W>(
        item: &Self,
        encoder: &mut wasm_emitter::Encoder<W>,
    ) -> std::result::Result<(), std::io::Error>
    where
        Self: Sized,
        W: Write;
}

impl<'a> ElementType<'a> for FunctionRef {
    fn decode(items: ElementItems<'a>) -> impl Iterator<Item = Result<Self>> + 'a
    where
        Self: Sized,
    {
        // Avoid boxing
        let decoded_items;
        let wrong_type;

        match items {
            ElementItems::Functions(func_indices) => {
                decoded_items = Some(func_indices.into_iter_with_offsets().map(|elem| {
                    let (_offset, func_id) = elem?;
                    Ok(FunctionRef::from_u32(func_id))
                }));
                wrong_type = None;
            }
            _ => {
                decoded_items = None;
                wrong_type = Some(std::iter::once(Err(anyhow::anyhow!(
                    "Invalid element items type for function indices"
                ))));
            }
        };
        std::iter::chain(
            decoded_items.into_iter().flatten(),
            wrong_type.into_iter().flatten(),
        )
    }

    fn encode_segment_start<W>(
        kind: ElementKind<TableRef, GlobalRef>,
        encoder: &mut wasm_emitter::Encoder<W>,
    ) -> std::result::Result<(), std::io::Error>
    where
        Self: Sized,
        W: Write,
    {
        const FUNCREF_ELEMKIND: u8 = 0x00;

        match kind {
            ElementKind::Active {
                table_ref,
                location,
            } => {
                if table_ref.as_u32() == 0 {
                    encoder.push_byte(0x00)?;
                } else {
                    encoder.push_byte(0x02)?;
                    encoder.encode_leb_5byte(table_ref.as_u32())?;
                }
                encoder.encode_const_expr(&location.to_init_expr())?;
                if table_ref.as_u32() != 0 {
                    encoder.push_byte(FUNCREF_ELEMKIND)?;
                }
            }
            ElementKind::Passive => {
                encoder.push_byte(0x01)?;
                encoder.push_byte(FUNCREF_ELEMKIND)?;
            }
            ElementKind::Declared => {
                encoder.push_byte(0x03)?;
                encoder.push_byte(FUNCREF_ELEMKIND)?;
            }
        }

        Ok(())
    }

    fn encode_item<W>(
        item: &Self,
        encoder: &mut wasm_emitter::Encoder<W>,
    ) -> std::result::Result<(), std::io::Error>
    where
        Self: Sized,
        W: Write,
    {
        encoder.encode_leb_5byte(item.as_u32())?;
        Ok(())
    }
}

impl<'src, T: ElementType<'src> + ReservedValue + Clone> ElementLayoutSealed<'src, T> {
    // Creates ElementLayoutSealed from raw module reader
    // search all element segments for needed table_id,
    // if default_table is set, then segments with no table_index (Wasm MVP spec) are also considered.
    // Returns error if element segment uses unsupported offset expression or item type.
    pub fn typed_from_reader(module: &raw::ObjectReader<'src>) -> Result<Self> {
        let mut this = ElementLayoutSealed {
            segments: PrimaryMap::new(),
        };

        for (id, element) in module.elements.iter() {
            let kind = element_kind_to_location(&element.kind);

            let parts = T::decode(element.items.clone()).collect::<Result<PrimaryMap<_, _>>>()?;

            let sealed_segment = SealedElementSegment {
                parts,
                kind,
                name: id.to_string().into(),
            };
            this.segments.push(sealed_segment);
        }
        Ok(this)
    }

    pub fn encode<W>(&self, writer: &mut SectionList<W>) -> std::result::Result<(), std::io::Error>
    where
        W: Write,
    {
        for segment in self.segments.values() {
            writer.item_from_encoder(|encoder| {
                T::encode_segment_start(segment.kind, encoder)?;
                encoder.encode_leb_5byte(segment.parts.len() as u32)?;
                for item in segment.parts.values() {
                    T::encode_item(item, encoder)?;
                }
                Ok(())
            })?;
        }

        Ok(())
    }
}
impl<'src> ElementLayoutSealed<'src, FunctionRef> {
    pub fn items_locations(&self) -> GappedMap<FunctionRef, ElementItemId> {
        let mut result = GappedMap::new();
        for segment in self.segments.values() {
            let starting_offset = segment.kind.location().map_or(0, |loc| loc.offset());
            for (item_id, item) in segment.parts.iter() {
                // TODO: handle duplicates
                result.insert(
                    *item,
                    ElementItemId::from_u32(starting_offset + item_id.as_u32()),
                );
            }
        }
        result
    }
}

// pub type IndirectFunctionTable = ElementTable<FunctionRef>;

fn element_kind_to_location(
    element_kind: &wasmparser::ElementKind,
) -> ElementKind<TableRef, GlobalRef> {
    match element_kind {
        wasmparser::ElementKind::Passive => ElementKind::Passive,
        wasmparser::ElementKind::Declared => ElementKind::Declared,
        wasmparser::ElementKind::Active {
            table_index,
            offset_expr,
        } => ElementKind::Active {
            table_ref: TableRef::from_u32(table_index.unwrap_or_default()),
            location: SegmentPlacement::try_from_const_expr(offset_expr)
                .expect("Only const offset supported for active data segments"),
        },
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub enum ElementKind<OwnerId, GlobalRef> {
    Active {
        /// Reference to owner memory.
        table_ref: OwnerId,
        ///
        /// Information about segment placement in owner unit.
        /// The segment placement is an virtual address in owner unit.
        ///
        /// Can be:
        /// - GotBased - means that segments will have offsets relative to value of global reference.
        /// - Constant - means that segments will have constant offsets.
        location: SegmentPlacement<GlobalRef>,
    },
    Passive,
    Declared,
}
impl<OwnerId, GlobalRef> ElementKind<OwnerId, GlobalRef>
where
    GlobalRef: Copy,
{
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Active { .. })
    }
    pub fn location(&self) -> Option<SegmentPlacement<GlobalRef>> {
        match self {
            Self::Active { location, .. } => Some(*location),
            _ => None,
        }
    }
    pub fn table(&self) -> Option<OwnerId>
    where
        OwnerId: Copy,
    {
        match self {
            Self::Active {
                table_ref: memory_ref,
                ..
            } => Some(*memory_ref),
            _ => None,
        }
    }
    pub fn map_ids<NewOwnerId, NewGlobalRef, F, U>(
        self,
        map_owner: F,
        map_global: U,
    ) -> ElementKind<NewOwnerId, NewGlobalRef>
    where
        F: FnOnce(OwnerId) -> NewOwnerId,
        U: FnOnce(GlobalRef) -> NewGlobalRef,
    {
        match self {
            Self::Active {
                table_ref,
                location,
            } => ElementKind::Active {
                table_ref: map_owner(table_ref),
                location: match location {
                    SegmentPlacement::GotBased { global, offset } => SegmentPlacement::GotBased {
                        global: map_global(global),
                        offset,
                    },
                    SegmentPlacement::ConstantOffset(offset) => {
                        SegmentPlacement::ConstantOffset(offset)
                    }
                },
            },
            Self::Passive => ElementKind::Passive,
            Self::Declared => ElementKind::Declared,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typed::GlobalRef;

    #[test]
    fn test_element_kind() {
        let active = ElementKind::<TableRef, GlobalRef>::Active {
            table_ref: TableRef::from_u32(1),
            location: SegmentPlacement::ConstantOffset(10),
        };
        assert!(active.is_active());
        assert_eq!(
            active.location(),
            Some(SegmentPlacement::ConstantOffset(10))
        );
        assert_eq!(active.table(), Some(TableRef::from_u32(1)));

        let passive = ElementKind::<TableRef, GlobalRef>::Passive;
        assert!(!passive.is_active());
        assert_eq!(passive.location(), None);
        assert_eq!(passive.table(), None);

        let declared = ElementKind::<TableRef, GlobalRef>::Declared;
        assert!(!declared.is_active());
        assert_eq!(declared.location(), None);
        assert_eq!(declared.table(), None);
    }

    #[test]
    fn test_build() {
        let mut builder = ElementLayoutBuilder::new();
        let vs_id = builder.virtual_spaces.push(ElementKind::Active {
            table_ref: Temp::from_defined(0),
            location: SegmentPlacement::ConstantOffset(1),
        });
        builder.segments.push(ElementSegmentSpec {
            vs_id,
            name: "segment1".into(),
        });
        builder.items.push(ElementInTable {
            item: FunctionRef::from_u32(42),
            segment_id: SegmentId::from_u32(0),
        });

        let sealed = builder.seal(
            |temp| TableRef::from_u32(temp.as_defined().unwrap()),
            |temp| GlobalRef::from_u32(temp.as_defined().unwrap()),
        );

        assert_eq!(sealed.segments.len(), 1);
        let segment = &sealed.segments[SegmentId::from_u32(0)];
        assert_eq!(segment.name, "segment1");
        assert_eq!(
            segment.kind,
            ElementKind::Active {
                table_ref: TableRef::from_u32(0),
                location: SegmentPlacement::ConstantOffset(1),
            }
        );
        assert_eq!(segment.parts.len(), 1);
        assert_eq!(
            segment.parts[PartId::from_u32(0)],
            FunctionRef::from_u32(42)
        );
    }
}
