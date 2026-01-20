//! Decomposed variant of relocation entries.
//!
//! For simplifcation relocation can be represented as next pseudocode:
//! ```compile_fail
//!  pointer = writter.offset_of(def_sym) + .offset;
//!  *pointer = encode::<Encoding>(addr_of(.sym) + sym.addend, width);
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
//! In this module we are trying to utilise this decomposed form.
//! Not all symbol indexes can be encoded as all combinations of relation/encoding/width.
//! Originally there was type-safe implementation that uses GADT like structure to force type-safe constraints for each of `RelocationEntry`
//! type.
//!
//! The experiment of type-safe relocation can be seen at commit:"978d259765c19daf1594c67a8465ec175f1e4f7a" and 6d536209516e3fc7946d955ecd65eb68974ca139
//! Both of variants looks non-usable and verbose, for implementing simple logic on top of them.
//!
//! Instead in current design constraints fo index types are enforced in runtime in From/Into implementations.
//!

use std::{fmt::Debug, hash::Hash};

use crate::{
    index::SectionId,
    read::{FuncTypeId, FunctionRef},
    symbols::SymbolId,
};

/// Lossless representation of `wasmparser::RelocationEntry` with type-safe disamiguation of symbol types.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub enum AnyRelocationEntry {
    Linkage(RelocationEntry),
    Type(TypeRelocationEntry),
}

impl AnyRelocationEntry {
    /// Get symbol index if this is linkage relocation entry.
    pub fn symbol_id(&self) -> Option<SymbolId> {
        match self {
            AnyRelocationEntry::Linkage(reloc) => Some(reloc.symbol_id),
            AnyRelocationEntry::Type(_) => None,
        }
    }
    /// Get offset of relocation entry within the containing symbol.
    pub fn offset(&self) -> u32 {
        match self {
            AnyRelocationEntry::Linkage(reloc) => reloc.offset,
            AnyRelocationEntry::Type(reloc) => reloc.offset,
        }
    }
    /// Set offset of relocation entry within the containing symbol.
    pub fn set_offset(&mut self, new_offset: u32) {
        match self {
            AnyRelocationEntry::Linkage(reloc) => reloc.offset = new_offset,
            AnyRelocationEntry::Type(reloc) => reloc.offset = new_offset,
        }
    }
    /// Get linkage relocation entry if applicable.
    pub fn linkage(&self) -> Option<&RelocationEntry> {
        match self {
            AnyRelocationEntry::Linkage(reloc) => Some(reloc),
            AnyRelocationEntry::Type(_) => None,
        }
    }
    /// Return range of bytes in the containing symbol that should be modified by this relocation.
    pub fn relocation_range(&self) -> std::ops::Range<usize> {
        let start = self.offset() as usize;
        let len = match self {
            AnyRelocationEntry::Type(_) => {
                5 // always leb32
            }
            AnyRelocationEntry::Linkage(reloc) => reloc.extent(),
        };

        start..(start + len)
    }
}

/// Implementation of relocation entry for function `type` index.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub struct TypeRelocationEntry {
    pub offset: u32,
    pub index: FuncTypeId,
    // pub addend: i64, // not applicable for type relocations
    // pub relation: Relative, // not applicable for type relocations
    // pub encoding: Encoding, // leb
    // pub width: RelocationWidth, // 32
}

///
/// Implementation of relocation entry type defined in linker symbols table.
/// Generic index type allows to split resolution of symbol index to typed entity id from the relocation application.
///
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub struct RelocationEntry<Index = SymbolId> {
    /// Optional addend to be added to the resulting value.
    pub addend: i64,
    /// Index of symbol in `Symbols` table that store information about relocated symbol.
    /// This is generic due to fact that type relocations has no `SymbolId`
    pub symbol_id: Index,
    /// Offset in bytes from the start of the symbol definition
    /// targeted by this relocation.
    pub offset: u32,
    /// Type of symbol stored in `Symbols` table.
    pub symbol_type: SymbolType,
    /// Information about global variable base, if this is position independent relocation.
    pub relation: Relative,
    /// Representation of resulting value in the output binary.
    /// Either Sleb/Leb or fixed integer.
    pub encoding: Encoding,
    /// Width of encoding result value (64 or 32 bit)
    pub width: RelocationWidth,
}

