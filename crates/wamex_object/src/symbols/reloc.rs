//! Type-safe implementation of relocation entries.
//! For simplifcation relocation can be represented as next pseudocode:
//! ```rust,no_build
//!  pointer = writter.offset_of(def_sym) + .offset;
//!  *pointer = encode_addr_of(.sym);
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

use std::{fmt::Debug, hash::Hash};

use crate::{index::SectionId, read::{FuncTypeId, FunctionRef, GlobalRef, TableRef}, symbols::SymbolId};

use type_safety::{ImplDyn, Has, TypeIs};


/// Lossless representation of `wasmparser::RelocationEntry` with type-safe disamiguation of symbol types.
/// 
/// Linkage - represents any relocation which index is symbol_id stored in `Symbols` table;
/// And Type - represents relocations which reffer to `type` index instead.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub enum LinkageRelocationEntry {
    Type(RelocationEntry<FuncTypeId>),
    Linkage(RelocationEntry<LinkageSymbol>),
}
// Converted `RelocationType` - with indexes 
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub enum TypedRelocationEntry {
    MemoryAddr (RelocationEntry<MemoryAddr>),
    TableNumber (RelocationEntry<TableRef>),
    GlobalIndex (RelocationEntry<GlobalRef>),
    FunctionIndex(RelocationEntry<FunctionRef>),
    // Indirect function index used in call_indirect
    TableIndex (RelocationEntry<IndirectFunctionIndex>),
    FunctionOffset (RelocationEntry<FunctionOffset>),
    SectionOffset (RelocationEntry<SectionOffset>),
}

///
/// Implementation of relocation entry for some typed index.
/// 
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub struct RelocationEntry<Indx> {
    /// Type-safe index in corresponding table, with optional information
    /// like addend for MemoryAddr/FunctionOffset/SectionOffset
    pub index: Indx,
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
}


// == Extra typed indexes == 
/// Wrapper around `FunctionRef`, that instead of giving function index - gives index function in `__indirect_function_table`
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub struct IndirectFunctionIndex(FunctionRef);

/// Addr of Chunk in memory
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
#[repr(packed)]
pub struct MemoryAddr {
    pub addend: i64,    // also used in section offset and function offset
    pub mem_chunk_id: u32, // data chunk ref?
}

/// Place in function code 
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
#[repr(packed)]
pub struct FunctionOffset {
    pub addend: i64,
    pub function: FunctionRef
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
#[repr(packed)]
pub struct SectionOffset {
    pub addend: i64,
    pub section: SectionId,
}

// == Type safety markers and traits ==

pub trait Encode {}
pub enum Sleb {}
pub enum Fixed {}
pub enum Leb {}

impl Encode for Sleb {}
impl Encode for Fixed {}
impl Encode for Leb {}

pub trait EncodableWith<Repr: Encode> {}
unsafe impl<V, AnyE> ImplDyn<dyn EncodableWith<AnyE>> for V {}


// impl of encoding
impl EncodableWith<Fixed> for IndirectFunctionIndex {}
impl EncodableWith<Sleb> for IndirectFunctionIndex {}
impl EncodableWith<Fixed> for FunctionRef {}
impl EncodableWith<Leb> for FunctionRef {}

impl EncodableWith<Fixed> for MemoryAddr {}
impl EncodableWith<Sleb> for MemoryAddr {}
impl EncodableWith<Leb> for MemoryAddr {}

// Extra markers impl
pub enum Width64 {}
pub enum Tls {}

impl Has<Width64> for MemoryAddr {}
impl Has<Width64> for IndirectFunctionIndex {}

impl Has<Tls> for MemoryAddr {}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub enum Encoding<Index> {
    // 4-byte little-endian integer
    // e.g. `uint32` or `int32`
    Fixed(TypeIs<Index, dyn EncodableWith<Fixed>>),
    // 5-byte Variable-length SIGNED integer
    // 32-bit SLEB128
    Sleb(TypeIs<Index, dyn EncodableWith<Sleb>>),
    // 5-byte Variable-length UNSIGNED integer
    // 32-bit ULEB128
    Leb(TypeIs<Index, dyn EncodableWith<Leb>>),
}

/// Base of addr/index is stored can be stored in global variable.
/// This enum indicates which variable stores this base.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub enum Relative<Index> {
    /// Absolute address
    None,
    /// Symbol relative to `__memory_base` / `__table_base` global
    Got,
    /// Symbol relative to `__tls_base` global
    Tls(TypeIs<Index, dyn Has<Tls>>),
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub enum RelocationWidth<Index> {
    Bits32(()),
    Bits64(TypeIs<Index, dyn Has<Width64>>), // Can also be used for FunctionOffset
}

/// Enumeration of all symbols that can be stored in `Symbols` table.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub enum LinkageSymbolType {
    FunctionIndex,
    TableIndex,
    GlobalIndex,
    MemoryAddr,
    TableNumber,
    SectionOffset,
    FunctionOffset,
    EventIndex,
}

