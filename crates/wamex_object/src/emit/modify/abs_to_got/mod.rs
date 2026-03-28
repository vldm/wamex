use anyhow::Result;

use crate::{
    emit::{
        modify::{Rewrite, cursor::Cursor},
        plan::OutputModuleCopyPlan,
        relocation::resolver::OutputEntitiesResolver,
    },
    index::Temp,
    linkage::reloc::EntityRelocationEntry,
    typed::{FileId, snapshot::EntitiesSnapshot},
};

mod code;
mod data;

pub use code::CodeAbsToGot;
pub use data::DataAbsToGot;
/// Implementation of modification routine.
/// Allows adding patches to the original code based on the original relocation entries.
///
/// The main purpose of this patches is to replace some symbol references with other types.
/// e.g. converting absoulte address to got-relative, or replacing a function call with an indirect call.
pub trait FixupFromRelocs<'src> {
    type EntityRef: Copy;
    type SetupData;
    type ExtraData;

    /// Setup the handler to work with new module.
    ///
    /// This function is called once per module,
    /// `SetupData` is shared part that passed to all modules,
    /// it might be some non-module related state - like blacklist of symbols, or some other global configuration.
    ///
    fn setup(
        shared: Self::SetupData,
        plan: &OutputModuleCopyPlan,
        module: &mut crate::typed::ModuleBuilder<'src>,
    ) -> Result<Option<Self>>
    where
        Self: Sized;

    // TODO: Suport modification that need two or more relocs
    // e.g., for got-relative addressing
    /// Create a modification entry based on the original relocation entry and the current state of the code.
    /// Return None if no modification is needed for this entry.
    fn create_entry(
        &self,
        entity: Temp<Self::EntityRef>,
        buffer: Cursor<'src>,
        // Usefull when we need to manually resolve relocs
        input_file: (FileId, &EntitiesSnapshot),
        entry: EntityRelocationEntry,
    ) -> Result<Option<(Rewrite, Self::ExtraData)>>;

    /// Handle finalization process.
    ///
    /// One can convert accumulated during `create_entry` indexes using `resolver`, and modify
    /// entities that was created during setup.
    fn finish(
        &self,
        _module: &mut crate::typed::Module<'src>,
        _resolver: &OutputEntitiesResolver,
        _agregated_data: Vec<Self::ExtraData>,
    ) -> Result<()> {
        Ok(())
    }
}
