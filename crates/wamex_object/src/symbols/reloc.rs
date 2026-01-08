//! Type-safe implementation of relocation entries.
//! For simplifcation relocation can be represented as next pseudocode:
//! ```rust,no_build
//!  pointer = writter.offset_of(def_sym) + .offset;
//!  *pointer = encode_addr_of(.sym) + sym.addend;
//! ```
//! `def_sym` - is the symbol that contain some relocated links (code or data),
//!   and have some place in output buffer.
//! `.sym` - contain information about linked symbol, and can be decomposed to the next information:
//!  - type of symbol (function, datachunk, global or event/section/type)
//!  - encoding type (fixed, leb, sleb)
//!  - encoding width (32 or 64)
//!  - relative information (_mem/table base, tls based, non)
//!  - addend if aplicable
//!
//! But instead of implementing it straightforward like in pseudo-code, due to design of `wasmparser::RelocationEntry`
//! (which is inherited from LLVM) real handling looks like a big match of `ty` and multiple duplicate handlers with copy-pasted logic.
//!
//! In this module we are trying to utilise this decomposed form, but not all symbol indexes can be encoded as all combinations of relation/encoding/width.
//! To keep the same constrains as in original `wasmparser::RelocationEntry`, we implement witness based enums.
//! This witness based type-safe system is hidden under `mod type_safe`
//!
//! This is mostly experiment of using GADT like structure in rust, to implement "optional" fields with type-level guarantee.
//!
//! Some of implementation may be overkill (like type-level option in addend, or handling entries in `TypedRelocationEntry::with_any`), but it could be always refactored to decomposed relocation entry with erased type, and runtime checks.

use std::{fmt::Debug, hash::Hash, marker::PhantomData};

use wamex_internal_macro::Constraints;

use crate::{
    index::SectionId,
    read::{FuncTypeId, FunctionRef, GlobalRef, TableRef},
    symbols::SymbolId,
};

/// Lossless representation of `wasmparser::RelocationEntry` with type-safe disamiguation of symbol types.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy, Constraints)]
pub enum TypedRelocationEntry {
    #[has(tls, base, has64, int, sleb, leb, addend)]
    MemoryAddr(RelocationEntry<MemoryAddr>),
    #[has(leb)]
    TableNumber(RelocationEntry<TableRef>),
    #[has(int, leb)]
    GlobalIndex(RelocationEntry<GlobalRef>),
    #[has(int, leb)]
    FunctionIndex(RelocationEntry<FunctionRef>),
    // Indirect function index used in call_indirect
    #[has(base, has64, int, sleb)]
    TableIndex(RelocationEntry<IndirectFunctionIndex>),
    #[has(has64, int, addend)]
    FunctionOffset(RelocationEntry<FunctionOffset>),
    #[has(int, addend)]
    SectionOffset(RelocationEntry<SectionOffset>),
    #[has(leb)]
    EventIndex(RelocationEntry<EventIndex>),
    #[has(int, addend)]
    MemoryAddrLocrel(RelocationEntry<MemoryAddrLoc>),
    #[index_type(FuncTypeId)]
    #[has(leb)]
    TypeIndex(RelocationEntry<FuncTypeId>),
}

impl TypedRelocationEntry {
    /// Return SymbolId is possible.
    ///
    /// Type index is special, since only one
    /// that contain `FunctionTypeId` in index field
    /// while other contain `SymbolId` instead
    pub fn symbol_id(&self) -> Option<SymbolId> {
        match self {
            Self::TypeIndex(v) => return None,
            _ => {}
        }

        return Some(self.with_any(GetSymbolId));
    }

    pub fn with_any<F>(&self, mut func: F) -> F::Output
    where
        F: Caller<RelocEntry<MemoryAddr> = RelocationEntry<MemoryAddr>>,
        F: Caller<RelocEntry<TableRef> = RelocationEntry<TableRef>>,
        F: Caller<RelocEntry<GlobalRef> = RelocationEntry<GlobalRef>>,
        F: Caller<RelocEntry<FunctionRef> = RelocationEntry<FunctionRef>>,
        F: Caller<RelocEntry<IndirectFunctionIndex> = RelocationEntry<IndirectFunctionIndex>>,
        F: Caller<RelocEntry<FunctionOffset> = RelocationEntry<FunctionOffset>>,
        F: Caller<RelocEntry<SectionOffset> = RelocationEntry<SectionOffset>>,
        F: Caller<RelocEntry<EventIndex> = RelocationEntry<EventIndex>>,
        F: Caller<RelocEntry<MemoryAddrLoc> = RelocationEntry<MemoryAddrLoc>>,
        F: Caller<RelocEntry<FuncTypeId> = RelocationEntry<FuncTypeId>>,
    {
        match self {
            Self::MemoryAddr(v) => func.call::<MemoryAddr>(v),
            Self::TableNumber(v) => func.call::<TableRef>(v),
            Self::GlobalIndex(v) => func.call::<GlobalRef>(v),
            Self::FunctionIndex(v) => func.call::<FunctionRef>(v),
            Self::TableIndex(v) => func.call::<IndirectFunctionIndex>(v),
            Self::FunctionOffset(v) => func.call::<FunctionOffset>(v),
            Self::SectionOffset(v) => func.call::<SectionOffset>(v),
            Self::EventIndex(v) => func.call::<EventIndex>(v),
            Self::MemoryAddrLocrel(v) => func.call::<MemoryAddrLoc>(v),
            Self::TypeIndex(v) => func.call::<FuncTypeId>(v),
        }
    }
}