// Unresolve symbol id
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub struct LinkageSymbol {
    addend: i64,
    symbol_id: SymbolId,
    index_type: LinkageSymbolType,
}

// LinkageRef is generic over index types
impl Has<Width64> for LinkageSymbol {}
impl Has<Tls> for LinkageSymbol {}


impl From<wasmparser::RelocationEntry> for LinkageRelocationEntry {
    fn from(entry: wasmparser::RelocationEntry) -> Self {
        use wasmparser::RelocationType::*;
        let index_type = match entry.ty {
            TypeIndexLeb => {
                return LinkageRelocationEntry::Type(RelocationEntry {
                    offset: entry.offset,
                    index: FuncTypeId::from_u32(entry.index),
                    relation: Relative::None,
                    encoding: Encoding::Leb(TypeIs::impl_of()),
                    width: RelocationWidth::Bits32(()),
                }
            );
            },
            EventIndexLeb => LinkageSymbolType::EventIndex,
            TableNumberLeb => LinkageSymbolType::TableNumber,
            SectionOffsetI32 => LinkageSymbolType::SectionOffset,
            FunctionOffsetI32 | FunctionOffsetI64 => LinkageSymbolType::FunctionOffset,
            GlobalIndexI32 | GlobalIndexLeb => LinkageSymbolType::GlobalIndex,
            FunctionIndexLeb | FunctionIndexI32 => LinkageSymbolType::FunctionIndex,
            TableIndexSleb | TableIndexI32 | TableIndexI64
            | TableIndexRelSleb | TableIndexRelSleb64 | TableIndexSleb64 => {
                LinkageSymbolType::TableIndex
            }
            MemoryAddrI32 | MemoryAddrLeb | MemoryAddrSleb | MemoryAddrRelSleb | MemoryAddrLocrelI32 | MemoryAddrTlsSleb
            //64 bit
            | MemoryAddrI64 | MemoryAddrLeb64 | MemoryAddrSleb64| MemoryAddrRelSleb64 
            | MemoryAddrTlsSleb64 => LinkageSymbolType::MemoryAddr,
        };
        let index = LinkageSymbol {
            addend: entry.addend,
            symbol_id: SymbolId::from_u32(entry.index),
            index_type,
        };
        let encoding: Encoding<LinkageSymbol> = match entry.ty {
            SectionOffsetI32 | FunctionOffsetI32 | GlobalIndexI32 | FunctionIndexI32
            | TableIndexI32 | MemoryAddrI32 | FunctionOffsetI64 | TableIndexI64
            | MemoryAddrI64 | MemoryAddrLocrelI32 => Encoding::Fixed(TypeIs::impl_of()),

            TableIndexRelSleb64 | TableIndexSleb64 | TableIndexRelSleb
            | TableIndexSleb | MemoryAddrLeb64 | MemoryAddrSleb64
            | MemoryAddrSleb | MemoryAddrRelSleb | MemoryAddrRelSleb64 | MemoryAddrTlsSleb
            | MemoryAddrTlsSleb64 => Encoding::Sleb(TypeIs::impl_of()),

            FunctionIndexLeb | GlobalIndexLeb | TableNumberLeb | MemoryAddrLeb
            | EventIndexLeb => Encoding::Leb(TypeIs::impl_of()),

            TypeIndexLeb => unreachable!(),
        };

        let relation = match entry.ty {
            TableIndexRelSleb | TableIndexRelSleb64 |  MemoryAddrRelSleb  | MemoryAddrRelSleb64 => {
                Relative::Got
            }
            MemoryAddrTlsSleb64 | MemoryAddrTlsSleb  => {
                Relative::Tls( TypeIs::impl_of() )
            }
            SectionOffsetI32 | FunctionOffsetI32 | GlobalIndexI32 | FunctionIndexI32
            | TableIndexI32 | MemoryAddrI32 | FunctionOffsetI64 | TableIndexI64
            | MemoryAddrI64 | TableIndexSleb64 | TableIndexSleb | MemoryAddrLeb64 | MemoryAddrSleb64 | MemoryAddrSleb 
            | FunctionIndexLeb | GlobalIndexLeb | TableNumberLeb | MemoryAddrLeb
                | EventIndexLeb | MemoryAddrLocrelI32=> {
                Relative::None
            }
            TypeIndexLeb=> unreachable!(),
        };

        let width  = match entry.ty {
            MemoryAddrLocrelI32| 
            SectionOffsetI32 | FunctionOffsetI32 | GlobalIndexI32 | GlobalIndexLeb| FunctionIndexI32 | FunctionIndexLeb
            | TableIndexI32 | TableIndexSleb | TableIndexRelSleb | MemoryAddrI32 | TableNumberLeb | EventIndexLeb
            | MemoryAddrLeb | MemoryAddrSleb | MemoryAddrRelSleb | MemoryAddrTlsSleb => {
                RelocationWidth::Bits32(())
            }
            FunctionOffsetI64 | TableIndexI64  | TableIndexRelSleb64 | TableIndexSleb64
            | MemoryAddrI64 | MemoryAddrLeb64 | MemoryAddrSleb64 
            | MemoryAddrRelSleb64 | MemoryAddrTlsSleb64 => {
                RelocationWidth::Bits64( TypeIs::impl_of() )
            }
            TypeIndexLeb => unreachable!(),
        };

        Self::Linkage(RelocationEntry {
            offset: entry.offset,
            index,
            relation,
            encoding,
            width,
        })
    }
}


