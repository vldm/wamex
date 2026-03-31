use std::{borrow::Cow, fmt::Debug};

use anyhow::{Result, bail};
use cranelift_entity::{PrimaryMap, packed_option::ReservedValue};
use wasmparser::ElementItems;

use crate::{
    index::{GappedMap, WithStart},
    layouts::{PartId, SegmentPlacement},
    raw::{self, SegmentId},
    typed::{FunctionRef, TableRef},
};

impl_entity_index! {
    #[display = "ei"]
    pub struct ElementItemId;
}
todo!(Implement element build + encoding for seal);
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedElementSegment<'src, T> {
    /// Body of segment, containing defined entities.
    pub parts: PrimaryMap<PartId, T>,

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
    /// Provides a hint for the number of items in the element segment, allowing to pre-allocate the map capacity.
    fn hint_size(items: &ElementItems<'a>) -> Option<u32>;
    /// Provides a way to access each element as internal iterator of `Self` type.
    ///
    /// Params:
    /// save - Internal iterator callback that handles each item
    fn for_item(items: ElementItems<'a>, save: impl FnMut(Self)) -> Result<()>
    where
        Self: Sized;
}

impl ElementType<'_> for FunctionRef {
    fn hint_size(items: &ElementItems<'_>) -> Option<u32> {
        match items {
            ElementItems::Functions(func_indices) => Some(func_indices.count()),
            _ => None,
        }
    }
    fn for_item(items: ElementItems<'_>, mut save: impl FnMut(Self)) -> Result<()> {
        match items {
            ElementItems::Functions(func_indices) => {
                for elem in func_indices.into_iter_with_offsets() {
                    let (_offset, func_id) = elem?;
                    save(FunctionRef::from_u32(func_id));
                }
                Ok(())
            }
            _ => bail!("Invalid element items type for function indices"),
        }
    }
}

impl<'src, T: ElementType<'src> + ReservedValue + Clone> ElementLayoutSealed<'src, T> {
    // Creates ElementLayoutSealed from raw module reader
    // search all element segments for needed table_id,
    // if default_table is set, then segments with no table_index (Wasm MVP spec) are also considered.
    // Returns error if element segment uses unsupported offset expression or item type.
    pub fn typed_from_reader(module: &raw::ObjectReader<'src>, table_id: TableRef) -> Result<Self> {
        let mut this = ElementLayoutSealed {
            segments: PrimaryMap::new(),
            external: WithStart::default(),
            defined: GappedMap::new(),
        };

        for (id, element) in module.elements.iter() {
            let kind = element_kind_to_location(&element.kind);

            match kind.table() {
                Some(idx) if idx == table_id => {}
                _ => {
                    // TODO: support passive/declared segments as well.
                    continue;
                }
            }

            let mut parts = PrimaryMap::new();

            T::for_item(element.items.clone(), |elem| {
                parts.push(elem);
            })?;

            let sealed_segment = SealedElementSegment {
                parts,
                name: id.to_string().into(),
            };
            this.segments.push(sealed_segment);
        }
        Ok(this)
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
