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

use crate::{
    index::SectionId,
    read::{FuncTypeId, FunctionRef, GlobalRef, TableRef},
    symbols::SymbolId,
};

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
    MemoryAddr(RelocationEntry<MemoryAddr>),
    TableNumber(RelocationEntry<TableRef>),
    GlobalIndex(RelocationEntry<GlobalRef>),
    FunctionIndex(RelocationEntry<FunctionRef>),
    // Indirect function index used in call_indirect
    TableIndex(RelocationEntry<IndirectFunctionIndex>),
    FunctionOffset(RelocationEntry<FunctionOffset>),
    SectionOffset(RelocationEntry<SectionOffset>),
    EventIndex(RelocationEntry<EventIndex>),
    MemoryAddrLoc(RelocationEntry<MemoryAddrLoc>),
    TypeIndex(RelocationEntry<FuncTypeId>),
}

///
/// Implementation of relocation entry for some typed index.
///
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub struct RelocationEntry<Indx: Constrains> {
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

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub struct EventIndex {
    index: u32,
}

/// Addr of Chunk in memory
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
#[repr(packed)]
pub struct MemoryAddr {
    pub addend: i64,
    pub mem_chunk_id: u32, // data chunk ref?
}

/// Addr of Chunk in segment?
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub struct MemoryAddrLoc(MemoryAddr);

/// Place in function code
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
#[repr(packed)] // always first element, so can be unaligned
pub struct FunctionOffset {
    pub addend: i64,
    pub function: FunctionRef,
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
#[repr(packed)] // always first element, so can be unaligned
pub struct SectionOffset {
    pub addend: i64,
    pub section: SectionId,
}

// == Type safety markers and traits ==

/// Each type can mark associated types either `Yes` or `No` for each of associated types.
/// Every `No` marker enforces that enum variant with this constraint cannot be constructed.
pub trait Constrains {
    type RelTls: Debug + Eq + Hash + Copy + SealedConstrain;
    type RelBase: Debug + Eq + Hash + Copy + SealedConstrain;
    type Has64: Debug + Eq + Hash + Copy + SealedConstrain;
    type Int: Debug + Eq + Hash + Copy + SealedConstrain;
    type Sleb: Debug + Eq + Hash + Copy + SealedConstrain;
    type Leb: Debug + Eq + Hash + Copy + SealedConstrain;
    // type Addend;?
}
use impl_type_safety::SealedConstrain;

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub struct Has;

mod impl_type_safety {

    pub trait SealedConstrain {}
    use super::*;

    impl SealedConstrain for Yes {}
    impl SealedConstrain for No {}
    type Yes = Has;
    type No = std::convert::Infallible;
    macro_rules! constrain {
        ($idx:ty { $($cap:ident),* $(,)? }) => {
            impl Constrains for $idx {
                type RelTls = constrain!(@has RelTls ; $($cap),*);
                type RelBase = constrain!(@has RelBase ; $($cap),*);
                type Has64 = constrain!(@has Has64 ; $($cap),*);
                type Int = constrain!(@has Int ; $($cap),*);
                type Sleb = constrain!(@has Sleb ; $($cap),*);
                type Leb = constrain!(@has Leb ; $($cap),*);
            }
        };

        (@has RelTls; RelTls $(, $rest:ident)*) => { Yes };
        (@has RelBase; RelBase $(, $rest:ident)*) => { Yes };
        (@has Has64; Has64 $(, $rest:ident)*) => { Yes };
        (@has Int; Int $(, $rest:ident)*) => { Yes };
        (@has Sleb; Sleb $(, $rest:ident)*) => { Yes };
        (@has Leb; Leb $(, $rest:ident)*) => { Yes };

        (@has $_any: ident; $_other:ident $(, $rest:ident)*) => { constrain!(@has $_any; $($rest),*) };
        // Final
        (@has $_any: ident; ) => { No };
    }

    constrain!(MemoryAddr {
        RelTls,
        RelBase,
        Has64,
        Int,
        Sleb,
        Leb
    });
    constrain!(IndirectFunctionIndex {
        RelBase,
        Has64,
        Int,
        Sleb,
        Leb
    });
    constrain!(LinkageSymbol {
        RelTls,
        RelBase,
        Has64,
        Int,
        Sleb,
        Leb
    });
    constrain!(FunctionRef { Leb, Int });
    constrain!(FuncTypeId { Leb });
    constrain!(TableRef { Leb });
    constrain!(GlobalRef { Leb, Int });
    constrain!(FunctionOffset { Int, Has64 });
    constrain!(SectionOffset { Int });
    constrain!(EventIndex { Leb });
    constrain!(MemoryAddrLoc { Int });

    pub trait Upcast<V> {
        fn upcast(self) -> Option<V>;
    }

    impl<T> Upcast<T> for No {
        fn upcast(self) -> Option<T> {
            match self {}
        }
    }

    impl Upcast<Yes> for Yes {
        fn upcast(self) -> Option<Yes> {
            Some(self)
        }
    }

    impl Upcast<No> for Yes {
        fn upcast(self) -> Option<No> {
            None
        }
    }
}
use impl_type_safety::Upcast;

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub enum Encoding<Index: Constrains> {
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
pub enum Relative<Index: Constrains> {
    /// Absolute address
    None(Has),
    /// Symbol relative to `__memory_base` / `__table_base` global
    Got(Index::RelBase),
    /// Symbol relative to `__tls_base` global
    Tls(Index::RelTls),
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub enum RelocationWidth<Index: Constrains> {
    Bits32(Has),
    Bits64(Index::Has64),
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

impl From<wasmparser::RelocationEntry> for LinkageRelocationEntry {
    fn from(entry: wasmparser::RelocationEntry) -> Self {
        use wasmparser::RelocationType::*;
        let index_type = match entry.ty {
            TypeIndexLeb => {
                return LinkageRelocationEntry::Type(RelocationEntry {
                    offset: entry.offset,
                    index: FuncTypeId::from_u32(entry.index),
                    relation: Relative::None(Has),
                    encoding: Encoding::Leb(Has),
                    width: RelocationWidth::Bits32(Has),
                }
            );},
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
            | TableIndexI32 | MemoryAddrI32 | FunctionOffsetI64 | TableIndexI64 | MemoryAddrI64
            | MemoryAddrLocrelI32 => Encoding::Fixed(Has),

            TableIndexRelSleb64 | TableIndexSleb64 | TableIndexRelSleb | TableIndexSleb
            | MemoryAddrLeb64 | MemoryAddrSleb64 | MemoryAddrSleb | MemoryAddrRelSleb
            | MemoryAddrRelSleb64 | MemoryAddrTlsSleb | MemoryAddrTlsSleb64 => Encoding::Sleb(Has),

            FunctionIndexLeb | GlobalIndexLeb | TableNumberLeb | MemoryAddrLeb | EventIndexLeb => {
                Encoding::Leb(Has)
            }

            TypeIndexLeb => unreachable!(),
        };

        let relation = match entry.ty {
            TableIndexRelSleb | TableIndexRelSleb64 | MemoryAddrRelSleb | MemoryAddrRelSleb64 => {
                Relative::Got(Has)
            }
            MemoryAddrTlsSleb64 | MemoryAddrTlsSleb => Relative::Tls(Has),
            SectionOffsetI32 | FunctionOffsetI32 | GlobalIndexI32 | FunctionIndexI32
            | TableIndexI32 | MemoryAddrI32 | FunctionOffsetI64 | TableIndexI64 | MemoryAddrI64
            | TableIndexSleb64 | TableIndexSleb | MemoryAddrLeb64 | MemoryAddrSleb64
            | MemoryAddrSleb | FunctionIndexLeb | GlobalIndexLeb | TableNumberLeb
            | MemoryAddrLeb | EventIndexLeb | MemoryAddrLocrelI32 => Relative::None(Has),
            TypeIndexLeb => unreachable!(),
        };

        let width = match entry.ty {
            MemoryAddrLocrelI32 | SectionOffsetI32 | FunctionOffsetI32 | GlobalIndexI32
            | GlobalIndexLeb | FunctionIndexI32 | FunctionIndexLeb | TableIndexI32
            | TableIndexSleb | TableIndexRelSleb | MemoryAddrI32 | TableNumberLeb
            | EventIndexLeb | MemoryAddrLeb | MemoryAddrSleb | MemoryAddrRelSleb
            | MemoryAddrTlsSleb => RelocationWidth::Bits32(Has),
            FunctionOffsetI64 | TableIndexI64 | TableIndexRelSleb64 | TableIndexSleb64
            | MemoryAddrI64 | MemoryAddrLeb64 | MemoryAddrSleb64 | MemoryAddrRelSleb64
            | MemoryAddrTlsSleb64 => RelocationWidth::Bits64(Has),
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

trait SymbolResolver {
    fn function_ref(&self, symbol: SymbolId) -> Option<FunctionRef>;
    fn global_ref(&self, symbol: SymbolId) -> Option<GlobalRef>;
    fn table_ref(&self, symbol: SymbolId) -> Option<TableRef>;
    fn memory_chunk(&self, symbol: SymbolId) -> Option<u32>;
}

impl TypedRelocationEntry {
    fn from_raw(entry: wasmparser::RelocationEntry, resolver: &dyn SymbolResolver) -> Option<Self> {
        use wasmparser::RelocationType::*;

        macro_rules! build_match {
            ($($idx: ident ($type:ident) => {
                $($prop: ident: $constr: ident),*
                $(,)?
            }),* $(,)?) => {
                {
                    use Relative::*;
                    use Encoding::*;
                    use RelocationWidth::*;
                    match entry.ty {
                        $(
                            $idx => {
                                let sym = build_match!(@bres $type);
                                TypedRelocationEntry::$type (RelocationEntry {
                                    offset: entry.offset,
                                    index: sym,
                                    $($prop: $constr(Has)),*
                                })
                            },
                        )*
                    }
                }
            };
            (@bres FunctionIndex) => {
                resolver.function_ref(SymbolId::from_u32(entry.index))?
            };
            (@bres GlobalIndex) => {
                resolver.global_ref(SymbolId::from_u32(entry.index))?
            };
            (@bres TableIndex) => {
                {
                    let function_ref = resolver.function_ref(SymbolId::from_u32(entry.index))?;
                    IndirectFunctionIndex(function_ref)
                }
            };
            (@bres MemoryAddr) => {
                {
                    let memory_chunk = resolver.memory_chunk(SymbolId::from_u32(entry.index))?;
                    MemoryAddr {
                        addend: entry.addend,
                        mem_chunk_id: memory_chunk,
                    }
                }
            };
            (@bres MemoryAddrLoc) => {
                MemoryAddrLoc (build_match!(@bres MemoryAddr))
            };

            (@bres TypeIndex) => {
                FuncTypeId::from_u32(entry.index)
            };
            (@bres TableNumber) => {
                resolver.table_ref(SymbolId::from_u32(entry.index))?
            };

            (@bres FunctionOffset) => {
                FunctionOffset {
                    addend: entry.addend,
                    function: resolver.function_ref(SymbolId::from_u32(entry.index))?,
                }
            };

            (@bres SectionOffset) => {
                SectionOffset {
                    addend: entry.addend,
                    section: entry.index,
                }
            };
            (@bres EventIndex) => {
                EventIndex {
                    index: entry.index,
                }
            };
        }

        let res = build_match! {
            TypeIndexLeb(TypeIndex) => {
                relation: None,
                encoding: Leb,
                width: Bits32,
            },
            EventIndexLeb(EventIndex) => {
                relation: None,
                encoding: Leb,
                width: Bits32,
            },
            MemoryAddrLocrelI32 (MemoryAddrLoc) => {
                relation: None,
                encoding: Fixed,
                width: Bits32,
            },
            SectionOffsetI32 (SectionOffset) => {
                relation: None,
                encoding: Fixed,
                width: Bits32,
            },
            FunctionOffsetI32 (FunctionOffset) => {
                relation: None,
                encoding: Fixed,
                width: Bits32,
            },
            FunctionOffsetI64 (FunctionOffset) => {
                relation: None,
                encoding: Fixed,
                width: Bits64,
            },
            TableNumberLeb (TableNumber) => {
                relation: None,
                encoding: Leb,
                width: Bits32,
            },
            FunctionIndexI32 (FunctionIndex) => {
                relation: None,
                encoding: Fixed,
                width: Bits32,
            },
            FunctionIndexLeb (FunctionIndex) => {
                relation: None,
                encoding: Leb,
                width: Bits32,
            },
            GlobalIndexI32 (GlobalIndex) => {
                relation: None,
                encoding: Fixed,
                width: Bits32,
            },
            GlobalIndexLeb (GlobalIndex) => {
                relation: None,
                encoding: Leb,
                width: Bits32,
            },
            TableIndexI32 (TableIndex) => {
                relation: None,
                encoding: Fixed,
                width: Bits32,
            },
            TableIndexSleb (TableIndex) => {
                relation: None,
                encoding: Sleb,
                width: Bits32,
            },
            TableIndexI64 (TableIndex) => {
                relation: None,
                encoding: Fixed,
                width: Bits64,
            },
            TableIndexSleb64 (TableIndex) => {
                relation: None,
                encoding: Sleb,
                width: Bits64,
            },
            TableIndexRelSleb (TableIndex) => {
                relation: Got,
                encoding: Sleb,
                width: Bits32,
            },
            TableIndexRelSleb64 (TableIndex) => {
                relation: Got,
                encoding: Sleb,
                width: Bits64,
            },
            MemoryAddrI32 (MemoryAddr) => {
                relation: None,
                encoding: Fixed,
                width: Bits32,
            },
            MemoryAddrI64 (MemoryAddr) => {
                relation: None,
                encoding: Fixed,
                width: Bits64,
            },
            MemoryAddrLeb (MemoryAddr) => {
                relation: None,
                encoding: Leb,
                width: Bits32
            },
            MemoryAddrLeb64 (MemoryAddr) => {
                relation: None,
                encoding: Leb,
                width: Bits64,
            },
            MemoryAddrSleb (MemoryAddr) => {
                relation: None,
                encoding: Sleb,
                width: Bits32
            },
            MemoryAddrSleb64 (MemoryAddr) => {
                relation: None,
                encoding: Sleb,
                width: Bits64
            },
            MemoryAddrRelSleb(MemoryAddr) => {
                relation: Got,
                encoding: Sleb,
                width: Bits32,
            },
            MemoryAddrRelSleb64(MemoryAddr) => {
                relation: Got,
                encoding: Sleb,
                width: Bits64,
            },
            MemoryAddrTlsSleb(MemoryAddr) => {
                relation: Tls,
                encoding: Sleb,
                width: Bits32,
            },
            MemoryAddrTlsSleb64(MemoryAddr) => {
                relation: Tls,
                encoding: Sleb,
                width: Bits64,
            }
        };
        Some(res)
    }
}

// Upcast between different constrains
// Make sure that type support needed constrains.
trait CheckConstrains<Suitable> {
    fn check(self) -> Option<Suitable>;
}

impl<C1, C2> CheckConstrains<Encoding<C2>> for Encoding<C1>
where
    C1: Constrains,
    C2: Constrains,
    C1::Int: Upcast<C2::Int>,
    C1::Sleb: Upcast<C2::Sleb>,
    C1::Leb: Upcast<C2::Leb>,
{
    fn check(self) -> Option<Encoding<C2>> {
        match self {
            Encoding::Fixed(v) => Some(Encoding::Fixed(v.upcast()?)),
            Encoding::Sleb(v) => Some(Encoding::Sleb(v.upcast()?)),
            Encoding::Leb(v) => Some(Encoding::Leb(v.upcast()?)),
        }
    }
}

impl<C1, C2> CheckConstrains<Relative<C2>> for Relative<C1>
where
    C1: Constrains,
    C2: Constrains,
    C1::RelBase: Upcast<C2::RelBase>,
    C1::RelTls: Upcast<C2::RelTls>,
{
    fn check(self) -> Option<Relative<C2>> {
        match self {
            Relative::None(v) => Some(Relative::None(v.upcast()?)),
            Relative::Got(v) => Some(Relative::Got(v.upcast()?)),
            Relative::Tls(v) => Some(Relative::Tls(v.upcast()?)),
        }
    }
}

impl<C1, C2> CheckConstrains<RelocationWidth<C2>> for RelocationWidth<C1>
where
    C1: Constrains,
    C2: Constrains,
    C1::Has64: Upcast<C2::Has64>,
{
    fn check(self) -> Option<RelocationWidth<C2>> {
        match self {
            RelocationWidth::Bits32(v) => Some(RelocationWidth::Bits32(v.upcast()?)),
            RelocationWidth::Bits64(v) => Some(RelocationWidth::Bits64(v.upcast()?)),
        }
    }
}

// Check size compatibility with wasmparser::RelocationEntry
const _ASSERT_SIZE: () = const {
    use std::mem::{align_of, size_of};
    assert!(size_of::<LinkageRelocationEntry>() <= size_of::<wasmparser::RelocationEntry>());
    assert!(align_of::<LinkageRelocationEntry>() <= align_of::<wasmparser::RelocationEntry>());
    assert!(size_of::<TypedRelocationEntry>() <= size_of::<wasmparser::RelocationEntry>());
    assert!(align_of::<TypedRelocationEntry>() <= align_of::<wasmparser::RelocationEntry>());
};

#[cfg(test)]
mod tests {
    #[test]
    fn runtime_assert_size() {
        use std::mem::{align_of, size_of};
        println!(
            "Size of LinkageRelocationEntry: {}",
            size_of::<super::LinkageRelocationEntry>()
        );
        println!(
            "Size of TypedRelocationEntry: {}",
            size_of::<super::TypedRelocationEntry>()
        );

        assert!(
            size_of::<super::LinkageRelocationEntry>() <= size_of::<wasmparser::RelocationEntry>()
        );
        assert!(
            align_of::<super::LinkageRelocationEntry>()
                <= align_of::<wasmparser::RelocationEntry>()
        );
        assert!(
            size_of::<super::TypedRelocationEntry>() <= size_of::<wasmparser::RelocationEntry>()
        );
        assert!(
            align_of::<super::TypedRelocationEntry>() <= align_of::<wasmparser::RelocationEntry>()
        );
    }
}
