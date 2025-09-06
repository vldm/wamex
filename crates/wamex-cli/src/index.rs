use std::{
    fmt::Debug,
    hash::Hash,
    marker::PhantomData,
    ops::{Deref, Index},
    str::FromStr,
};

use vec_map::VecMap;
use wasmparser::{Data, Element, Export, FuncType, Global, Import, MemoryType, Table, TagType};

use crate::read::{
    code::{FunctionWithBody, InputFunction},
    linking::section::DataInSegment,
};

pub type AnySymbolId = usize;
pub type SectionId = usize;
pub type OutputSymbolDataId = usize;

pub type FuncTypeId = Id<FuncType>;
pub type InputFuncId = Id<InputFunction<'static>>;
pub type DefinedFuncId = Id<FunctionWithBody<'static>>;
pub type TableId = Id<Table<'static>>;
pub type ImportId = Id<Import<'static>>;
pub type ExportId = Id<Export<'static>>;
pub type MemoryId = Id<MemoryType>;
pub type InputGlobalId = Id<Global<'static>>;
pub type ElementId = Id<Element<'static>>;
pub type DataSegmentId = Id<Data<'static>>;
pub type DataSymbolId = Id<DataInSegment<'static>>;
pub type DataId = (DataSegmentId, DataSymbolId);
pub type TagId = Id<TagType>;
// TODO: Maybe replace Vecs with id_arena?
// Currently the only difference is that we also use

type PhantomCovariant<T> = PhantomData<fn() -> T>;
pub struct Id<TypeTag> {
    id: usize,
    _ty: PhantomCovariant<TypeTag>,
}
// TODO: Remove this functions later
impl<TypeTag> Id<TypeTag> {
    pub fn from_index<T>(id: T) -> Self
    where
        T: TryInto<usize>,
    {
        Id {
            id: id.try_into().ok().expect("Invalid ID"),
            _ty: PhantomData,
        }
    }
    pub fn as_raw_index(&self) -> usize {
        self.id
    }
    pub fn next(&self) -> Self {
        Id {
            id: self.id + 1,
            _ty: PhantomData,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct IdMap<Idx, Res> {
    vecmap: VecMap<Res>,
    _res: PhantomCovariant<Idx>,
}
impl<Idx, Res> Debug for IdMap<Idx, Res>
where
    Res: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.vecmap.fmt(f)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct IdVec<
    Type,
    // Allow customization of index type, in case where you have sub-collection for some data Type
    Idx = <Type as Indexed>::IndexType,
> {
    types: Vec<Type>,
    _idx: PhantomCovariant<Idx>,
}
impl<Type, Idx> Debug for IdVec<Type, Idx>
where
    Type: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.types.fmt(f)
    }
}

pub trait Indexed {
    type StaticTypeTagForIndex: 'static;
    type IndexType: 'static;
}

/// Store additional information about section, to apply relocation
#[derive(Debug)]
pub struct IndexedSection<T> {
    pub starting_offset: usize,
    pub section_index: usize,
    pub section_payload: T,
}

impl<T> Deref for IndexedSection<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.section_payload
    }
}
impl<T: Default> Default for IndexedSection<T> {
    fn default() -> Self {
        Self {
            section_payload: T::default(),
            starting_offset: 0,
            section_index: usize::MAX,
        }
    }
}

impl<Type, Idx> Default for IdVec<Type, Idx> {
    fn default() -> Self {
        IdVec {
            types: Vec::new(),
            _idx: PhantomData,
        }
    }
}

impl<Type, Result> Default for IdMap<Id<Type>, Result> {
    fn default() -> Self {
        IdMap {
            vecmap: VecMap::new(),
            _res: PhantomData,
        }
    }
}

impl<Type, Idx> FromIterator<Type> for IdVec<Type, Idx> {
    fn from_iter<T: IntoIterator<Item = Type>>(iter: T) -> Self {
        let vec = Vec::from_iter(iter);
        IdVec {
            types: vec,
            _idx: PhantomData,
        }
    }
}
impl<Type, Res> FromIterator<(Id<Type>, Res)> for IdMap<Id<Type>, Res> {
    fn from_iter<T: IntoIterator<Item = (Id<Type>, Res)>>(iter: T) -> Self {
        let vecmap = VecMap::from_iter(iter.into_iter().map(|(id, res)| (id.id, res)));
        IdMap {
            vecmap,
            _res: PhantomData,
        }
    }
}

impl<T, Idx> IdVec<T, Id<Idx>> {
    pub fn new() -> Self {
        IdVec {
            types: Vec::new(),
            _idx: PhantomData,
        }
    }

    pub fn push(&mut self, item: T) -> Id<Idx> {
        let id = self.types.len();
        self.types.push(item);
        Id {
            id,
            _ty: PhantomData,
        }
    }

