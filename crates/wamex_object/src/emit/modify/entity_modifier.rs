use super::abs_to_got::FixupFromRelocs;
use crate::{
    emit::{
        modify::{
            Blacklist,
            abs_to_got::{CodeAbsToGot, DataAbsToGot},
            blacklist::IsSet,
            create_fixup_for_entity,
        },
        plan::OutputModuleCopyPlan,
        relocation::resolver::OutputEntitiesResolver,
    },
    index::Temp,
    linkage::reloc::EntityRelocationEntry,
    typed::{
        DefinedDataChunk, DefinedFunction, DefinedGlobal, DefinedMemory, DefinedTable, DefinedTag,
        EntityBody, FileId, FunctionRef, GlobalRef, ImportOrDefined, ImportedDataChunk,
        ImportedFunction, ImportedGlobal, ImportedMemory, ImportedTable, ImportedTag, MemoryRef,
        TableRef, TagRef,
        data::DataSymbolRef,
        snapshot::{EntitiesSnapshot, FlatEntityRef},
    },
};

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

impl<'src, F> EntityModifier<'src> for AbsToGot<F>
where
    Blacklist<F>: IsSet,
{
    type SetupData = Blacklist<F>;
    type ExtraData = (
        Vec<<CodeAbsToGot<F> as FixupFromRelocs<'src>>::ExtraData>,
        Vec<<DataAbsToGot<F> as FixupFromRelocs<'src>>::ExtraData>,
    );

    fn setup(
        shared: Self::SetupData,
        plan: &OutputModuleCopyPlan,
        module: &mut crate::typed::ModuleBuilder<'src>,
    ) -> Result<Self, anyhow::Error>
    where
        Self: Sized,
    {
        let code_modifier = CodeAbsToGot::setup(shared.clone(), plan, module)?;
        let data_modifier = DataAbsToGot::setup(shared.clone(), plan, module)?;
        Ok(Self {
            code_modifier,
            data_modifier,
        })
    }

    fn modify_entity(
        &self,
        source_info: SourceInfo<'_>,
        entity: AnyEntity<'src, '_>,
    ) -> Result<Self::ExtraData, anyhow::Error> {
        macro_rules! handle_defined {
            (@$v:ident $def:ident, $new_ref:expr, $modifier:expr) => {
                match ($def.as_mut(), $modifier) {
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
            AnyEntity::Function { new_ref, entity } => {
                handle_defined!(@code entity, new_ref, self.code_modifier.as_ref())
            }
            AnyEntity::DataSymbol { entity, new_ref } => {
                handle_defined!(@data entity, new_ref, self.data_modifier.as_ref())
            }
            AnyEntity::Global { .. }
            | AnyEntity::Table { .. }
            | AnyEntity::Memory { .. }
            | AnyEntity::Tag { .. } => Ok((Vec::new(), Vec::new())),
        }
    }

    fn finish(
        &self,
        module: &mut crate::typed::Module<'src>,
        resolver: &OutputEntitiesResolver,
        (aggregated_code_data, aggregated_data_data): Self::ExtraData,
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

pub trait EntityModifier<'src> {
    type SetupData;
    type ExtraData: Merge;

    fn setup(
        shared: Self::SetupData,
        plan: &OutputModuleCopyPlan,
        module: &mut crate::typed::ModuleBuilder<'src>,
    ) -> Result<Self, anyhow::Error>
    where
        Self: Sized;

    fn modify_entity(
        &self,
        source_info: SourceInfo<'_>,
        entity: AnyEntity<'src, '_>,
    ) -> Result<Self::ExtraData, anyhow::Error>;

    fn finish(
        &self,
        module: &mut crate::typed::Module<'src>,
        resolver: &OutputEntitiesResolver,
        aggregated_data: Self::ExtraData,
    ) -> Result<(), anyhow::Error>;
}

pub struct NoModification;
impl EntityModifier<'_> for NoModification {
    type SetupData = ();
    type ExtraData = ();

    fn setup(
        _shared: Self::SetupData,
        _plan: &OutputModuleCopyPlan,
        _module: &mut crate::typed::ModuleBuilder<'_>,
    ) -> Result<Self, anyhow::Error>
    where
        Self: Sized,
    {
        Ok(Self)
    }

    fn modify_entity(
        &self,
        _source_info: SourceInfo<'_>,
        _entity: AnyEntity<'_, '_>,
    ) -> Result<Self::ExtraData, anyhow::Error> {
        Ok(())
    }

    fn finish(
        &self,
        _module: &mut crate::typed::Module<'_>,
        _resolver: &OutputEntitiesResolver,
        _aggregated_data: Self::ExtraData,
    ) -> Result<(), anyhow::Error> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SourceInfo<'any> {
    pub source_entity: FlatEntityRef,
    pub input_file: FileId,
    pub snapshot: &'any EntitiesSnapshot,
    pub entity_relocs: &'any [EntityRelocationEntry],
}

#[derive(Debug)]
pub enum AnyEntity<'src, 'any> {
    Global {
        new_ref: Temp<GlobalRef>,
        entity: &'any mut ImportOrDefined<ImportedGlobal<'src>, DefinedGlobal<'src>>,
    },
    Function {
        new_ref: Temp<FunctionRef>,
        entity: &'any mut ImportOrDefined<ImportedFunction<'src>, DefinedFunction<'src>>,
    },
    DataSymbol {
        new_ref: Temp<DataSymbolRef>,
        entity: &'any mut ImportOrDefined<ImportedDataChunk<'src>, DefinedDataChunk<'src>>,
    },
    Table {
        new_ref: Temp<TableRef>,
        entity: &'any mut ImportOrDefined<ImportedTable<'src>, DefinedTable<'src>>,
    },
    Memory {
        new_ref: Temp<MemoryRef>,
        entity: &'any mut ImportOrDefined<ImportedMemory<'src>, DefinedMemory<'src>>,
    },
    Tag {
        new_ref: Temp<TagRef>,
        entity: &'any mut ImportOrDefined<ImportedTag<'src>, DefinedTag<'src>>,
    },
}
impl<'src, 'any> AnyEntity<'src, 'any> {
    pub fn reborrow<'other>(&'other mut self) -> AnyEntity<'src, 'other>
    where
        'any: 'other,
    {
        match self {
            AnyEntity::Global { new_ref, entity } => AnyEntity::Global {
                new_ref: *new_ref,
                entity,
            },
            AnyEntity::Function { new_ref, entity } => AnyEntity::Function {
                new_ref: *new_ref,
                entity,
            },
            AnyEntity::DataSymbol { new_ref, entity } => AnyEntity::DataSymbol {
                new_ref: *new_ref,
                entity,
            },
            AnyEntity::Table { new_ref, entity } => AnyEntity::Table {
                new_ref: *new_ref,
                entity,
            },
            AnyEntity::Memory { new_ref, entity } => AnyEntity::Memory {
                new_ref: *new_ref,
                entity,
            },
            AnyEntity::Tag { new_ref, entity } => AnyEntity::Tag {
                new_ref: *new_ref,
                entity,
            },
        }
    }
}

pub trait Merge {
    fn new() -> Self;
    fn merge(&mut self, other: Self);
}

impl<T> Merge for Vec<T> {
    fn new() -> Self {
        Vec::new()
    }
    fn merge(&mut self, other: Self) {
        self.extend(other);
    }
}
impl Merge for () {
    fn new() -> Self {}
    fn merge(&mut self, _other: Self) {}
}

//
// Tuple implementation.
//
macro_rules! impl_merge_for_tuple {
    ($($idx:tt => $T:ident),+ $(,)?) => {
        impl<$($T: Merge),+> Merge for ($($T,)+) {
            fn new() -> Self {
                ($($T::new(),)+)
            }
            fn merge(&mut self, other: Self) {
                $(self.$idx.merge(other.$idx);)+
            }
        }
    };
}
impl_merge_for_tuple!(0 => T0);
impl_merge_for_tuple!(0 => T0, 1 => T1);
impl_merge_for_tuple!(0 => T0, 1 => T1, 2 => T2);
impl_merge_for_tuple!(0 => T0, 1 => T1, 2 => T2, 3 => T3);
impl_merge_for_tuple!(0 => T0, 1 => T1, 2 => T2, 3 => T3, 4 => T4);
impl_merge_for_tuple!(0 => T0, 1 => T1, 2 => T2, 3 => T3, 4 => T4, 5 => T5);

macro_rules! impl_modifier_for_tuple {
    ($($idx:tt => $T:ident),+ $(,)?) => {
        impl<'src, $( $T, )+> EntityModifier<'src> for ( $( $T, )+ )
        where
            $( $T: EntityModifier<'src>, )+
        {
            type ExtraData = (
                $( < $T as EntityModifier<'src>>::ExtraData, )+
            );
            type SetupData = (
                $( < $T as EntityModifier<'src>>::SetupData, )+
            );

            fn setup(
                shared: Self::SetupData,
                plan: &OutputModuleCopyPlan,
                module: &mut crate::typed::ModuleBuilder<'src>,
            ) -> Result<Self, anyhow::Error>
            where
                Self: Sized,
            {
                Ok(($(
                   $T::setup(shared.$idx, plan, module)?,
                )+))
            }
            fn modify_entity(
                &self,
                source_info: SourceInfo<'_>,
                mut entity: AnyEntity<'src, '_>,
            ) -> Result<Self::ExtraData, anyhow::Error> {

                Ok(($(
                    self.$idx.modify_entity(source_info, entity.reborrow())?,
                )+))
            }
            fn finish(
                &self,
                module: &mut crate::typed::Module<'src>,
                resolver: &OutputEntitiesResolver,
                agregated_data: Self::ExtraData,
            ) -> Result<(), anyhow::Error> {
                $(self.$idx.finish(module, resolver, agregated_data.$idx)?;)+
                Ok(())
            }
        }
    };
}

impl_modifier_for_tuple!(0 => T0);
impl_modifier_for_tuple!(0 => T0, 1 => T1);
impl_modifier_for_tuple!(0 => T0, 1 => T1, 2 => T2);
impl_modifier_for_tuple!(0 => T0, 1 => T1, 2 => T2, 3 => T3);
impl_modifier_for_tuple!(0 => T0, 1 => T1, 2 => T2, 3 => T3, 4 => T4);
impl_modifier_for_tuple!(0 => T0, 1 => T1, 2 => T2, 3 => T3, 4 => T4, 5 => T5);
