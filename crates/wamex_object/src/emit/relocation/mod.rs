pub mod encode;

use cranelift_entity::packed_option::ReservedValue;

use crate::{
    index::GappedMap,
    typed::{
        FileId,
        common_index::{EntitiesSnapshot, EntityKind, FlatEntityRef},
    },
};

#[derive(Debug, Clone, Copy)]
pub struct EntityLocation {
    file_id: FileId,
    entity: EntityKind,
}

impl ReservedValue for EntityLocation {
    fn is_reserved_value(&self) -> bool {
        self.file_id.is_reserved_value()
            && matches!(self.entity, EntityKind::Tag(t) if t.is_reserved_value())
    }

    fn reserved_value() -> Self {
        EntityLocation {
            file_id: ReservedValue::reserved_value(),
            entity: EntityKind::Tag(ReservedValue::reserved_value()),
        }
    }
}

pub struct InputMapping {
    entity_map: GappedMap<FlatEntityRef, EntityLocation>,
    snapshot: EntitiesSnapshot,
}
impl InputMapping {
    pub fn new(prealocated_snapshot: EntitiesSnapshot) -> Self {
        Self {
            entity_map: GappedMap::new(),
            snapshot: prealocated_snapshot,
        }
    }

    /// Saves the mapping from source entity to output entity, and the file it belongs to.
    /// Panics if the same source entity is inserted more than once.
    pub fn insert(&mut self, src: EntityKind, file_id: FileId, entity: EntityKind) {
        let src = self.snapshot.pack_ref(src);
        let any_ref = self
            .entity_map
            .insert(src, EntityLocation { file_id, entity });

        assert!(any_ref.is_none(), "Duplicate entity saved")
    }

    /// Returns the file id of the source entity, if it exists in the map
    pub fn get_file_id(&self, src: EntityKind) -> Option<FileId> {
        let src = self.snapshot.pack_ref(src);
        self.entity_map.get(src).map(|f| f.file_id)
    }

    /// Returns source entity ref, if it exists in the map
    pub fn get_entity(&self, src: EntityKind) -> Option<EntityKind> {
        let src = self.snapshot.pack_ref(src);
        self.entity_map.get(src).map(|f| f.entity)
    }
}
// use std::fmt::Debug;

// use anyhow::{Result, anyhow, bail};
// use cranelift_entity::EntityRef;

// use crate::{
//     InputObject,
//     emit::{
//         ComputedModules, GotBase, ModuleEmitState,
//         index_safety::{OutputDataId, OutputFuncId, OutputGlobalId},
//         modify::{SymbolOp, newgen::ErasedEntityId},
//     },
//     read::{
//         raw::DataSegmentId,
//         typed::{FunctionRef, GlobalRef, data::DataSymbolRef},
//     },
//     symbols::{
//         SymbolId, SymbolKind,
//         reloc::{
//             AnyRelocationEntry, Encoding, Relative, RelocationEntry, RelocationWidth, SymbolType,
//         },
//     },
// };

// pub(crate) trait EntryTypeTag {
//     type OutputValue;
//     // Index or offset of symbol in corresponding module
//     fn get_mapped_value(
//         input: &InputObject<'_>,
//         state: &ModuleEmitState,
//         src_symbol: SymbolId,
//     ) -> Option<Self::OutputValue>;
//     // fn get_mapped_value2(
//     //     state: &ModuleEmitState,
//     //     src_symbol: ErasedEntityId,
//     // ) -> Option<Self::OutputValue>;
//     fn get_got(got_base: &GotBase) -> OutputGlobalId;
// }

// pub enum FunctionIndexTag {}
// pub enum DataSymbolTag {}

// impl FunctionIndexTag {
//     fn get_input_function_id(input: &InputObject<'_>, src_symbol: SymbolId) -> Option<FunctionRef> {
//         let SymbolKind::Func { input_id } = input.symbols.get(src_symbol)?.kind else {
//             return None;
//         };
//         Some(input_id)
//     }
//     // fn get_input_function_id2(
//     //     state: &ModuleEmitState,
//     //     src_symbol: ErasedEntityId,
//     // ) -> Option<FunctionRef> {
//     //     let input_func_id = match src_symbol.unpack_id() {
//     //         UnpackedEntityId::Input(i) => FunctionRef::from_u32(i),
//     //         UnpackedEntityId::Output(o) => {
//     //             // already resolved
//     //             let output_func_id = OutputFuncId::from_u32(o);

//     //             state.functions.get_input_id(output_func_id)?
//     //         }
//     //     };
//     //     return Some(input_func_id);
//     // }
// }