    pub fn get(&self, id: Id<Idx>) -> Option<&T> {
        self.types.get(id.id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (Id<Idx>, &T)> {
        self.types.iter().enumerate().map(|(id, ty)| {
            (
                Id {
                    id,
                    _ty: PhantomData,
                },
                ty,
            )
        })
    }
    pub fn as_slice(&self) -> &[T] {
        &self.types
    }
    pub fn len(&self) -> usize {
        self.types.len()
    }
    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }
}

impl<Type, Res> IdMap<Id<Type>, Res> {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            vecmap: VecMap::new(),
            _res: PhantomData,
        }
    }
    pub fn insert(&mut self, id: Id<Type>, res: Res) {
        self.vecmap.insert(id.id, res);
    }
    pub fn get(&self, id: Id<Type>) -> Option<&Res> {
        self.vecmap.get(id.id)
    }
    pub fn iter(&self) -> impl Iterator<Item = (Id<Type>, &Res)> {
        self.vecmap.iter().map(|(id, res)| {
            (
                Id {
                    id,
                    _ty: PhantomData,
                },
                res,
            )
        })
    }
    pub fn entry(&mut self, id: Id<Type>) -> vec_map::Entry<'_, Res> {
        self.vecmap.entry(id.id)
    }
}

impl<T: Indexed, Idx> Index<Id<Idx>> for IdVec<T, Id<Idx>> {
    type Output = T;

    fn index(&self, id: Id<Idx>) -> &Self::Output {
        &self.types[id.id]
    }
}
impl<T: Indexed, Res> Index<Id<T::StaticTypeTagForIndex>> for IdMap<Id<T>, Res> {
    type Output = Res;

    fn index(&self, id: Id<T::StaticTypeTagForIndex>) -> &Self::Output {
        self.vecmap.index(id.id)
    }
}

impl<Type> Hash for Id<Type> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl<Type> PartialEq for Id<Type> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl<Type> Eq for Id<Type> {}

impl<Type> Clone for Id<Type> {
    fn clone(&self) -> Self {
        Id {
            id: self.id,
            _ty: PhantomData,
        }
    }
}

impl<Type> FromStr for Id<Type> {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let id = s.parse::<usize>()?;
        Ok(Id {
            id,
            _ty: PhantomData,
        })
    }
}

impl<Type> PartialOrd for Id<Type> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.id.cmp(&other.id))
    }
}
impl<Type> Ord for Id<Type> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.id.cmp(&other.id)
    }
}
impl<Type> Copy for Id<Type> {}
impl<Type> std::fmt::Debug for Id<Type> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.id)
    }
}
impl<Type> std::fmt::Display for Id<Type> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.id)
    }
}
macro_rules! impl_indexed_type {
    (@lf $($ty:ident),*) => {
        $(
            impl<'a> Indexed for $ty<'a> {
                type StaticTypeTagForIndex = $ty<'static>;
                type IndexType = Id<$ty<'static>>;
            }
        )*
    };
    ($($ty:ident),*) => {
        $(
            impl Indexed for $ty {
                type StaticTypeTagForIndex = $ty;
                type IndexType = Id<$ty>;
            }
        )*
    };
}

impl_indexed_type!(@lf InputFunction, FunctionWithBody, Import, Export, Table, Global, Element, Data, DataInSegment);

impl_indexed_type!(MemoryType, FuncType, TagType);

// TODO: replace with macro_metavar_expr_concat
// Currently need explicitly define private type for each index
#[macro_export]
macro_rules! impl_standalone_index {
    ( $($ty:ident($priv:ident)),* ) => {
        $(
            pub enum $priv {}
            pub type $ty = $crate::index::Id<$priv>;
            impl crate::index::Indexed for $priv {
                type StaticTypeTagForIndex = $priv;
                type IndexType = Id<$priv>;
            }
        )*
    };
}

// Type that maybe defined or imported.
// In WASM a lot of objects can be either defined or imported,
// and all imports are stored before defined, so index for defined is always shifted by imports count.
// Currently Defined or Import can be: function, global, table, memory, ..etc.
pub trait Defined<'src>: Indexed {
    type Import;
}

pub trait OutputType<'src> {
    type InputType: Indexed + 'src;
    // to use InputType::IndexType we need rtn
    fn get_input_index(&self) -> Option<Id<<Self::InputType as Indexed>::StaticTypeTagForIndex>>;
}

/// One place for storing imports and defined items,
/// so we can use
#[derive(Debug)]
pub struct ImportsOrDefined<'src, D: Defined<'src>> {
    pub imports: Vec<D::Import>,
    pub defined: Vec<D>,
}