impl<Index> RelocationEntry<Index> {
    pub fn relocation_range(&self) -> std::ops::Range<usize> {
        let start = self.offset as usize;
        let len = self.extent();
        start..(start + len)
    }

    pub fn extent(&self) -> usize {
        match (self.encoding, self.width) {
            (Encoding::Fixed, RelocationWidth::Bits32) => 4,
            (Encoding::Fixed, RelocationWidth::Bits64) => 8,
            (Encoding::Sleb | Encoding::Leb, RelocationWidth::Bits32) => 5,
            (Encoding::Sleb | Encoding::Leb, RelocationWidth::Bits64) => 10,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
/// Enumeration of all symbols that can be stored in `Symbols` table.
pub enum SymbolType {
    MemoryAddr,
    TableNumber,
    GlobalIndex,
    FunctionIndex,
    // Indirect function index used in call_indirect
    TableIndex,
    FunctionOffset,
    SectionOffset,
    EventIndex,
    MemoryAddrLocrel,
    // Not supported in `AnyRelocationEntry::Linkage` because type is not placed as symbol in symbols table.
    TypeIndex,
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
#[repr(transparent)]
pub struct Addend {
    pub value: i64,
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub enum Encoding {
    // 4-byte little-endian integer
    // e.g. `uint32` or `int32`
    Fixed,
    // 5-byte Variable-length SIGNED integer
    // 32-bit SLEB128
    Sleb,
    // 5-byte Variable-length UNSIGNED integer
    // 32-bit ULEB128
    Leb,
}

/// Base of addr/index is stored can be stored in global variable.
/// This enum indicates which variable stores this base.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub enum Relative {
    /// Absolute address
    None,
    /// Symbol relative to `__memory_base` / `__table_base` global
    Got,
    /// Symbol relative to `__tls_base` global
    Tls,
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub enum RelocationWidth {
    Bits32,
    Bits64,
}

// Is not used anymore - but during implementation we highlighted what entities are actually used
//  - therefore let it now live in comment
// trait SymbolResolver {
//     fn function_ref(&self, symbol: SymbolId) -> Option<FunctionRef>;
//     fn global_ref(&self, symbol: SymbolId) -> Option<GlobalRef>;
//     fn table_ref(&self, symbol: SymbolId) -> Option<TableRef>;
//     fn memory_chunk(&self, symbol: SymbolId) -> Option<u32>;
// }

impl AnyRelocationEntry {
    pub fn from_raw(entry: wasmparser::RelocationEntry, symbol_start: u32) -> Self {
        use wasmparser::RelocationType::*;
        let symbol_type = match entry.ty {
            TypeIndexLeb => {
                return AnyRelocationEntry::Type(TypeRelocationEntry {
                    offset: entry.offset - symbol_start,
                    index: FuncTypeId::from_u32(entry.index),
                });
            }
            EventIndexLeb => SymbolType::EventIndex,
            TableNumberLeb => SymbolType::TableNumber,
            SectionOffsetI32 => SymbolType::SectionOffset,
            FunctionOffsetI32 | FunctionOffsetI64 => SymbolType::FunctionOffset,
            GlobalIndexI32 | GlobalIndexLeb => SymbolType::GlobalIndex,
            FunctionIndexLeb | FunctionIndexI32 => SymbolType::FunctionIndex,
            TableIndexSleb | TableIndexI32 | TableIndexI64 | TableIndexRelSleb
            | TableIndexRelSleb64 | TableIndexSleb64 => SymbolType::TableIndex,
            MemoryAddrLocrelI32 => SymbolType::MemoryAddrLocrel,
            MemoryAddrI32 | MemoryAddrLeb | MemoryAddrSleb | MemoryAddrRelSleb
            | MemoryAddrTlsSleb | MemoryAddrI64 | MemoryAddrLeb64 | MemoryAddrSleb64
            | MemoryAddrRelSleb64 | MemoryAddrTlsSleb64 => SymbolType::MemoryAddr,
        };
        let encoding: Encoding = match entry.ty {
            SectionOffsetI32 | FunctionOffsetI32 | GlobalIndexI32 | FunctionIndexI32
            | TableIndexI32 | MemoryAddrI32 | FunctionOffsetI64 | TableIndexI64 | MemoryAddrI64
            | MemoryAddrLocrelI32 => Encoding::Fixed,
            TableIndexRelSleb64 | TableIndexSleb64 | TableIndexRelSleb | TableIndexSleb
            | MemoryAddrLeb64 | MemoryAddrSleb64 | MemoryAddrSleb | MemoryAddrRelSleb
            | MemoryAddrRelSleb64 | MemoryAddrTlsSleb | MemoryAddrTlsSleb64 => Encoding::Sleb,
            FunctionIndexLeb | GlobalIndexLeb | TableNumberLeb | MemoryAddrLeb | EventIndexLeb => {
                Encoding::Leb
            }
            TypeIndexLeb => unreachable!(),
        };

        let relation = match entry.ty {
            TableIndexRelSleb | TableIndexRelSleb64 | MemoryAddrRelSleb | MemoryAddrRelSleb64 => {
                Relative::Got
            }
            MemoryAddrTlsSleb64 | MemoryAddrTlsSleb => Relative::Tls,
            SectionOffsetI32 | FunctionOffsetI32 | GlobalIndexI32 | FunctionIndexI32
            | TableIndexI32 | MemoryAddrI32 | FunctionOffsetI64 | TableIndexI64 | MemoryAddrI64
            | TableIndexSleb64 | TableIndexSleb | MemoryAddrLeb64 | MemoryAddrSleb64
            | MemoryAddrSleb | FunctionIndexLeb | GlobalIndexLeb | TableNumberLeb
            | MemoryAddrLeb | EventIndexLeb | MemoryAddrLocrelI32 => Relative::None,
            TypeIndexLeb => unreachable!(),
        };

        let width = match entry.ty {
            MemoryAddrLocrelI32 | SectionOffsetI32 | FunctionOffsetI32 | GlobalIndexI32
            | GlobalIndexLeb | FunctionIndexI32 | FunctionIndexLeb | TableIndexI32
            | TableIndexSleb | TableIndexRelSleb | MemoryAddrI32 | TableNumberLeb
            | EventIndexLeb | MemoryAddrLeb | MemoryAddrSleb | MemoryAddrRelSleb
            | MemoryAddrTlsSleb => RelocationWidth::Bits32,
            FunctionOffsetI64 | TableIndexI64 | TableIndexRelSleb64 | TableIndexSleb64
            | MemoryAddrI64 | MemoryAddrLeb64 | MemoryAddrSleb64 | MemoryAddrRelSleb64
            | MemoryAddrTlsSleb64 => RelocationWidth::Bits64,
            TypeIndexLeb => unreachable!(),
        };

        Self::Linkage(RelocationEntry {
            offset: entry.offset - symbol_start,
            addend: entry.addend,
            symbol_id: SymbolId::from_u32(entry.index),
            symbol_type,
            relation,
            encoding,
            width,
        })
    }
}

// // Check size compatibility with wasmparser::RelocationEntry
const _ASSERT_SIZE: () = const {
    // Because type entry doesn't have addend - enum tag can be packed and resulting size remains equal to non decomposed version.
    assert!(size_of::<RelocationEntry>() <= size_of::<wasmparser::RelocationEntry>());
    assert!(align_of::<RelocationEntry>() <= align_of::<wasmparser::RelocationEntry>());
};

#[cfg(test)]
mod tests {

    #[test]
    fn runtime_assert_size() {
        use std::mem::{align_of, size_of};
        println!(
            "Size of LinkageRelocation: {}",
            size_of::<super::RelocationEntry>()
        );

        println!(
            "Size of wasmparser::RelocationEntry: {}",
            size_of::<wasmparser::RelocationEntry>()
        );
        assert!(size_of::<super::RelocationEntry>() <= size_of::<wasmparser::RelocationEntry>());
        assert!(align_of::<super::RelocationEntry>() <= align_of::<wasmparser::RelocationEntry>());
    }
}