// Check size compatibility with wasmparser::RelocationEntry
const _ASSERT_SIZE: () = const {
    use std::mem::{size_of, align_of};
    assert!(size_of::<LinkageRelocationEntry>() <= size_of::<wasmparser::RelocationEntry>());
    assert!(align_of::<LinkageRelocationEntry>() <= align_of::<wasmparser::RelocationEntry>());
    assert!(size_of::<TypedRelocationEntry>() <= size_of::<wasmparser::RelocationEntry>());
    assert!(align_of::<TypedRelocationEntry>() <= align_of::<wasmparser::RelocationEntry>());
};


mod type_safety {
    use std::{fmt::Debug, hash::Hash, marker::PhantomData};

    pub trait Has<Other> {}
    pub unsafe trait ImplDyn<Trait: ?Sized> {}
    unsafe impl<Any, T> ImplDyn<dyn Has<Any>> for T {}
    pub struct TypeIs<Left, Right: ?Sized> {
        _left: PhantomData<*const core::cell::Cell<Left>>,
        _right: PhantomData<*const core::cell::Cell<Right>>,
    }

    impl<Type, DynTrait: ?Sized> TypeIs<Type, DynTrait>
    where
        Type: ImplDyn<DynTrait>,
    {
        pub const fn impl_of() -> Self {
            Self {
                _left: PhantomData,
                _right: PhantomData,
            }
        }
    }
    impl<Type> TypeIs<Type, Type> {
        pub const fn trivial() -> Self {
            Self {
                _left: PhantomData,
                _right: PhantomData,
            }
        }
    }
    impl<Left, Right> TypeIs<Left, Right> {
        // SAFETY: Caller must ensure that Left and Right are equivalent types.
        pub const unsafe fn eq_unchecked() -> Self {
            Self {
                _left: PhantomData,
                _right: PhantomData,
            }
        }
    }
    impl<Left, Right: ?Sized> Debug for TypeIs<Left, Right> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            Ok(())
        }
    }
    impl<Left, Right: ?Sized> PartialEq for TypeIs<Left, Right> {
        fn eq(&self, _other: &Self) -> bool {
            true
        }
    }
    impl<Left, Right: ?Sized> PartialOrd for TypeIs<Left, Right> {
        fn partial_cmp(&self, _: &Self) -> Option<std::cmp::Ordering> {
            Some(std::cmp::Ordering::Equal)
        }
    }
    impl<Left, Right: ?Sized> Eq for TypeIs<Left, Right> {}
    impl<Left, Right: ?Sized> Ord for TypeIs<Left, Right> {
        fn cmp(&self, _: &Self) -> std::cmp::Ordering {
            std::cmp::Ordering::Equal
        }
    }

    impl<Left, Right: ?Sized> Clone for TypeIs<Left, Right> {
        fn clone(&self) -> Self {
            Self {
                _left: PhantomData,
                _right: PhantomData,
            }
        }
    }
    impl<Left, Right: ?Sized> Copy for TypeIs<Left, Right> {}
    impl<Left, Right: ?Sized> Hash for TypeIs<Left, Right> {
        fn hash<H: std::hash::Hasher>(&self, _state: &mut H) {}
    }
}