impl<'src, D: Defined<'src>> ImportsOrDefined<'src, D> {
    pub fn new(imports: Vec<D::Import>, defined: Vec<D>) -> Self {
        Self { imports, defined }
    }
    pub fn imports(&self) -> &[D::Import] {
        &self.imports
    }
    pub fn defined(&self) -> &[D] {
        &self.defined
    }

    pub fn push_import(&mut self, import: D::Import) -> Id<D::StaticTypeTagForIndex> {
        self.imports.push(import);
        Id::from_index(self.imports.len() - 1)
    }

    /// After locking, no modification is allowed.
    #[allow(private_bounds)]
    pub fn lock(self) -> WithOriginalIndex<'src, D>
    where
        D: OutputType<'src>,
        D::Import: OutputType<'src, InputType = D::InputType>,
    {
        WithOriginalIndex::new(self)
    }
}

/// After building this collection, no modification is allowed.

pub struct WithOriginalIndex<'src, T>
where
    T: OutputType<'src> + Defined<'src>,
{
    collection: ImportsOrDefined<'src, T>,
    map: IdMap<
        Id<<T::InputType as Indexed>::StaticTypeTagForIndex>,
        Id<<T as Indexed>::StaticTypeTagForIndex>,
    >,
}

impl<'src, T: Debug> Debug for WithOriginalIndex<'src, T>
where
    T: OutputType<'src> + Defined<'src>,
    T::Import: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WithOriginalIndex")
            .field("collection", &self.collection)
            .field("map", &self.map)
            .finish()
    }
}

#[allow(private_bounds)]
impl<'src, T> WithOriginalIndex<'src, T>
where
    T: OutputType<'src> + Defined<'src>,
    T::Import: OutputType<'src, InputType = T::InputType>,
{
    pub fn new(collection: ImportsOrDefined<'src, T>) -> Self {
        let imports = collection.imports().iter().map(OutputType::get_input_index);
        let defined = collection.defined().iter().map(OutputType::get_input_index);
        let map = imports
            .chain(defined)
            .enumerate()
            .filter_map(|(i, input_id)| input_id.map(|input_id| (input_id, Id::from_index(i))))
            .collect();
        WithOriginalIndex { collection, map }
    }

    pub fn get_output_id(
        &self,
        input_id: Id<<T::InputType as Indexed>::StaticTypeTagForIndex>,
    ) -> Option<Id<<T as Indexed>::StaticTypeTagForIndex>> {
        self.map.get(input_id).cloned()
    }
    pub fn get_input_id(
        &self,
        output_id: Id<<T as Indexed>::StaticTypeTagForIndex>,
    ) -> Option<Id<<T::InputType as Indexed>::StaticTypeTagForIndex>> {
        let raw_output_id = output_id.as_raw_index();
        if raw_output_id < self.collection.imports().len() {
            // If output_id is less than imports count, then it is import
            self.collection
                .imports()
                .get(raw_output_id)
                .and_then(OutputType::get_input_index)
        } else {
            // Otherwise it is defined
            let defined_index = raw_output_id - self.collection.imports().len();
            self.collection
                .defined()
                .get(defined_index)
                .and_then(OutputType::get_input_index)
        }
    }

    pub fn imports(
        &self,
    ) -> impl Iterator<Item = (Id<<T as Indexed>::StaticTypeTagForIndex>, &T::Import)> + ExactSizeIterator
    {
        self.collection
            .imports()
            .iter()
            .enumerate()
            .map(|(id, import)| (Id::from_index(id), import))
    }
    pub fn defined(
        &self,
    ) -> impl Iterator<Item = (Id<<T as Indexed>::StaticTypeTagForIndex>, &T)> + ExactSizeIterator
    {
        let num_imports = self.collection.imports().len();
        self.collection
            .defined()
            .iter()
            .enumerate()
            .map(move |(id, defined)| (Id::from_index(id + num_imports), defined))
    }

    pub fn get_import_for_output_id(
        &self,
        output_id: Id<<T as Indexed>::StaticTypeTagForIndex>,
    ) -> Option<&T::Import> {
        let raw_output_id = output_id.as_raw_index();
        if raw_output_id < self.collection.imports().len() {
            self.collection.imports().get(raw_output_id)
        } else {
            None
        }
    }

    pub fn get_defined_for_output_id(
        &self,
        output_id: Id<<T as Indexed>::StaticTypeTagForIndex>,
    ) -> Option<&T> {
        let raw_output_id = output_id.as_raw_index();
        if raw_output_id < self.collection.defined().len() {
            self.collection.defined().get(raw_output_id)
        } else {
            None
        }
    }

    pub fn iter_all_ids<'a>(
        &'a self,
    ) -> impl Iterator<Item = Id<<T as Indexed>::StaticTypeTagForIndex>> {
        (0..self.len()).map(Id::from_index)
    }
    pub fn len(&self) -> usize {
        self.collection.imports().len() + self.collection.defined().len()
    }
}
