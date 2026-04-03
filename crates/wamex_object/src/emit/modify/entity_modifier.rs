use super::abs_to_got::FixupFromRelocs;
use crate::{
    SVec,
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
    layouts::DataSymbolRef,
    linkage::reloc::EntityRelocationEntry,
    typed::{
        DefinedDataChunk, DefinedFunction, DefinedGlobal, DefinedMemory, DefinedTable, DefinedTag,
        EntityBody, EntityKind, FileId, FunctionRef, GlobalRef, ImportOrDefined, ImportedDataChunk,
        ImportedFunction, ImportedGlobal, ImportedMemory, ImportedTable, ImportedTag, MemoryRef,
        TableRef, TagRef, snapshot::EntitiesSnapshot,
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

    fn finish(
        &self,
        module: &mut crate::typed::Module<'src>,
        resolver: &mut OutputEntitiesResolver,
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

    /// Last chance to add some entities to module
    fn before_lock(
        &mut self,
        _module: &mut crate::typed::ModuleBuilder<'src>,
        _aggregated_data: &Self::ExtraData,
    ) -> Result<(), anyhow::Error> {
        Ok(())
    }

    /// Now can handle index conversion.
    fn finish(
        &self,
        module: &mut crate::typed::Module<'src>,
        resolver: &mut OutputEntitiesResolver,
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
        _resolver: &mut OutputEntitiesResolver,
        _aggregated_data: Self::ExtraData,
    ) -> Result<(), anyhow::Error> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SourceInfo<'any> {
    pub source_entity: EntityKind,
    pub input_file: FileId,
    pub snapshot: &'any EntitiesSnapshot,
    pub entity_relocs: &'any [EntityRelocationEntry],
}

//TODO: Conception of providing new_ref and entity &mut not working correctly if
//      one modify type of entity from imports -> defined, etc.
#[derive(Debug)]
pub enum AnyEntity<'src, 'any> {
    Global {
        new_ref: Temp<GlobalRef>,
        entity: ImportOrDefined<&'any mut ImportedGlobal<'src>, &'any mut DefinedGlobal<'src>>,
    },
    Function {
        new_ref: Temp<FunctionRef>,
        entity: ImportOrDefined<&'any mut ImportedFunction<'src>, &'any mut DefinedFunction<'src>>,
    },
    DataSymbol {
        new_ref: Temp<DataSymbolRef>,
        entity:
            ImportOrDefined<&'any mut ImportedDataChunk<'src>, &'any mut DefinedDataChunk<'src>>,
    },
    Table {
        new_ref: Temp<TableRef>,
        entity: ImportOrDefined<&'any mut ImportedTable<'src>, &'any mut DefinedTable<'src>>,
    },
    Memory {
        new_ref: Temp<MemoryRef>,
        entity: ImportOrDefined<&'any mut ImportedMemory<'src>, &'any mut DefinedMemory<'src>>,
    },
    Tag {
        new_ref: Temp<TagRef>,
        entity: ImportOrDefined<&'any mut ImportedTag<'src>, &'any mut DefinedTag<'src>>,
    },
}
impl<'src, 'any> AnyEntity<'src, 'any> {
    pub fn reborrow<'other>(&'other mut self) -> AnyEntity<'src, 'other>
    where
        'any: 'other,
    {
        match *self {
            AnyEntity::Global {
                new_ref,
                ref mut entity,
            } => AnyEntity::Global {
                new_ref,
                entity: entity.as_deref_mut(),
            },
            AnyEntity::Function {
                new_ref,
                ref mut entity,
            } => AnyEntity::Function {
                new_ref,
                entity: entity.as_deref_mut(),
            },
            AnyEntity::DataSymbol {
                new_ref,
                ref mut entity,
            } => AnyEntity::DataSymbol {
                new_ref,
                entity: entity.as_deref_mut(),
            },
            AnyEntity::Table {
                new_ref,
                ref mut entity,
            } => AnyEntity::Table {
                new_ref,
                entity: entity.as_deref_mut(),
            },
            AnyEntity::Memory {
                new_ref,
                ref mut entity,
            } => AnyEntity::Memory {
                new_ref,
                entity: entity.as_deref_mut(),
            },
            AnyEntity::Tag {
                new_ref,
                ref mut entity,
            } => AnyEntity::Tag {
                new_ref,
                entity: entity.as_deref_mut(),
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

impl<T, const N: usize> Merge for SVec<T, N>
where
    [T; N]: smallvec::Array,
{
    fn new() -> Self {
        SVec::new()
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
                resolver: &mut OutputEntitiesResolver,
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

impl<'src, M> EntityModifier<'src> for Option<M>
where
    M: EntityModifier<'src>,
{
    type SetupData = Option<M::SetupData>;
    type ExtraData = M::ExtraData;

    fn setup(
        shared: Self::SetupData,
        plan: &OutputModuleCopyPlan,
        module: &mut crate::typed::ModuleBuilder<'src>,
    ) -> Result<Self, anyhow::Error>
    where
        Self: Sized,
    {
        if let Some(shared) = shared {
            Ok(Some(M::setup(shared, plan, module)?))
        } else {
            Ok(None)
        }
    }

    fn modify_entity(
        &self,
        source_info: SourceInfo<'_>,
        entity: AnyEntity<'src, '_>,
    ) -> Result<Self::ExtraData, anyhow::Error> {
        Ok(if let Some(modifier) = self {
            modifier.modify_entity(source_info, entity)?
        } else {
            M::ExtraData::new()
        })
    }

    fn finish(
        &self,
        module: &mut crate::typed::Module<'src>,
        resolver: &mut OutputEntitiesResolver,
        aggregated_data: Self::ExtraData,
    ) -> Result<(), anyhow::Error> {
        if let Some(modifier) = self {
            modifier.finish(module, resolver, aggregated_data)?;
        }
        Ok(())
    }
}
