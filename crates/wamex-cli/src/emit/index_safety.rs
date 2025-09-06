use super::{DefinedFunction, Indexed};
use crate::{
    emit::{
        globals::{DefinedGlobal, GlobalImport},
        ImportedFunction,
    },
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
    ) -> Option<crate::index::Id<<Self::InputType as Indexed>::StaticTypeTagForIndex>> {
        Some(self.input_func_id)
    }
}

impl<'src> crate::index::OutputType<'src> for ImportedFunction<'src> {
    type InputType = InputFunction<'src>;
    fn get_input_index(
        &self,
    ) -> Option<crate::index::Id<<Self::InputType as Indexed>::StaticTypeTagForIndex>> {
        Some(self.input_func_id())
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
    ) -> Option<crate::index::Id<<Self::InputType as Indexed>::StaticTypeTagForIndex>> {
        match self {
            DefinedGlobal::PlainCopy {
                input_global_id, ..
            } => Some(*input_global_id),
            DefinedGlobal::WithConstructor(_) => None,
        }
    }
}

impl<'src> crate::index::OutputType<'src> for GlobalImport<'src> {
    type InputType = wasmparser::Global<'src>;
    fn get_input_index(
        &self,
    ) -> Option<crate::index::Id<<Self::InputType as Indexed>::StaticTypeTagForIndex>> {
        match self {
            GlobalImport::Existing {
                input_global_id, ..
            } => Some(*input_global_id),
            GlobalImport::New {
                input_global_id, ..
            } => *input_global_id,
        }
    }
}