// impl EntryTypeTag for FunctionIndexTag {
//     type OutputValue = usize;
//     fn get_mapped_value(
//         input: &InputObject<'_>,
//         state: &ModuleEmitState,
//         src_symbol: SymbolId,
//     ) -> Option<Self::OutputValue> {
//         let input_func_id = FunctionIndexTag::get_input_function_id(input, src_symbol)?;
//         state
//             .indirect_functions
//             .function_table_index
//             .get(&input_func_id)
//             .copied()
//     }
//     // fn get_mapped_value2(
//     //     state: &ModuleEmitState,
//     //     src_symbol: ErasedEntityId,
//     // ) -> Option<Self::OutputValue> {
//     //     let input_func_id = FunctionIndexTag::get_input_function_id2(state, src_symbol)?;
//     //     // TODO: use output function id instead of input function id?
//     //     state
//     //         .indirect_functions
//     //         .function_table_index
//     //         .get(&input_func_id)
//     //         .copied()
//     // }
//     fn get_got(got_base: &GotBase) -> OutputGlobalId {
//         got_base.table_base_id
//     }
// }

// impl DataSymbolTag {
//     fn get_symbol_offset(
//         state: &ModuleEmitState,
//         segment_id: DataSegmentId,
//         data_symbol_id: SymbolId,
//     ) -> Option<<DataSymbolTag as EntryTypeTag>::OutputValue> {
//         let segment = state.data.get(segment_id)?;
//         let symbol = segment.symbols().get(&data_symbol_id);
//         let Some(symbol) = symbol else {
//             return None;
//         };

//         Some(segment.memory_offset() as i64 + symbol.data_mem_offset as i64)
//     }
// }
// impl EntryTypeTag for DataSymbolTag {
//     type OutputValue = i64;
//     fn get_mapped_value(
//         input: &InputObject<'_>,
//         state: &ModuleEmitState,
//         src_symbol: SymbolId,
//     ) -> Option<Self::OutputValue> {
//         let SymbolKind::DataDefined { segment_id, .. } = input.symbols.get(src_symbol)?.kind else {
//             return None;
//         };

//         DataSymbolTag::get_symbol_offset(state, segment_id, src_symbol)
//     }
//     // fn get_mapped_value2(
//     //     state: &ModuleEmitState,
//     //     src_symbol: ErasedEntityId,
//     // ) -> Option<Self::OutputValue> {
//     //     todo!()
//     // }
//     fn get_got(got_base: &GotBase) -> OutputGlobalId {
//         got_base.lib_base_id
//     }
// }

// #[derive(Clone)]
// pub(crate) struct RelocateState<'any, 'src> {
//     pub input_module: &'any InputObject<'src>,
//     pub computed_modules: &'any ComputedModules<'any, 'src>,
//     pub global_id_mapper: &'any dyn Fn(GlobalRef) -> Option<OutputGlobalId>,
//     pub emit_module: &'any ModuleEmitState<'any, 'src>,
// }

// impl Debug for RelocateState<'_, '_> {
//     fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
//         f.debug_struct("RelocateState")
//             .field("input_module", &"InputModule { ... }")
//             .field("main_module", &"ModuleEmitState { ... }")
//             .field("emit_module", &"ModuleEmitState { ... }")
//             .finish()
//     }
// }

// impl<'any, 'src> RelocateState<'any, 'src> {
//     fn _get_symbol_op<T: EntryTypeTag, U>(
//         &self,
//         getter: impl Fn(&ModuleEmitState) -> Option<U>,
//         not_found: impl FnOnce() -> anyhow::Error,
//     ) -> Result<SymbolOp<U>> {
//         if let Some(value) = getter(&self.computed_modules.main_module) {
//             return Ok(SymbolOp::StaticOffset { value });
//         }
//         if let Some(value) = getter(self.emit_module) {
//             return Ok(SymbolOp::GotBased {
//                 value,
//                 got: self
//                     .emit_module
//                     .get_submodule_extra(None)
//                     .map(|extra| T::get_got(extra))
//                     .ok_or_else(not_found)?,
//             });
//         }

//         for (id, module) in &self.computed_modules.shared_modules {
//             if let Some(value) = getter(module) {
//                 return Ok(SymbolOp::GotBased {
//                     value,

//                     got: self
//                         .emit_module
//                         .get_submodule_extra(Some(id))
//                         .map(|extra| T::get_got(extra))
//                         .ok_or_else(not_found)?,
//                 });
//             }
//         }

//         Err(not_found())
//     }

