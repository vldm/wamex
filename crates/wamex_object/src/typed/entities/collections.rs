//! Wasm spec defines multiple types of entities https://webassembly.github.io/spec/core/syntax/types.html
//! for representing various concepts in wasm modules.
//!
//! Some of them are usefull for low-level representation of wasm modules, like value types (i32, i64, f32, f64, v128, funcref, externref),
//! that are used in function signatures, global types, table types, etc.
//!
//! Other types are more high-level and represent concepts like function types, table types, global types, memory types, etc.
//! These types are used to define the structure and behavior of wasm modules.
//!
//! This module agregate common functionality used for high-level wasm entities representation.
//! the list of supported types is based on `wasmparser::EntityType` and includes:
//! - Function types
//! - Table types
//! - Memory types
//! - Global types
//! - Tag types
//!
//! Common functionality includes:
//! - imports/defined indexing
//! - exports listing
//! - name resolution (from name section)
//!
//! unlike in `walrus` this information represented as structure of arrays
//!

use std::borrow::Cow;

use cranelift_entity::EntityRef;
use wasmparser::SymbolFlags;

use super::{
    FunctionRef, GlobalRef, MemoryRef, TableRef, TagRef,
    types::{self, ExportEntry},
};
use crate::{
    index::{Building, CompoundList, Finished, GappedMap, ImportOrDefined, NonDefault, TempIndex},
    raw,
    typed::{
        DefinedFunction, DefinedGlobal, DefinedMemory, DefinedTable, DefinedTag, ImportedFunction,
        ImportedGlobal, ImportedMemory, ImportedTable, ImportedTag,
    },
};

pub type Functions<'src, BS = Finished> =
    EntitiesCollection<'src, FunctionRef, ImportedFunction<'src>, DefinedFunction<'src>, BS>;
pub type Tables<'src, BS = Finished> =
    EntitiesCollection<'src, TableRef, ImportedTable<'src>, DefinedTable<'src>, BS>;
pub type Globals<'src, BS = Finished> =
    EntitiesCollection<'src, GlobalRef, ImportedGlobal<'src>, DefinedGlobal<'src>, BS>;
pub type Memories<'src, BS = Finished> =
    EntitiesCollection<'src, MemoryRef, ImportedMemory<'src>, DefinedMemory, BS>;
pub type Tags<'src, BS = Finished> =
    EntitiesCollection<'src, TagRef, ImportedTag<'src>, DefinedTag, BS>;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EntitiesCollection<'src, Ref, Import, Defined, BuilderState = Finished>
where
    Ref: TempIndex,
{
    /// Both defined and imports entities.
    pub items: CompoundList<Ref, Import, Defined, BuilderState>,

    /// Names for entities, if present.
    /// Name information is gathered from name section,
    /// if no name is present in name section, name is retrieved from export entry.
    /// Note: name can be different from one in linkage Symbols.
    pub names: GappedMap<Ref, NonDefault<&'src str>>,

    /// List of exported entries.
    /// Because exports array are usually small, no need to store them as `IdMap`
    pub exports: Vec<ExportEntry<'src, Ref>>,
}

impl<'src, Ref, Import, Defined> EntitiesCollection<'src, Ref, Import, Defined, Building>
where
    Ref: TempIndex,
{
    pub fn new() -> Self {
        Self {
            items: CompoundList::empty(),
            names: GappedMap::new(),
            exports: Vec::new(),
        }
    }
    
    pub fn into_finished(self) -> EntitiesCollection<'src, Ref, Import, Defined, Finished> {
        EntitiesCollection {
            items: self.items.into_finished(),
            names: self.names,
            exports: self.exports,
        }
    }
}

