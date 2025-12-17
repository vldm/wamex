use super::DefinedFunction;
use crate::{
    emit::{
        DefinedFunctionKind, ImportedFunction,
        globals::{DefinedGlobal, GlobalImport},
    },
    index::{OutputMapType, PrimaryKey},
    read::code::InputFunction,
};

// impl Indexed for DefinedFunction {
//     type StaticTypeTagForIndex = Self;
//     type IndexType = crate::index::Id<Self::StaticTypeTagForIndex>;
// }
// impl Indexed for ImportedFunction<'_> {
//     type StaticTypeTagForIndex = ImportedFunction<'static>;
//     type IndexType = crate::index::Id<Self::StaticTypeTagForIndex>;
// }
impl_entity_index! {
    OutputFuncId(DefinedFunction) => "output_function";
    ImportFnId(for<'a> ImportedFunction<'a>);
    OutputGlobalId(for<'a> DefinedGlobal<'a>) => "output_global";
    GlobalImportId(for<'a> GlobalImport<'a>);
}

impl<'src> crate::index::Defined<'src> for DefinedFunction {
    type Import = ImportedFunction<'src>;
}
impl<'src> crate::index::OutputType<'src> for DefinedFunction {
    type InputType = InputFunction<'src>;
    fn get_input_index(&self) -> OutputMapType<<Self::InputType as PrimaryKey>::EntityType> {
        if matches!(self.kind, DefinedFunctionKind::Trampoline { .. }) {
            // Import stubs is not a real function in input module.
            return OutputMapType::OutputHasInput(self.input_func_id);
        }
        OutputMapType::BidirectionalMap(self.input_func_id)
    }
}

impl<'src> crate::index::OutputType<'src> for ImportedFunction<'src> {
    type InputType = InputFunction<'src>;
    fn get_input_index(&self) -> OutputMapType<<Self::InputType as PrimaryKey>::EntityType> {
        OutputMapType::BidirectionalMap(self.input_func_id())
    }
}

impl<'src> crate::index::Defined<'src> for DefinedGlobal<'src> {
    type Import = crate::emit::globals::GlobalImport<'src>;
}
impl<'src> crate::index::OutputType<'src> for DefinedGlobal<'src> {
    type InputType = wasmparser::Global<'src>;

    fn get_input_index(&self) -> OutputMapType<<Self::InputType as PrimaryKey>::EntityType> {
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
    fn get_input_index(&self) -> OutputMapType<<Self::InputType as PrimaryKey>::EntityType> {
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
