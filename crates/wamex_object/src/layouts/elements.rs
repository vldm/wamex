use std::{borrow::Cow, fmt::Debug, io::Write};

use anyhow::Result;
use cranelift_entity::{PrimaryMap, packed_option::ReservedValue};
use wasmparser::ElementItems;

use crate::{
    emit::modify::wasm_emitter::{self, SectionList},
    index::{GappedMap, WithStart},
    layouts::{PartId, SegmentPlacement},
    raw::{self, SegmentId},
    typed::{FunctionRef, TableRef},
};

impl_entity_index! {
    #[display = "ei"]
    pub struct ElementItemId;
}
// TODO: 1. element builder
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedElementSegment<'src, T> {
    /// Body of segment, containing defined entities.
    pub parts: PrimaryMap<PartId, T>,

    pub kind: ElementKind<TableRef>,
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

    /// Imported items that left after sealing.
    pub(crate) external: WithStart<ElementItemId, ()>,
    /// Can have gaps when recover from object file (e.g. overlapping items).
    pub(crate) defined: GappedMap<ElementItemId, ElementItemPlace>,
}

pub trait ElementType<'a>: Debug {
    fn decode(items: ElementItems<'a>) -> impl Iterator<Item = Result<Self>> + 'a
    where
        Self: Sized;

    fn encode_segment_start<W>(
        kind: ElementKind<TableRef>,
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
        kind: ElementKind<TableRef>,
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
            external: WithStart::default(),
            defined: GappedMap::new(),
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

// pub type IndirectFunctionTable = ElementTable<FunctionRef>;

fn element_kind_to_location(element_kind: &wasmparser::ElementKind) -> ElementKind<TableRef> {
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
pub enum ElementKind<OwnerId> {
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
        location: SegmentPlacement,
    },
    Passive,
    Declared,
}
impl<OwnerId> ElementKind<OwnerId> {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Active { .. })
    }
    pub fn location(&self) -> Option<SegmentPlacement> {
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
}