impl<'src, Ref, Import, Defined> EntitiesCollection<'src, Ref, Import, Defined>
where
    Ref: TempIndex,
{
    pub fn from_parts(
        declared: CompoundList<Ref, Import, Defined>,
        mut names: GappedMap<Ref, NonDefault<&'src str>>,
        exports: Vec<ExportEntry<'src, Ref>>,
    ) -> Self {
        for exports in &exports {
            // if name is not present in names map
            if names.get(exports.entity_index).is_none() {
                // and if it can be borrowed from export entry
                if let Cow::Borrowed(name) = exports.name {
                    // insert it into names map
                    names.insert(exports.entity_index, NonDefault::from(name));
                }
            }
        }

        Self {
            items: declared,
            names,
            exports,
        }
    }
    pub fn defined_iter(&self) -> impl Iterator<Item = (Ref, &Defined)> {
        self.items.defined_iter()
    }
    pub fn imports_iter(&self) -> impl Iterator<Item = (Ref, &Import)> {
        self.items.imports_iter()
    }
    pub fn iter(&self) -> impl Iterator<Item = (Ref, ImportOrDefined<&Import, &Defined>)> {
        self.items.iter()
    }
    pub fn iter_all_ids(&self) -> impl ExactSizeIterator<Item = Ref> {
        (0..(self.items.imports.len() + self.items.defined.len())).map(EntityRef::new)
    }
    pub fn len(&self) -> usize {
        self.items.imports.len() + self.items.defined.len()
    }
}

#[cfg(debug_assertions)]
mod assert_covariance {
    use super::*;

    macro_rules! assert_covariance {
        ($v:ident) => {
            impl<'long, V> $v<'long, V> {
                pub fn _assert_covariance<'short>(self) -> $v<'short, V>
                where
                    'long: 'short,
                {
                    self
                }
            }
        };
    }
    #[allow(dead_code)]
    struct Invariant<'a, V> {
        _marker: std::marker::PhantomData<fn(&'a ()) -> &'a ()>,
        _marker2: std::marker::PhantomData<V>,
    }
    #[allow(dead_code)]
    struct Covariant<'a, V> {
        _marker: std::marker::PhantomData<&'a ()>,
        _marker2: std::marker::PhantomData<V>,
    }

    assert_covariance!(Covariant);
    // Failed at the moment:
    assert_covariance!(Functions);
    assert_covariance!(Tables);
    assert_covariance!(Memories);
    assert_covariance!(Globals);
    assert_covariance!(Tags);

    // Expected to fail test
    // assert_covariance!(Invariant);
}

//TODO: Move Output->input mapping to separate module?

// /// A type that can provide index of corresponding entity in input module.
// pub trait GetInputRef<InputRef> {
//     // TODO: Module ID?
//     /// Returns index of corresponding entity in input module.
//     fn get_input_index(&self) -> OutputMapType<InputRef>;
// }

// /// A reference that possibly linked to another entity in input module.
// pub trait LinkedToInputRef: ReservedValue {
//     type InputRef: EntityRef;
// }

// /// Implementation of `GetInputRef` for `ImportOrDefined`.
// impl<Imported, Defined, InputRef> GetInputRef<InputRef> for ImportOrDefined<Imported, Defined>
// where
//     Imported: GetInputRef<InputRef>,
//     Defined: GetInputRef<InputRef>,
// {
//     fn get_input_index(&self) -> OutputMapType<InputRef> {
//         match self {
//             ImportOrDefined::Import(import) => import.get_input_index(),
//             ImportOrDefined::Defined(defined) => defined.get_input_index(),
//         }
//     }
// }

// impl<'a, T, InputRef> GetInputRef<InputRef> for &'a T
// where
//     T: GetInputRef<InputRef>,
// {
//     fn get_input_index(&self) -> OutputMapType<InputRef> {
//         (*self).get_input_index()
//     }
// }

// /// Collection of entities used in build of output module, with information about corresponding entity in input module.
// pub struct EntitiesFromInput<'src, IDX>
// where
//     IDX: LinkedToInputRef + CompoundRef,
// {
//     entities: CompoundList<'src, IDX>,
//     /// Map from input entity to coresponding output entity in `entities` collection.
//     from_input: GappedMap<IDX::InputRef, IDX>,
// }