//     pub(crate) fn get_entry_symbol_op<T: EntryTypeTag>(
//         &self,
//         relocation: &RelocationEntry,
//     ) -> Result<SymbolOp<T::OutputValue>>
//     where
//         T::OutputValue: TryFrom<i64>,
//         <T::OutputValue as TryFrom<i64>>::Error: Debug,
//         T::OutputValue: std::ops::Add<Output = T::OutputValue>,
//     {
//         self._get_symbol_op::<T, _>(
//             |module| {
//                 T::get_mapped_value(self.input_module, module, relocation.symbol_id)
//             },
//             || {
//                 anyhow!(
//                 "Symbol within relocation {relocation:?} not found in either main or emit module"
//             )
//             },
//         ).map(|res| res.map(|v| v + relocation.addend.try_into().unwrap()))
//     }

//     pub(crate) fn get_data_symbol_op(
//         &self,
//         segment_id: DataSegmentId,
//         data_symbol_id: SymbolId,
//     ) -> Result<SymbolOp<<DataSymbolTag as EntryTypeTag>::OutputValue>> {
//         self._get_symbol_op::<DataSymbolTag, _>(
//             |module| {
//                 DataSymbolTag::get_symbol_offset(module, segment_id, data_symbol_id)
//             },
//             || {
//                 anyhow!(
//                     "Data symbol {segment_id:?}: {data_symbol_id:?} not found in either main or emit module"
//                 )
//             },
//         )
//     }

//     fn ensure_empty_addend<Any: Debug>(relocation: &RelocationEntry<Any>) -> Result<()> {
//         if relocation.addend != 0 {
//             bail!(
//                 "Relocation {relocation:?} has non-zero addend {}, which is not supported",
//                 relocation.addend
//             );
//         }
//         Ok(())
//     }

//     fn get_relocated_function_index(&self, relocation: &RelocationEntry) -> Result<usize> {
//         Self::ensure_empty_addend(relocation)?;
//         let Some(input_func_id) =
//             FunctionIndexTag::get_input_function_id(self.input_module, relocation.symbol_id)
//         else {
//             bail!("Relocation {relocation:?} does not refer to a valid function")
//         };
//         let Some(output_func_id) = self.emit_module.functions.get_output_id(input_func_id) else {
//             bail!(
//                 "Cannot find output function for input function {input_func_id} referenced by relocation {relocation:?}"
//             )
//         };
//         Ok(output_func_id.index())
//     }

//     fn get_relocated_function_table_index(&self, relocation: &RelocationEntry) -> Result<usize> {
//         Self::ensure_empty_addend(relocation)?;
//         let result = self.get_entry_symbol_op::<FunctionIndexTag>(relocation)?;
//         Ok(*result
//             .as_static()
//             .unwrap_or_else(||panic!("Relocation should only process static symbols, got {result:?}, for entry {relocation:?}")))
//     }

//     fn get_relocated_memory_offset(&self, relocation: &RelocationEntry) -> Result<usize> {
//         let result = self.get_entry_symbol_op::<DataSymbolTag>(relocation)?;
//         let offset = *result
//             .as_static()
//             .unwrap_or_else(||panic!("Relocation should only process static symbols, got {result:?}, for entry {relocation:?}"));

//         Ok(offset as usize)
//     }

//     fn get_relocated_global_index(&self, relocation: &RelocationEntry) -> Result<usize> {
//         let symbol = self
//             .input_module
//             .symbols
//             .get(relocation.symbol_id)
//             .ok_or_else(|| {
//                 anyhow!(
//                     "Relocation {relocation:?} refers to invalid symbol id {}",
//                     relocation.symbol_id
//                 )
//             })?;
//         let SymbolKind::Global(original_global_id) = symbol.kind else {
//             bail!(
//                 "Relocation {relocation:?} does not refer to a global symbol, instead got {symbol:?}"
//             );
//         };

//         let global_id = (self.global_id_mapper)(original_global_id)
//             .ok_or_else(|| {
//                 anyhow!(
//                     "Dependency analysis error: No output global for input global {original_global_id} referenced by relocation {relocation:?}"
//                 )
//             })?;
//         Ok(global_id.index())
//     }

