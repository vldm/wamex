use crate::emit::ImportedFunction;
use crate::read::code::InputFunction;

use super::DefinedFunction;
use super::Indexed;

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
    ) -> crate::index::Id<<Self::InputType as Indexed>::StaticTypeTagForIndex> {
        self.input_func_id
    }
}

impl<'src> crate::index::OutputType<'src> for ImportedFunction<'src> {
    type InputType = InputFunction<'src>;
    fn get_input_index(
        &self,
    ) -> crate::index::Id<<Self::InputType as Indexed>::StaticTypeTagForIndex> {
        self.input_func_id()
    }
}