// impl<'src, IDX> EntitiesFromInput<'src, IDX>
// where
//     IDX: LinkedToInputRef + CompoundRef,
//     IDX::ImportType<'src>: GetInputRef<IDX::InputRef>,
//     IDX::DefinedType<'src>: GetInputRef<IDX::InputRef>,
// {
//     pub fn new(entities: CompoundList<'src, IDX>) -> Self {
//         let from_input = entities
//             .iter()
//             .filter_map(|(output_id, v)| {
//                 v.get_input_index()
//                     .into_bidirectional()
//                     .map(|input_id| (input_id, output_id))
//             })
//             .collect();
//         Self {
//             entities,
//             from_input,
//         }
//     }

//     pub fn get_output_id(&self, input_id: IDX::InputRef) -> Option<IDX> {
//         self.from_input.get(input_id).copied()
//     }
//     pub fn get_input_id(&self, output_id: IDX) -> Option<IDX::InputRef> {
//         let v = self.entities.get_entity(output_id);
//         v.get_input_index().has_input()
//     }

//     pub fn imports(&self) -> impl ExactSizeIterator<Item = (IDX, &IDX::ImportType<'src>)> {
//         self.entities.imports_iter()
//     }
//     pub fn defined(&self) -> impl ExactSizeIterator<Item = (IDX, &IDX::DefinedType<'src>)> {
//         self.entities.defined_iter()
//     }

//     pub fn get_import_for_output_id(&self, output_id: IDX) -> Option<&IDX::ImportType<'src>> {
//         let raw_id = output_id.index();
//         self.entities.imports.get(EntityRef::new(raw_id))
//     }

//     pub fn get_defined_for_output_id(&self, output_id: IDX) -> Option<&IDX::DefinedType<'src>> {
//         let raw_id = output_id.index().checked_sub(self.entities.imports.len())?;
//         self.entities.defined.get(EntityRef::new(raw_id))
//     }

//     pub fn iter_all_ids(&self) -> impl ExactSizeIterator<Item = IDX> {
//         (0..self.len()).map(EntityRef::new)
//     }
//     pub fn len(&self) -> usize {
//         self.entities.imports.len() + self.entities.defined.len()
//     }
// }

// pub enum OutputMapType<Input> {
//     /// Each output type can be mapped to an input type and vice versa.
//     BidirectionalMap(Input),
//     /// Only Output -> Input mapping is guaranteed.
//     OutputHasInput(Input),
//     // The output is a new type that has no corresponding input.
//     None,
// }

// impl<Input> OutputMapType<Input> {
//     pub fn bidirectional_from_option(input: Option<Input>) -> Self {
//         match input {
//             Some(input) => OutputMapType::BidirectionalMap(input),
//             None => OutputMapType::None,
//         }
//     }
//     pub fn into_bidirectional(self) -> Option<Input> {
//         match self {
//             OutputMapType::BidirectionalMap(input) => Some(input),
//             _ => None,
//         }
//     }
//     pub fn has_input(self) -> Option<Input> {
//         match self {
//             OutputMapType::BidirectionalMap(input) | OutputMapType::OutputHasInput(input) => {
//                 Some(input)
//             }
//             OutputMapType::None => None,
//         }
//     }
// }
// impl<'src, IDX> Debug for CompoundList<'src, IDX>
// where
//     IDX: CompoundRef,
//     IDX::ImportType<'src>: Debug,
//     <IDX::ImportType<'src> as PrimaryKey>::EntityRef: Debug,
//     IDX::DefinedType<'src>: Debug,
//     <IDX::DefinedType<'src> as PrimaryKey>::EntityRef: Debug,
// {
//     fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
//         f.debug_struct("EntitiesCollection")
//             .field("imports", &self.imports)
//             .field("defined", &self.defined)
//             .finish()
//     }
// }

// impl<'src, IDX> Debug for EntitiesCollection<'src, IDX>
// where
//     IDX: CompoundRef + Debug,
//     IDX::ImportType<'src>: Debug,
//     <IDX::ImportType<'src> as PrimaryKey>::EntityRef: Debug,
//     IDX::DefinedType<'src>: Debug,
//     <IDX::DefinedType<'src> as PrimaryKey>::EntityRef: Debug,
// {
//     fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
//         f.debug_struct("EntitiesCollection")
//             .field("items", &self.items)
//             .field("names", &self.names)
//             .field("exports", &self.exports)
//             .finish()
//     }
// }