//     pub fn apply_relocation(&self, data: &mut [u8], relocation: &AnyRelocationEntry) -> Result<()> {
//         let relocation_range = relocation.relocation_range();
//         let target = &mut data[relocation_range];
//         let relocated_value = match relocation {
//             AnyRelocationEntry::Linkage(relocation) => match relocation.symbol_type {
//                 SymbolType::FunctionIndex => self.get_relocated_function_index(relocation)? as u32,
//                 SymbolType::TableIndex => {
//                     self.get_relocated_function_table_index(relocation)? as u32
//                 }
//                 SymbolType::MemoryAddr => self.get_relocated_memory_offset(relocation)? as u32,
//                 SymbolType::GlobalIndex => self.get_relocated_global_index(relocation)? as u32,
//                 SymbolType::TableNumber
//                 | SymbolType::SectionOffset
//                 | SymbolType::MemoryAddrLocrel
//                 | SymbolType::FunctionOffset
//                 | SymbolType::EventIndex => {
//                     log::warn!("This type of relocation is not supported yet: {relocation:?}");
//                     // skip relocation.
//                     return Ok(());
//                 }
//                 SymbolType::TypeIndex => {
//                     unreachable!("BUG: TypeId relocation cannot use linkage entry")
//                 }
//             },
//             AnyRelocationEntry::Type(relocation) => {
//                 log::warn!("TypeId relocation is not supported yet: {relocation:?}");
//                 // return original type index as-is
//                 relocation.index.as_u32()
//             }
//         };

//         match relocation {
//             AnyRelocationEntry::Linkage(relocation) => Self::encode(
//                 target,
//                 relocated_value,
//                 relocation.encoding,
//                 relocation.width,
//             ),
//             AnyRelocationEntry::Type(_) => Self::encode(
//                 target,
//                 relocated_value,
//                 Encoding::Leb,
//                 RelocationWidth::Bits32,
//             ),
//         }

//         Ok(())
//     }

//     fn encode(target: &mut [u8], value: u32, encoding: Encoding, width: RelocationWidth) {
//         use encode::*;
//         match (encoding, width) {
//             (Encoding::Fixed, RelocationWidth::Bits32) => {
//                 encode_u32(value, target.try_into().unwrap())
//             }
//             (Encoding::Leb, RelocationWidth::Bits32) => {
//                 encode_leb128_u32_5byte(value, target.try_into().unwrap())
//             }
//             (Encoding::Sleb, RelocationWidth::Bits32) => {
//                 encode_leb128_i32_5byte(value as i32, target.try_into().unwrap())
//             }
//             (Encoding::Fixed, RelocationWidth::Bits64) => {
//                 encode_u64(value as u64, target.try_into().unwrap())
//             }
//             (Encoding::Leb, RelocationWidth::Bits64) => {
//                 encode_leb128_u64_10byte(value as u64, target.try_into().unwrap())
//             }
//             (Encoding::Sleb, RelocationWidth::Bits64) => {
//                 encode_leb128_i64_10byte(value as i64, target.try_into().unwrap())
//             }
//         }
//     }

//     fn get_relocated_function_index2(
//         &self,
//         relocation: &RelocationEntry<ErasedEntityId>,
//     ) -> Result<usize> {
//         Self::ensure_empty_addend(relocation)?;
//         debug_assert_eq!(relocation.symbol_type, SymbolType::FunctionIndex);

//         let output_func_id = match relocation.symbol_id {
//             ErasedEntityId::Input(i) => {
//                 let input_func_id = FunctionRef::from_u32(i);
//                 let Some(output_func_id) = self.emit_module.functions.get_output_id(input_func_id)
//                 else {
//                     bail!(
//                         "Cannot find output function for input function {input_func_id} referenced by relocation {relocation:?}"
//                     )
//                 };
//                 output_func_id
//             }
//             ErasedEntityId::Output(o) => {
//                 // already resolved
//                 OutputFuncId::from_u32(o)
//             }
//         };

//         Ok(output_func_id.index())
//     }
//     fn get_relocated_function_table_index2(
//         &self,
//         relocation: &RelocationEntry<ErasedEntityId>,
//     ) -> Result<usize> {
//         Self::ensure_empty_addend(relocation)?;
//         debug_assert_eq!(relocation.symbol_type, SymbolType::TableIndex);

//         let input_func_id = match relocation.symbol_id {
//             ErasedEntityId::Input(i) => FunctionRef::from_u32(i),
//             ErasedEntityId::Output(o) => {
//                 // already resolved
//                 let output_func_id = OutputFuncId::from_u32(o);

//                 self.static_module()
//                     .functions
//                     .get_input_id(output_func_id)
//                     .unwrap_or_else(|| panic!("output function {output_func_id} should have input id, required for relocation: {relocation:?}"))
//             }
//         };

//         // TODO: use output function id instead of input function id?
//         let table_index = self
//             .static_module()
//             .indirect_functions
//             .function_table_index
//             .get(&input_func_id)
//             .copied();