pub trait Caller {
    type RelocEntry<Type: Constraints>;
    type Output;
    fn call<Type: Constraints>(&mut self, r: &Self::RelocEntry<Type>) -> Self::Output;
}

fn get_symbol_id<Type: Constraints>(entry: &RelocationEntry<Type>) -> SymbolId {
    SymbolId::new(entry.index.index())
}
struct GetSymbolId;

impl Caller for GetSymbolId {
    type RelocEntry<Type: Constraints> = RelocationEntry<Type>;
    type Output = SymbolId;
    fn call<Type: Constraints>(&mut self, r: &Self::RelocEntry<Type>) -> Self::Output {
        get_symbol_id(r)
    }
}

///
/// Implementation of relocation entry for some typed index.
/// We use type-safe indexes to enforce constraints of specific relocation type.
///
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub struct RelocationEntry<Indx: Constraints> {
    /// Optional addend to be added to the resulting value.
    pub addend: Addend<Indx>,
    /// Index of symbol in `Symbols` table that store information about relocated symbol.
    /// This is generic due to fact that `types` isn't stored as symbol in `Symbols` table.
    pub index: Indx::IndexType,
    /// Offset in bytes from the start of the symbol definition
    /// targeted by this relocation.
    pub offset: u32,
    /// Information about global variable base, if this is position independent relocation.
    pub relation: Relative<Indx>,
    /// Representation of resulting value in the output binary.
    /// Either Sleb/Leb or fixed integer.
    pub encoding: Encoding<Indx>,
    /// Width of encoding result value (64 or 32 bit)
    pub width: RelocationWidth<Indx>,
    /// Type-safe index tag in, that signalize in which table object is stored,
    /// like addend for MemoryAddr/FunctionOffset/SectionOffset
    pub index_type: PhantomData<Indx>,
}

// == Extra typed indexes ==
// This extra typed indexes currently cannot be constructed, and only used as type level tag -
// therefore don't need actual fields, but if at any future development we would need some of this type - we will have them (but probably in other places).

/// Wrapper around `FunctionRef`, that instead of giving function index - gives index function in `__indirect_function_table`
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub struct IndirectFunctionIndex(pub FunctionRef);

/// Representation of wasm `event` index
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub struct EventIndex {
    pub index: u32,
}

/// Addr of Chunk in memory
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub struct MemoryAddr {
    pub mem_chunk_id: u32, // data chunk ref?
}

/// Addr of Chunk in segment?
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub struct MemoryAddrLoc(MemoryAddr);

/// Place in function code
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub struct FunctionOffset {
    pub function: FunctionRef,
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
#[repr(packed)] // always first element, so can be unaligned
pub struct SectionOffset {
    pub section: SectionId,
}

/// Value that should be added to relocated address/index.
/// For functions/globals/events - addend is not applicable.
/// For memory addresses and offsets - addend is either
/// 32 or 64 bit integer that added to resulting address.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub struct Addend<Index: Constraints> {
    v: TypeLevelOption<i64, Index::Addend>,
}

impl<Index> Addend<Index>
where
    Index: Constraints<Addend = Has>,
{
    pub fn some(value: i64) -> Addend<Index> {
        Addend {
            v: TypeLevelOption::some(value),
        }
    }
}

impl<Index> Addend<Index>
where
    Index: Constraints,
{
    pub fn none() -> Addend<Index> {
        Addend {
            v: TypeLevelOption::none(),
        }
    }
    pub fn to_option(&self) -> Option<i64> {
        self.v.to_option()
    }
}

// == Type safety markers and traits ==

/// Each type can mark associated types either `Yes` or `No` for each of associated types.
/// Every `No` marker enforces that enum variant with this constraint cannot be constructed.
pub trait Constraints {
    type RelTls: Debug + Eq + Hash + Copy + SealedOptionTag;
    type RelBase: Debug + Eq + Hash + Copy + SealedOptionTag;
    type Has64: Debug + Eq + Hash + Copy + SealedOptionTag;
    type Int: Debug + Eq + Hash + Copy + SealedOptionTag;
    type Sleb: Debug + Eq + Hash + Copy + SealedOptionTag;
    type Leb: Debug + Eq + Hash + Copy + SealedOptionTag;
    type Addend: Debug + Eq + Hash + Copy + SealedOptionTag;
    type IndexType: Debug + Eq + Hash + Copy + EntityRef;
}
use impl_type_safety::{SealedOptionTag, TypeLevelOption};

use crate::index::EntityRef;

/// Constructor of witnesses
/// Use as
/// ```
/// RelocationEntry {
///     encoding: Encoding::Leb(Has),
/// // ...
/// }
/// ```
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub struct Has;

