use std::fmt::Debug;

use anyhow::{Context, bail, ensure};
use cranelift_entity::{EntityRef, packed_option::ReservedValue};
use itertools::Itertools;
use wasmparser::ElementKind;

use super::{ElementItems, Result};
use crate::{
    SVec,
    index::GappedMap,
    raw,
    typed::{FunctionRef, Module, TableRef, data::SpecificLocation},
};

impl_entity_index! {
    #[display = "ei"]
    pub struct ElementItemId;
}
pub trait ElementType<'a>: Debug {
    /// Provides a hint for the number of items in the element segment, allowing to pre-allocate the map capacity.
    fn hint_size(items: &ElementItems<'a>) -> Option<u32>;
    /// Provides a way to access each element as internal iterator of `Self` type.
    ///
    /// Params:
    /// first - Id of the first element item.
    /// save - Internal iterator callback that handles each item (stores in a map ID -> Self)
    fn for_item(
        items: ElementItems<'a>,
        first: ElementItemId,
        save: impl FnMut(ElementItemId, Self),
    ) -> Result<()>
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
    fn for_item(
        items: ElementItems<'_>,
        first: ElementItemId,
        mut save: impl FnMut(ElementItemId, Self),
    ) -> Result<()> {
        match items {
            ElementItems::Functions(func_indices) => {
                let mut elem_id = first;
                for elem in func_indices.into_iter_with_offsets() {
                    let (_offset, func_id) = elem?;
                    save(elem_id, FunctionRef::from_u32(func_id));
                    elem_id = elem_id.next();
                }
                Ok(())
            }
            _ => bail!("Invalid element items type for function indices"),
        }
    }
}

#[derive(Debug)]
pub struct ElementTable<T: ReservedValue + Clone> {
    /// ID of table with indirect functions definition
    pub table_id: TableRef,
    /// Allow gaps in case of non-initialized elements
    pub items: GappedMap<ElementItemId, T>,
    /// Enforce item strarting from ElementId to be placed in new segment.
    pub extra_segments: SVec<ElementItemId>,
    /// Location of element segment
    pub location: SpecificLocation,
}

impl<T: ReservedValue + Clone> ElementTable<T> {
    pub fn new(table_id: TableRef) -> Self {
        Self {
            table_id,
            items: GappedMap::new(),
            extra_segments: SVec::new(),
            location: SpecificLocation::ConstantOffset(0),
        }
    }
    /// Iterates over all items,
    /// split by segments if gaps are present, or if extra_segments are specified.
    /// The callback receives segment id and iterator of items in the segment.
    pub fn for_each_segment(
        &self,
        mut f: impl FnMut(usize, &mut dyn Iterator<Item = (ElementItemId, &T)>),
    ) {
        let mut extra_segments = self.extra_segments.clone();
        extra_segments.sort_unstable();
        extra_segments.dedup();

        let scan_state = (
            0,
            ElementItemId::from_u32(0),
            extra_segments.into_iter().peekable(),
        );

        let iter = self
            .items
            .iter()
            .peekable()
            .scan(
                scan_state,
                |(segment_id, prev, extra_segments), (id, item)| {
                    let has_gap = *prev != id;
                    let id_eq_extra = extra_segments
                        .peek()
                        .is_some_and(|&extra_id| id == extra_id);

                    if id_eq_extra {
                        extra_segments.next();
                    }

                    if has_gap || id_eq_extra {
                        *segment_id += 1;
                    }
                    *prev = id;
                    // Mark segments with segment_id
                    Some((*segment_id, (id, item)))
                },
            )
            .chunk_by(|(segment_id, _)| *segment_id);
        for (segment_id, group) in &iter {
            f(segment_id, &mut group.map(|(_, item)| item))
        }
    }
}

impl<'a, T: ElementType<'a> + ReservedValue + Clone> ElementTable<T> {
    // Creates ElementTable from raw module reader
    // search all element segments for needed table_id,
    // if default_table is set, then segments with no table_index (Wasm MVP spec) are also considered.
    // Returns error if element segment uses unsupported offset expression or item type.
    pub fn from_reader(
        module: &raw::ObjectReader<'a>,
        table_id: TableRef,
        default_table: bool,
    ) -> Result<Self> {
        let mut table = Self::new(table_id);
        for (id, element) in module.elements.iter() {
            let ElementKind::Active {
                table_index,
                offset_expr,
            } = &element.kind
            else {
                continue;
            };

            match table_index {
                // if table_id matched
                Some(idx) if *idx == table_id.index() as u32 => {}
                // or we processing default table on Wasm MVP spec
                None if default_table => {}
                _ => {
                    continue;
                }
            }

            // Multisegment support
            let offset = Module::read_const_expr(offset_expr)
                .with_context(|| format!("Failed to read offset expression for element {id:?}"))?;

            ensure!(
                offset > 0,
                "Negative offset expressions are not supported in element segments (element {id:?})",
            );

            let item_id =
                ElementItemId::from_u32(offset.try_into().expect("Negative offset checked above"));

            // SecondaryMap not yet support reserving capacity, so skipping for now
            // if let Some(size) = T::hint_size(&element.items) {
            //     let max_elem = item_id.index() + size as usize;
            //     let extra = max_elem.saturating_sub(table.items.len());

            //     log::debug!(
            //         "Reserving {} extra element slots for element segment {:?} in table {:?}",
            //         extra,
            //         id,
            //         table_id,
            //     );

            //     table.items.reserve(extra);
            // }

            T::for_item(element.items.clone(), item_id, |elem_id, elem| {
                table.items[elem_id] = elem.into();
            })?;
        }
        Ok(table)
    }
}

pub type IndirectFunctionTable = ElementTable<FunctionRef>;
