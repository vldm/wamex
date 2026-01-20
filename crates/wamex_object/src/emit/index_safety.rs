use super::DefinedFunction;
use crate::{
    emit::{
        DefinedFunctionKind, ImportedFunction,
        globals::{DefinedGlobal, GlobalImport},
    },
    read::{FunctionRef, GetInputRef, LinkedToInputRef, OutputMapType, typed::CompoundRef},
};

impl_entity_index! {
    #[display = "output_function"]
    pub struct OutputFuncId;
    #[display = "output_global"]
    pub struct OutputGlobalId;
    // Non wasm-entity type
    #[display = "output_data"]
    pub struct OutputDataId;
    // private types
    pub struct OutputDefinedFuncId(for<'a> DefinedFunction<'a>);
    pub struct OutputImportFuncId(for<'a> ImportedFunction<'a>);
    pub struct OutputDefinedGlobalId(for<'a> DefinedGlobal<'a>);
    pub struct OutputGlobalImportId(for<'a> GlobalImport<'a>);
}

impl CompoundRef for OutputFuncId {
    type DefinedType<'src> = DefinedFunction<'src>;
    type ImportType<'src> = ImportedFunction<'src>;
}
impl CompoundRef for OutputGlobalId {
    type DefinedType<'src> = DefinedGlobal<'src>;
    type ImportType<'src> = GlobalImport<'src>;
}

impl LinkedToInputRef for OutputFuncId {
    type InputRef = FunctionRef;
}
impl LinkedToInputRef for OutputGlobalId {
    type InputRef = crate::read::typed::GlobalRef;
}

impl GetInputRef<FunctionRef> for DefinedFunction<'_> {
    fn get_input_index(&self) -> OutputMapType<FunctionRef> {
        if matches!(self.kind, DefinedFunctionKind::Trampoline { .. }) {
            // Import stubs is not a real function in input module.
            return OutputMapType::OutputHasInput(self.input_func_id);
        }
        // indirect trampolines are imports that converted into defined functions
        OutputMapType::BidirectionalMap(self.input_func_id)
    }
}
// imported always exist in input module
impl GetInputRef<FunctionRef> for ImportedFunction<'_> {
    fn get_input_index(&self) -> OutputMapType<FunctionRef> {
        OutputMapType::BidirectionalMap(self.input_func_id())
    }
}
impl GetInputRef<crate::read::typed::GlobalRef> for DefinedGlobal<'_> {
    fn get_input_index(&self) -> OutputMapType<crate::read::typed::GlobalRef> {
        match self {
            DefinedGlobal::PlainCopy {
                input_global_id, ..
            } => OutputMapType::BidirectionalMap(*input_global_id),
            DefinedGlobal::WithConstructor(_) => OutputMapType::None,
        }
    }
}
impl GetInputRef<crate::read::typed::GlobalRef> for GlobalImport<'_> {
    fn get_input_index(&self) -> OutputMapType<crate::read::typed::GlobalRef> {
        match self {
            GlobalImport::Existing {
                input_global_id, ..
            } => OutputMapType::BidirectionalMap(*input_global_id),
            GlobalImport::New {
                input_global_id, ..
            } => OutputMapType::bidirectional_from_option(*input_global_id),
        }
    }
}

// Encode output index as `SymbolId` for storing in relocation entries.
// Uses MSB to distinguish from symbols that can be found in symbols table.
//
// This is intended for use in places where we need to allocate new symbols, b
// 1. Make sure to not use this SmbolID in SecondaryMap since it will allocate space for all gaps up to 2^31.
// 2.
trait OutputIndex {
    fn as_u32(&self) -> u32;
    fn from_u32(value: u32) -> Self
    where
        Self: Sized;

    fn to_output_symbol_id(&self) -> crate::emit::SymbolId {
        crate::emit::SymbolId::from_u32(self.as_u32() | 1 << 31)
    }
    fn from_output_symbol_id_unchecked(value: crate::emit::SymbolId) -> Self
    where
        Self: Sized,
    {
        Self::from_u32(value.as_u32() & !(1 << 31))
    }
}
