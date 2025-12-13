use super::{DefinedFunction, Indexed};
use crate::{
    emit::{
        DefinedFunctionKind, ImportedFunction,
        globals::{DefinedGlobal, GlobalImport},
    },
    index::OutputMapType,
    read::code::InputFunction,
};

pub type OutputFuncId = crate::index::Id<DefinedFunction>;

impl Indexed for DefinedFunction {
    type StaticTypeTagForIndex = Self;
    type IndexType = crate::index::Id<Self::StaticTypeTagForIndex>;
}

impl<'src> crate::index::Defined<'src> for DefinedFunction {
    type Import = ImportedFunction<'src>;
}
impl<'src> crate::index::OutputType<'src> for DefinedFunction {
    type InputType = InputFunction<'src>;
    fn get_input_index(
        &self,
    ) -> OutputMapType<crate::index::Id<<Self::InputType as Indexed>::StaticTypeTagForIndex>> {
        if matches!(self.kind, DefinedFunctionKind::Trampoline { .. }) {
            // Import stubs is not a real function in input module.
            return OutputMapType::OutputHasInput(self.input_func_id);
        }
        OutputMapType::BidirectionalMap(self.input_func_id)
    }
}

impl<'src> crate::index::OutputType<'src> for ImportedFunction<'src> {
    type InputType = InputFunction<'src>;
    fn get_input_index(
        &self,
    ) -> OutputMapType<crate::index::Id<<Self::InputType as Indexed>::StaticTypeTagForIndex>> {
        OutputMapType::BidirectionalMap(self.input_func_id())
    }
}

pub type OutputGlobalId = crate::index::Id<DefinedGlobal<'static>>;

impl Indexed for DefinedGlobal<'_> {
    type StaticTypeTagForIndex = DefinedGlobal<'static>;
    type IndexType = crate::index::Id<Self::StaticTypeTagForIndex>;
}

impl<'src> crate::index::Defined<'src> for DefinedGlobal<'src> {
    type Import = crate::emit::globals::GlobalImport<'src>;
}
impl<'src> crate::index::OutputType<'src> for DefinedGlobal<'src> {
    type InputType = wasmparser::Global<'src>;

    fn get_input_index(
        &self,
    ) -> OutputMapType<crate::index::Id<<Self::InputType as Indexed>::StaticTypeTagForIndex>> {
        match self {
            DefinedGlobal::PlainCopy {
                input_global_id, ..
            } => OutputMapType::BidirectionalMap(*input_global_id),
            DefinedGlobal::WithConstructor(_) => OutputMapType::None,
        }
    }
}

impl<'src> crate::index::OutputType<'src> for GlobalImport<'src> {
    type InputType = wasmparser::Global<'src>;
    fn get_input_index(
        &self,
    ) -> OutputMapType<crate::index::Id<<Self::InputType as Indexed>::StaticTypeTagForIndex>> {
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
