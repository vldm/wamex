use anyhow::Result;

use super::{Blacklist, blacklist::IsSet};
use crate::{
    emit::{
        modify::{Rewrite, create_fixup_for_entity, cursor::Cursor},
        plan::{AnyEntity, SourceInfo},
        relocation::resolver::OutputEntitiesResolver,
    },
    index::Temp,
    linkage::reloc::EntityRelocationEntry,
    typed::{
        EntityBody, FileId, ImportOrDefined,
        snapshot::{EntitiesSnapshot, FlatEntityRef},
    },
};

mod code;
mod data;

pub use code::CodeAbsToGot;
pub use data::DataAbsToGot;

pub type Artifact = (
    Vec<<CodeAbsToGot<fn(FlatEntityRef) -> bool> as FixupFromRelocs<'static>>::ExtraData>,
    Vec<<DataAbsToGot<fn(FlatEntityRef) -> bool> as FixupFromRelocs<'static>>::ExtraData>,
);
///
/// Convert Absolute addresses to GOT entries.
///
pub struct AbsToGot<F>
where
    Blacklist<F>: IsSet,
{
    code_modifier: Option<CodeAbsToGot<F>>,
    data_modifier: Option<DataAbsToGot<F>>,
}
impl<F> AbsToGot<F>
where
    Blacklist<F>: IsSet,
{
    pub fn setup(
        shared: Blacklist<F>,
        module: &mut crate::typed::ModuleBuilder<'_>,
    ) -> Result<Self, anyhow::Error>
    where
        Self: Sized,
    {
        let code_modifier = CodeAbsToGot::setup(shared.clone(), module)?;
        let data_modifier = DataAbsToGot::setup(shared.clone(), module)?;
        Ok(Self {
            code_modifier,
            data_modifier,
        })
    }

    pub fn modify_entity(
        &self,
        source_info: SourceInfo<'_>,
        entity: AnyEntity<'_, '_>,
    ) -> Result<Artifact, anyhow::Error> {
        macro_rules! handle_defined {
            (@$v:ident $def:ident, $new_ref:expr, $modifier:expr) => {
                match ($def.as_deref_mut(), $modifier) {
                    (ImportOrDefined::Defined(def), Some(modifier)) => {
                        let EntityBody::Copied(body) = &mut def.body else {
                            panic!(
                                "Trying to modify already modified entity {id} has body {:#?}",
                                def.body,
                                id = source_info.source_entity
                            );
                        };
                        assert!(body.fixups.is_empty());
                        let res = create_fixup_for_entity(
                            body,
                            $new_ref,
                            (source_info.input_file, source_info.snapshot),
                            source_info.entity_relocs,
                            modifier,
                        )?;
                        handle_defined!(@$v res)
                    }
                    _ => Ok((Vec::new(), Vec::new())),
                }
            };
            (@code $res: expr) => {
                Ok(($res, Vec::new()))
            };

            (@data $res: expr) => {
                Ok((Vec::new(), $res))
            };
        }

        match entity {
            AnyEntity::Function {
                new_ref,
                mut entity,
            } => {
                handle_defined!(@code entity, new_ref, self.code_modifier.as_ref())
            }
            AnyEntity::DataSymbol {
                mut entity,
                new_ref,
            } => {
                handle_defined!(@data entity, new_ref, self.data_modifier.as_ref())
            }
            AnyEntity::Global { .. }
            | AnyEntity::Table { .. }
            | AnyEntity::Memory { .. }
            | AnyEntity::Tag { .. } => Ok((Vec::new(), Vec::new())),
        }
    }

    pub fn finish(
        &self,
        module: &mut crate::typed::Module<'_>,
        resolver: &mut OutputEntitiesResolver,
        (aggregated_code_data, aggregated_data_data): Artifact,
    ) -> Result<(), anyhow::Error> {
        if let Some(modifier) = &self.code_modifier {
            modifier.finish(module, resolver, aggregated_code_data)?;
        }
        if let Some(modifier) = &self.data_modifier {
            modifier.finish(module, resolver, aggregated_data_data)?;
        }
        Ok(())
    }
}

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