mod impl_type_safety {
    use super::*;

    pub trait SealedOptionTag {
        const SOME: bool;
    }
    impl SealedOptionTag for Yes {
        const SOME: bool = true;
    }
    impl SealedOptionTag for No {
        const SOME: bool = false;
    }
    pub type Yes = Has;
    pub type No = std::convert::Infallible;

    #[derive(Clone, Copy)]
    pub union TypeLevelOption<V: Copy, Tag: Copy> {
        has: (V, Tag),
        none: (),
    }
    impl<V: Copy> TypeLevelOption<V, Has> {
        pub fn some(value: V) -> Self {
            Self { has: (value, Has) }
        }
        pub fn into_inner(self) -> V {
            unsafe { self.has.0 }
        }
    }

    impl<V: Copy, Tag: Copy> TypeLevelOption<V, Tag> {
        pub fn none() -> Self {
            Self { none: () }
        }

        pub fn to_option(self) -> Option<V>
        where
            Tag: SealedOptionTag,
        {
            if Tag::SOME {
                // Safety: SealedOptionTag has SOME=false when Tag is Never type.
                Some(unsafe { self.has.0 })
            } else {
                let _ = unsafe { self.none };
                None
            }
        }
    }

    impl<V: Copy, Tag: Copy + SealedOptionTag> Debug for TypeLevelOption<V, Tag>
    where
        V: Debug,
    {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.to_option().fmt(f)
        }
    }

    impl<V: Copy, Tag: Copy + SealedOptionTag> PartialEq for TypeLevelOption<V, Tag>
    where
        V: PartialEq,
    {
        fn eq(&self, other: &Self) -> bool {
            self.to_option() == other.to_option()
        }
    }

    impl<V: Copy, Tag: Copy + SealedOptionTag> Eq for TypeLevelOption<V, Tag> where V: Eq {}
    impl<V: Copy, Tag: Copy + SealedOptionTag> Hash for TypeLevelOption<V, Tag>
    where
        V: Hash,
    {
        fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
            self.to_option().hash(state)
        }
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub enum Encoding<Index: Constraints> {
    // 4-byte little-endian integer
    // e.g. `uint32` or `int32`
    Fixed(Index::Int),
    // 5-byte Variable-length SIGNED integer
    // 32-bit SLEB128
    Sleb(Index::Sleb),
    // 5-byte Variable-length UNSIGNED integer
    // 32-bit ULEB128
    Leb(Index::Leb),
}

/// Base of addr/index is stored can be stored in global variable.
/// This enum indicates which variable stores this base.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub enum Relative<Index: Constraints> {
    /// Absolute address
    None(Has),
    /// Symbol relative to `__memory_base` / `__table_base` global
    Got(Index::RelBase),
    /// Symbol relative to `__tls_base` global
    Tls(Index::RelTls),
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub enum RelocationWidth<Index: Constraints> {
    Bits32(Has),
    Bits64(Index::Has64),
}

// Is not used anymore - but during implementation we highlighted what entities are actually used
//  - therefore let it now live in comment
// trait SymbolResolver {
//     fn function_ref(&self, symbol: SymbolId) -> Option<FunctionRef>;
//     fn global_ref(&self, symbol: SymbolId) -> Option<GlobalRef>;
//     fn table_ref(&self, symbol: SymbolId) -> Option<TableRef>;
//     fn memory_chunk(&self, symbol: SymbolId) -> Option<u32>;
// }

// // Check size compatibility with wasmparser::RelocationEntry
const _ASSERT_SIZE: () = const {
    use std::mem::align_of;
    // Current rust version cannot pack 3 1-byte fields into outer enum-variant without padding
    // We could use repr(packed) but it could potentially cause perfomance loss on some architectures.
    // Therefore for now it is commented and runtime test is added as marker.
    // assert!(size_of::<TypedRelocationEntry>() <= size_of::<wasmparser::RelocationEntry>());
    assert!(align_of::<TypedRelocationEntry>() <= align_of::<wasmparser::RelocationEntry>());
    let addend_offset = std::mem::offset_of!(RelocationEntry<MemoryAddr>, addend);
    let index_offset = std::mem::offset_of!(RelocationEntry<MemoryAddr>, index);
    let offset_offset = std::mem::offset_of!(RelocationEntry<MemoryAddr>, offset);

    assert!(addend_offset == 0);
    assert!(index_offset == 8);
    assert!(offset_offset == 12);
};

#[cfg(test)]
mod tests {

    #[test]
    fn runtime_assert_size() {
        use std::mem::{align_of, size_of};
        println!(
            "Size of TypedRelocationEntry: {}",
            size_of::<super::TypedRelocationEntry>()
        );

        println!(
            "Size of wasmparser::RelocationEntry: {}",
            size_of::<wasmparser::RelocationEntry>()
        );
        assert!(
            size_of::<super::TypedRelocationEntry>() <= size_of::<wasmparser::RelocationEntry>()
        );
        assert!(
            align_of::<super::TypedRelocationEntry>() <= align_of::<wasmparser::RelocationEntry>()
        );
    }
}
