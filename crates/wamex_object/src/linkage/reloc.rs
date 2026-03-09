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
    helpers::RangeExt,
    index::SectionId,
    typed::{FnTypeRef, FunctionRef, SymbolId, common_index::EntityKind},
};

/// Index of symbol in `Symbols` table that store information about relocated symbol.
/// This is generic due to fact that type relocations has no `SymbolId`
///
/// Type of symbol stored in `Symbols` table.
pub type LinkageRelocationEntry = RelocationEntry<SymbolId, SymbolType>;

pub type EntityRelocationEntry = RelocationEntry<EntityKind, EntityAddressMode>;

/// Lossless representation of `wasmparser::RelocationEntry` with type-safe disamiguation of symbol types.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub enum AnyRelocationEntry {
    Linkage(LinkageRelocationEntry),
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
    pub fn linkage(&self) -> Option<&LinkageRelocationEntry> {
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct TypeRelocationEntry {
    pub offset: u32,
    pub index: FnTypeRef,
    // pub addend: i64, // not applicable for type relocations
    // pub relation: Relative, // not applicable for type relocations
    // pub encoding: Encoding, // leb
    // pub width: RelocationWidth, // 32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct LinkageSymbol {
    /// Index of symbol in `Symbols` table that store information about relocated symbol.
    /// This is generic due to fact that type relocations has no `SymbolId`
    pub id: SymbolId,
    /// Type of symbol stored in `Symbols` table.
    pub ty: SymbolType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub enum EntityAddressMode {
    /// Index of entity in their index space, e.g. function/global/event/table index.
    StaticIndex,
    /// Addr of entity in runtime, e.g. addr of memory chunk, or index of table in `__indirect_function_table`.
    RuntimeAddr,
    /// Offset of entity definition in wasm file, e.g. offset of function/section
    FileOffset,
    /// Index of base for entity, e.g. global index for GOT/TLS based addressing.
    // TODO: Memory index in case of multiple memories.
    // TODO: Should be combined with entry Relative?.
    BaseStaticIndex,
}
impl EntityAddressMode {
    pub fn from_llvm_relocs(entity_kind: EntityKind, reloc_ty: SymbolType) -> Self {
        match (&entity_kind, reloc_ty) {
            (EntityKind::Function(_), SymbolType::FunctionIndex)
            | (EntityKind::Global(_), SymbolType::GlobalIndex)
            | (EntityKind::Table(_), SymbolType::TableNumber) => EntityAddressMode::StaticIndex,
            (EntityKind::DataSymbol(_), SymbolType::MemoryAddr)
            | (EntityKind::Function(_), SymbolType::TableIndex) => EntityAddressMode::RuntimeAddr,
            (EntityKind::Function(_), SymbolType::FunctionOffset) => EntityAddressMode::FileOffset,
            (EntityKind::Memory(_) | EntityKind::Tag(_), _) => {
                panic!("Unsupported entity ref for relocation")
            }
            _ => panic!("Mismatched entity ref and symbol type"),
        }
    }
}

///
/// Implementation of relocation entry type defined in linker symbols table.
/// Generic Symbols allows to map SymbolId to EntityRef and
/// decompose work with relocation into two parts:
/// - resolution of symbol index to typed entity_id
/// - application of symbol offset.
///
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct RelocationEntry<SymbolIndex, SymbolOp> {
    /// Optional addend to be added to the resulting value.
    pub addend: i64,
    /// Offset in bytes from the start of the symbol definition
    /// targeted by this relocation.
    pub offset: u32,
    /// Symbol implementation representing reference relocated entity.
    pub symbol_id: SymbolIndex,
    /// Extra information about symbol, e.g. type of symbol in symbol table, or addressing mode of symbol.
    pub symbol_op: SymbolOp,
    /// Information about global variable base, if this is position independent relocation.
    pub relation: Relative,
    /// Representation of resulting value in the output binary.
    /// Either Sleb/Leb or fixed integer.
    pub encoding: Encoding,
    /// Width of encoding result value (64 or 32 bit)
    pub width: RelocationWidth,
}

impl<Index, Op> RelocationEntry<Index, Op> {
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
impl EntityRelocationEntry {
    pub fn index_base(offset_of_got: u32, symbol_ref: EntityKind) -> Self {
        Self {
            // entity should have information about GOT they used, since there maybe more than one.
            symbol_id: symbol_ref,
            symbol_op: EntityAddressMode::BaseStaticIndex,
            offset: offset_of_got,
            encoding: Encoding::Leb,
            width: RelocationWidth::Bits32,
            relation: Relative::None,
            addend: 0,
        }
    }
    pub fn runtime_addr(place_for_adddr: u32, symbol_ref: EntityKind, is_got: bool) -> Self {
        // only data or fn can have runtime addr
        debug_assert!(symbol_ref.is_function() || symbol_ref.is_data());
        Self {
            symbol_id: symbol_ref,
            symbol_op: EntityAddressMode::RuntimeAddr,
            offset: place_for_adddr,
            encoding: Encoding::Sleb,
            width: RelocationWidth::Bits32,
            relation: if is_got {
                Relative::Got
            } else {
                Relative::None
            },
            addend: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
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
    // Not supported in `AnyRelocationEntry::Linkage` because type is not placed as symbol in symbols table.
    TypeIndex,
}

// == Extra typed indexes ==
// This extra typed indexes currently cannot be constructed, and only used as type level tag -
// therefore don't need actual fields, but if at any future development we would need some of this type - we will have them (but probably in other places).

/// Wrapper around `FunctionRef`, that instead of giving function index - gives index function in `__indirect_function_table`
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct IndirectFunctionIndex(pub FunctionRef);

/// Representation of wasm `event` index
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct EventIndex {
    pub index: u32,
}

/// Addr of Chunk in memory
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct MemoryAddr {
    pub mem_chunk_id: u32, // data chunk ref?
}

/// Addr of Chunk in segment?
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct MemoryAddrLoc(MemoryAddr);

/// Place in function code
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct FunctionOffset {
    pub function: FunctionRef,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
#[repr(Rust, packed)] // always first element, so can be unaligned
pub struct SectionOffset {
    pub section: SectionId,
}

/// Value that should be added to relocated address/index.
/// For functions/globals/events - addend is not applicable.
/// For memory addresses and offsets - addend is either
/// 32 or 64 bit integer that added to resulting address.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
#[repr(transparent)]
pub struct Addend {
    pub value: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub enum Relative {
    /// Absolute address
    None,
    /// Symbol relative to `__memory_base` / `__table_base` global
    Got,
    /// Data Symbol relative to `__tls_base` global
    Tls,
    /// Data Symbol relative to it's location in memory (addr = addend - offset)
    LocRel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
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
    pub fn from_raw(entry: wasmparser::RelocationEntry, entry_offset: isize) -> Self {
        use wasmparser::RelocationType::*;
        let symbol_type = match entry.ty {
            TypeIndexLeb => {
                return AnyRelocationEntry::Type(TypeRelocationEntry {
                    offset: (entry.offset as isize + entry_offset) as u32,
                    index: FnTypeRef::from_u32(entry.index),
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
            MemoryAddrLocrelI32 | MemoryAddrI32 | MemoryAddrLeb | MemoryAddrSleb
            | MemoryAddrRelSleb | MemoryAddrTlsSleb | MemoryAddrI64 | MemoryAddrLeb64
            | MemoryAddrSleb64 | MemoryAddrRelSleb64 | MemoryAddrTlsSleb64 => {
                SymbolType::MemoryAddr
            }
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
            | MemoryAddrLeb | EventIndexLeb => Relative::None,
            MemoryAddrLocrelI32 => Relative::LocRel,
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
            offset: (entry.offset as isize + entry_offset) as u32,
            addend: entry.addend,
            symbol_id: SymbolId::from_u32(entry.index),
            symbol_op: symbol_type,
            relation,
            encoding,
            width,
        })
    }
}

impl RangeExt for AnyRelocationEntry {
    fn shift_left(&self, offset: usize) -> Self {
        let mut modified = *self;
        let new_offset = self.offset().checked_sub(offset as u32).unwrap();
        modified.set_offset(new_offset);
        modified
    }

    fn shift_right(&self, offset: usize) -> Self {
        let mut modified = *self;
        let new_offset = modified.offset() + offset as u32;
        modified.set_offset(new_offset);
        modified
    }
}

impl<Any: Clone, Other: Clone> RangeExt for RelocationEntry<Any, Other> {
    fn shift_left(&self, offset: usize) -> Self {
        let mut modified = self.clone();
        modified.offset = modified.offset.checked_sub(offset as u32).unwrap();
        modified
    }
    fn shift_right(&self, offset: usize) -> Self {
        let mut modified = self.clone();
        modified.offset += offset as u32;
        modified
    }
}

// Check size compatibility with wasmparser::RelocationEntry
const _ASSERT_SIZE: () = const {
    // Because type entry doesn't have addend - enum tag can be packed and resulting size remains equal to non decomposed version.
    assert!(size_of::<LinkageRelocationEntry>() <= size_of::<wasmparser::RelocationEntry>());
    assert!(align_of::<LinkageRelocationEntry>() <= align_of::<wasmparser::RelocationEntry>());

    assert!(size_of::<EntityRelocationEntry>() <= size_of::<wasmparser::RelocationEntry>());
    assert!(align_of::<EntityRelocationEntry>() <= align_of::<wasmparser::RelocationEntry>());
};

#[cfg(test)]
mod tests {

    #[test]
    fn runtime_assert_size() {
        use std::mem::{align_of, size_of};
        println!(
            "Size of LinkageRelocation: {}",
            size_of::<super::LinkageRelocationEntry>()
        );

        println!(
            "Size of wasmparser::RelocationEntry: {}",
            size_of::<wasmparser::RelocationEntry>()
        );
        assert!(
            size_of::<super::LinkageRelocationEntry>() <= size_of::<wasmparser::RelocationEntry>()
        );

        assert!(
            align_of::<super::LinkageRelocationEntry>()
                <= align_of::<wasmparser::RelocationEntry>()
        );
    }
}
