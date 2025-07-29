use std::{
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

pub type SymbolId = usize;
pub type SectionId = usize;
pub type OutputFuncId = usize;
pub type OutputSymbolDataId = usize;
pub type OutputGlobalId = usize;

pub type FuncTypeId = Id<FuncType>;
pub type InputFuncId = Id<InputFunction<'static>>;
pub type DefinedFuncId = Id<FunctionWithBody<'static>>;
pub type TableId = Id<Table<'static>>;
pub type ImportId = Id<Import<'static>>;
pub type ExportId = Id<Export<'static>>;
pub type MemoryId = Id<MemoryType>;
pub type GlobalId = Id<Global<'static>>;
pub type ElementId = Id<Element<'static>>;
pub type DataSegmentId = Id<Data<'static>>;
pub type DataSymbolId = Id<DataInSegment<'static>>;
pub type DataId = (DataSegmentId, DataSymbolId);
pub type TagId = Id<TagType>;
// TODO: Maybe replace Vecs with id_arena?
// Currently the only difference is that we also use

pub struct Id<Type> {
    id: usize,
    _ty: PhantomData<fn() -> Type>,
}
// TODO: Remove this functions later
impl<Type> Id<Type> {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdMap<Idx: 'static, Res> {
    vecmap: VecMap<Res>,
    _res: PhantomData<Idx>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdVec<Type, Idx = Id<<Type as Indexed>::StaticIndexType>> {
    types: Vec<Type>,
    _idx: PhantomData<Idx>,
}

pub trait Indexed {
    type StaticIndexType;
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

impl<T> IndexedSection<T> {
    fn is_default(&self) -> bool {
        self.starting_offset == 0 && self.section_index == usize::MAX
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
impl<Type, Res> FromIterator<(Id<Type>, Res)> for IdMap<Id<Type>, Res>
where
    Type: 'static,
{
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
    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }
}

impl<Type, Res> IdMap<Id<Type>, Res> {
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

impl<T: Indexed> Index<Id<T>> for IdVec<T> {
    type Output = T;

    fn index(&self, id: Id<T>) -> &Self::Output {
        &self.types[id.id]
    }
}
impl<T: Indexed, Res> Index<Id<T::StaticIndexType>> for IdMap<Id<T>, Res> {
    type Output = Res;

    fn index(&self, id: Id<T::StaticIndexType>) -> &Self::Output {
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
        write!(f, "Id({})", self.id)
    }
}
impl<Type> std::fmt::Display for Id<Type> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.id)
    }
}
macro_rules! impl_as_static_type {
    (@lf $($ty:ident),*) => {
        $(
            impl<'a> Indexed for $ty<'a> {
                type StaticIndexType = $ty<'static>;
            }
        )*
    };
    ($($ty:ident),*) => {
        $(
            impl Indexed for $ty {
                type StaticIndexType = $ty;
            }
        )*
    };
}

impl_as_static_type!(@lf InputFunction, FunctionWithBody, Import, Export, Table, Global, Element, Data, DataInSegment);

impl_as_static_type!(MemoryType, FuncType, TagType);
