use crate::{
    SVec,
    index::Temp,
    layouts::{DataSymbolRef, ImportedDataChunk},
    linkage::reloc::EntityRelocationEntry,
    typed::{
        DefinedDataChunk, DefinedFunction, DefinedGlobal, DefinedMemory, DefinedTable, DefinedTag,
        EntityKind, FileId, FunctionRef, GlobalRef, ImportOrDefined, ImportedFunction,
        ImportedGlobal, ImportedMemory, ImportedTable, ImportedTag, MemoryRef, TableRef, TagRef,
        snapshot::{EntitiesSnapshot, FlatEntityRef},
    },
};

pub type OutputId = String;

/// Externdable plan for emitting module.
///
/// Consist of following steps:
/// 1. Setup - create new entities related to this plan. E.g. Init GOT/Memory, etc.
/// 2. Copy entities from input modules - During this step one can perform some transformations
///    (like replacing addressing mode to got-relative). Thats why `copy` is splitted into two
///    methods `entities_to_copy` and `transform_copied`. Thats allow future parallelization, and more efficient transformation.
/// 3. Finalization - allows to perform some finalization steps before module emitted. Also consist of two steps:
///    1. `before_finish` - allows to look on aggregated artifacts and perform last entities additions.
///    2. `finish` - work with finalized module, and only can modify existing entities (e.g. fill function bodies, etc.).
///       This step allow to add some linkage to `OutputEntitiesResolver` that would be used when processing relocations.
pub trait OutputPlan<'src> {
    type Artifacts: Merge;
    type ExtraData;
    /// Perform some module-specific setup.
    fn setup(&mut self, module: &mut crate::typed::ModuleBuilder<'src>) -> anyhow::Result<()>;
    /// List of entities that need to be copied from input modules to one that we are going to emit.
    fn entities_to_copy(&self) -> impl Iterator<Item = (FlatEntityRef, Self::ExtraData)>;

    /// For each copied entity - we can perform some transformation.
    fn transform(
        &self,
        source_info: SourceInfo<'_>,
        entity: AnyEntity<'src, '_>,
        extra_data: Self::ExtraData,
    ) -> anyhow::Result<Self::Artifacts>;

    /// Allows to perform some finalization steps before module is emitted.
    fn before_lock(
        &mut self,
        module: &mut crate::typed::ModuleBuilder<'src>,
        aggregated_data: &Self::Artifacts,
    ) -> anyhow::Result<()>;

    fn finish(
        self,
        module: &mut crate::typed::Module<'src>,
        aggregated_data: Self::Artifacts,
        resolver: &mut crate::emit::relocation::resolver::OutputEntitiesResolver,
    ) -> anyhow::Result<()>;
}

#[derive(Debug, Clone, Copy)]
pub struct SourceInfo<'any> {
    pub source_entity: EntityKind,
    pub source_flat: FlatEntityRef,
    pub input_file: FileId,
    // snapshot of current file
    pub snapshot: &'any EntitiesSnapshot,
    // source file relocs
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
