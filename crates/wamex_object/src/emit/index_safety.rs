use super::DefinedFunction;
use crate::{
    emit::{
        DefinedFunctionKind, ImportedFunction,
        globals::{DefinedGlobal, GlobalImport},
    },
    index::PrimaryKey,
    read::{
        GetInputRef, InputFuncId, LinkedToInputRef, OutputMapType,
        code::InputFunction,
        typed::{CompoundRef, EntitiesFromInput},
    },
};

impl_entity_index! {
    #[display = "output_function"]
    pub struct OutputFuncId;
    #[display = "output_global"]
    pub struct OutputGlobalId;
    // private types
    pub struct OutputDefinedFuncId(DefinedFunction);
    pub struct OutputImportFuncId(for<'a> ImportedFunction<'a>);
    pub struct OutputDefinedGlobalId(for<'a> DefinedGlobal<'a>);
    pub struct OutputGlobalImportId(for<'a> GlobalImport<'a>);
}

impl CompoundRef for OutputFuncId {
    type DefinedType<'src> = DefinedFunction;
    type ImportType<'src> = ImportedFunction<'src>;
}
impl CompoundRef for OutputGlobalId {
    type DefinedType<'src> = DefinedGlobal<'src>;
    type ImportType<'src> = GlobalImport<'src>;
}

impl LinkedToInputRef for OutputFuncId {
    type InputRef = InputFuncId;
}
impl LinkedToInputRef for OutputGlobalId {
    type InputRef = crate::read::typed::GlobalRef;
}

impl GetInputRef<InputFuncId> for DefinedFunction {
    fn get_input_index(&self) -> OutputMapType<InputFuncId> {
        if matches!(self.kind, DefinedFunctionKind::Trampoline { .. }) {
            // Import stubs is not a real function in input module.
            return OutputMapType::OutputHasInput(self.input_func_id);
        }
        OutputMapType::BidirectionalMap(self.input_func_id)
    }
}
// imported always exist in input module
impl GetInputRef<InputFuncId> for ImportedFunction<'_> {
    fn get_input_index(&self) -> OutputMapType<InputFuncId> {
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