//         table_index.ok_or_else(||
//             anyhow!("failed to find indirect table index for function {input_func_id}, required for relocation: {relocation:?}")
//         )
//     }
//     fn get_relocated_memory_offset2(
//         &self,
//         relocation: &RelocationEntry<ErasedEntityId>,
//     ) -> Result<usize> {
//         let output_data_ref = match relocation.symbol_id {
//             ErasedEntityId::Input(i) => {
//                 let input_data_id = DataSymbolRef::from_u32(i);
//                 self.static_module()
//                     .get_output_data_id(input_data_id)
//                     .ok_or_else(|| {
//                         anyhow!("failed to get symbol reference {input_data_id:?} in output module")
//                     })?
//             }
//             ErasedEntityId::Output(o) => OutputDataId::from_u32(o),
//         };
//         let (segment_id, symbol_id) = self.static_module().get_output_data_sym(output_data_ref);
//         let segment = self.static_module().data.get(segment_id).ok_or_else(|| {
//             anyhow!(
//                 "failed to get data segment {segment_id:?} for output data symbol {output_data_ref:?}"
//             )
//         })?;
//         let Some(symbol) = segment.symbols().get(&symbol_id) else {
//             bail!(
//                 "failed to get data symbol {symbol_id:?} in segment {segment_id:?} for output data symbol {output_data_ref:?}"
//             );
//         };

//         Ok(
//             (segment.memory_offset() as i64 + symbol.data_mem_offset as i64)
//                 .try_into()
//                 .unwrap(),
//         )
//     }

//     fn get_relocated_global_index2(
//         &self,
//         relocation: &RelocationEntry<ErasedEntityId>,
//     ) -> Result<usize> {
//         let output_global_id = match relocation.symbol_id {
//             ErasedEntityId::Input(i) => {
//                 let input_global_id = GlobalRef::from_u32(i);
//                 (self.global_id_mapper)(input_global_id)
//                 .ok_or_else(|| {
//                     anyhow!(
//                         "Dependency analysis error: No output global for input global {input_global_id} referenced by relocation {relocation:?}"
//                     )
//                 })?
//             }
//             ErasedEntityId::Output(o) => {
//                 // already resolved
//                 let output_global_id = OutputGlobalId::from_u32(o);
//                 output_global_id
//             }
//         };
//         Ok(output_global_id.index())
//     }
//     pub fn apply_relocation2(
//         &self,
//         data: &mut [u8],
//         relocation: &RelocationEntry<ErasedEntityId>,
//     ) -> Result<()> {
//         let relocation_range = relocation.relocation_range();
//         let target = &mut data[relocation_range];
//         if relocation.relation != Relative::None {
//             log::warn!(
//                 "Relocation with non relation to global is not supported yet: {relocation:?}"
//             );
//             // skip relocation.
//             return Ok(());
//         }
//         let relocated_value = match relocation.symbol_type {
//             SymbolType::FunctionIndex => self.get_relocated_function_index2(&relocation)? as u32,
//             SymbolType::TableIndex => self.get_relocated_function_table_index2(&relocation)? as u32,
//             SymbolType::MemoryAddr => self.get_relocated_memory_offset2(relocation)? as u32,
//             SymbolType::GlobalIndex => self.get_relocated_global_index2(relocation)? as u32,
//             SymbolType::TableNumber
//             | SymbolType::SectionOffset
//             | SymbolType::MemoryAddrLocrel
//             | SymbolType::FunctionOffset
//             | SymbolType::EventIndex => {
//                 log::warn!("This type of relocation is not supported yet: {relocation:?}");
//                 // skip relocation.
//                 return Ok(());
//             }
//             SymbolType::TypeIndex => {
//                 log::warn!("TypeId relocation is not supported yet: {relocation:?}");
//                 // return original type index as-is
//                 match relocation.symbol_id {
//                     ErasedEntityId::Input(i) => i,
//                     ErasedEntityId::Output(o) => o,
//                 }
//             }
//         };

//         Self::encode(
//             target,
//             relocated_value,
//             relocation.encoding,
//             relocation.width,
//         );
//         // match relocation {
//         //     AnyRelocationEntry::Linkage(relocation) => Self::encode(
//         //         target,
//         //         relocated_value,
//         //         relocation.encoding,
//         //         relocation.width,
//         //     ),
//         //     AnyRelocationEntry::Type(_) => Self::encode(
//         //         target,
//         //         relocated_value,
//         //         Encoding::Leb,
//         //         RelocationWidth::Bits32,
//         //     ),
//         // }

//         Ok(())
//     }

//     fn static_module(&self) -> &ModuleEmitState<'any, 'src> {
//         &self.computed_modules.main_module
//     }
// }
