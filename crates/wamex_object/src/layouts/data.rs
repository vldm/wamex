use anyhow::{Result, bail, ensure};
use cranelift_entity::EntityRef;

use super::MemLayoutSealed;
use crate::{
    index::{GappedMap, Temp},
    layouts::{DefinedDataChunk, MemLayoutBuilder, VirtualSpaceLocation, sealed::ItemPlace},
    raw::SegmentId,
    typed::{GlobalRef, ImportOrDefined, ImportedDataChunk, MemoryRef},
};

impl_entity_index! {
    #[display="data"]
    pub struct DataSymbolRef;
}

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
        id.to_stable_n32(0, self.defined.len())
    }
}
//
// Impl for builder
//

// Align base of memory to 16 bytes, if it wasn't already aligned.
pub const BASE_ALIGNMENT: u8 = u8::trailing_zeros(16) as u8;

pub const DEFAULT_HEAP_START: SpecificLocation = SpecificLocation::ConstantOffset(0x100000);

impl<'src> MemLayoutBuilder<'src> {
    /// Create default virtual space, inited as active with one segment.
    ///
    /// Return id of this segment
    pub fn try_create_base_segment(&mut self, owner_id: Temp<MemoryRef>) -> SegmentId {
        let vs = self.try_create_base_vs(VirtualSpaceLocation {
            owner_id,
            location: DEFAULT_HEAP_START,
        });
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
    pub fn main_vs(&self) -> Option<VirtualSpaceLocation<Temp<MemoryRef>>> {
        self.virtual_spaces
            .values()
            .copied()
            .find(Option::is_none)
            .flatten()
    }
}

pub type DataSymbolsOffsets = GappedMap<DataSymbolRef, ItemPlace>;

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

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SpecificLocation {
    /// Place chunk at offset (starting from mem_start) in active memory, where offset is calculated as value of global + offset.
    GotBased { global: GlobalRef, offset: u32 },
    /// Place chunk at offset (starting from mem_start) in active memory.
    ConstantOffset(u32),
}

impl SpecificLocation {
    pub fn offset(&self) -> u32 {
        match self {
            SpecificLocation::GotBased { offset, .. } => *offset,
            SpecificLocation::ConstantOffset(offset) => *offset,
        }
    }
    pub fn global_ref(&self) -> Option<GlobalRef> {
        match self {
            SpecificLocation::GotBased { global, .. } => Some(*global),
            SpecificLocation::ConstantOffset(_) => None,
        }
    }
    pub fn with_zero_offset(&self) -> Self {
        match self {
            SpecificLocation::GotBased { global, .. } => SpecificLocation::GotBased {
                global: *global,
                offset: 0,
            },
            SpecificLocation::ConstantOffset(_) => SpecificLocation::ConstantOffset(0),
        }
    }
    /// Decode simple offset expressions:
    /// - `i32.const` for constant offsets
    /// - `global.get` for GOT based offsets
    /// - `global.get + i32.const` for GOT based offsets with constant offset.
    pub fn try_from_const_expr(offset_expr: &wasmparser::ConstExpr) -> Result<Self> {
        let mut reader = offset_expr.get_operators_reader();

        let mut offset = None;
        let mut got = None;

        match reader.read()? {
            wasmparser::Operator::I32Const { value } => {
                ensure!(
                    offset.is_none(),
                    "Too complex expression (more than one offset)"
                );
                offset = Some(value);
            }
            wasmparser::Operator::GlobalGet { global_index } => {
                ensure!(
                    got.is_none(),
                    "Too complex expression (more than one got reference)"
                );
                got = Some(GlobalRef::from_u32(global_index))
            }
            op => bail!(
                "Too complex expression found unexpected instruction: {:?}",
                op
            ),
        };
        if got.is_some() && offset.is_some() {
            match reader.read()? {
                wasmparser::Operator::I32Add => {}
                op => bail!(
                    "Too complex expression found expected I32Add found: {:?}",
                    op
                ),
            }
        }
        match reader.read()? {
            wasmparser::Operator::End => {}
            op => bail!("Expected End after const expr: {:?}", op),
        }
        let offset = offset.unwrap_or_default() as u32;
        Ok(match got {
            Some(global) => SpecificLocation::GotBased { global, offset },
            None => SpecificLocation::ConstantOffset(offset),
        })
    }

    pub fn add_offset(&self, offset: u32) -> Self {
        match self {
            SpecificLocation::GotBased {
                global,
                offset: base,
            } => SpecificLocation::GotBased {
                global: *global,
                offset: base + offset,
            },
            SpecificLocation::ConstantOffset(base) => {
                SpecificLocation::ConstantOffset(base + offset)
            }
        }
    }
    pub fn to_init_expr(&self) -> wasm_encoder::ConstExpr {
        match self {
            SpecificLocation::GotBased { global, offset } => {
                wasm_encoder::ConstExpr::global_get(global.as_u32())
                    .with_i32_const((*offset).try_into().unwrap())
                    .with_i32_add()
            }
            SpecificLocation::ConstantOffset(base) => {
                wasm_encoder::ConstExpr::i32_const((*base).try_into().unwrap())
            }
        }
    }
}
