use super::DefinedFunction;
use crate::{
    emit::{
        DefinedFunctionKind, ImportedFunction,
        globals::{DefinedGlobal, GlobalImport},
    },
    index::PrimaryKey,
    read::{Defined, OutputMapType, OutputType, code::InputFunction},
};

impl_entity_index! {
    #[display = "output_function"]
    pub struct OutputFuncId(DefinedFunction);
    pub struct ImportFnId(for<'a> ImportedFunction<'a>);
    #[display = "output_global"]
    pub struct OutputGlobalId(for<'a> DefinedGlobal<'a>);
    pub struct GlobalImportId(for<'a> GlobalImport<'a>);
}

impl<'src> Defined<'src> for DefinedFunction {
    type Import = ImportedFunction<'src>;
}
impl<'src> OutputType<'src> for DefinedFunction {
    type InputType = InputFunction<'src>;
    fn get_input_index(&self) -> OutputMapType<<Self::InputType as PrimaryKey>::EntityType> {
        if matches!(self.kind, DefinedFunctionKind::Trampoline { .. }) {
            // Import stubs is not a real function in input module.
            return OutputMapType::OutputHasInput(self.input_func_id);
        }
        OutputMapType::BidirectionalMap(self.input_func_id)
    }
}

impl<'src> OutputType<'src> for ImportedFunction<'src> {
    type InputType = InputFunction<'src>;
    fn get_input_index(&self) -> OutputMapType<<Self::InputType as PrimaryKey>::EntityType> {
        OutputMapType::BidirectionalMap(self.input_func_id())
    }
}

impl<'src> Defined<'src> for DefinedGlobal<'src> {
    type Import = GlobalImport<'src>;
}
impl<'src> OutputType<'src> for DefinedGlobal<'src> {
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

impl<'src> OutputType<'src> for GlobalImport<'src> {
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
