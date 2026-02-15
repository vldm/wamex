use std::{borrow::Cow, collections::HashMap, ops::Range};

use anyhow::Result;
use cranelift_entity::{PrimaryMap, packed_option::ReservedValue};
use log::error;
use wasmparser::SymbolInfo;

use crate::{
    ObjectReader,
    index::GappedMap,
    linkage::reloc::{
        AnyRelocationEntry, Encoding, Relative, RelocationEntry, RelocationWidth, SymbolType,
    },
    typed::{
        FnTypeRef, FunctionRef, GlobalRef, Module, SymbolId, TableRef, TagRef,
        common_index::{AnyEntityRef, EntitiesSnapshot, ErasedEntityRef, TaggedEntityRef},
        data::DataDefined,
    },
};

// 1 / WASM_SYM_BINDING_WEAK - Indicating that this is a weak symbol.
//  When linking multiple modules defining the same symbol, all weak
//  definitions are discarded if any strong definitions exist; then
//  if multiple weak definitions exist all but one (unspecified)
//  are discarded; and finally it is an error if more than one definition remains.
// 2 / WASM_SYM_BINDING_LOCAL - Indicating that this is a local symbol
//  (this is exclusive with WASM_SYM_BINDING_WEAK). Local symbols are not
//  to be exported, or linked to other modules/sections.
//  The names of all non-local symbols must be unique, but the names of
//  local symbols are not considered for uniqueness. A local function or global symbol cannot reference an import.
// 4 / WASM_SYM_VISIBILITY_HIDDEN - Indicating that this is a hidden symbol.
//  Hidden symbols are not to be exported when performing the final link, but may be linked to other modules.
// 0x10 / WASM_SYM_UNDEFINED - Indicating that this symbol is not defined.
//  For non-data symbols, this must match whether the symbol is an import or is defined; for data symbols, determines whether a segment is specified.
// 0x20 / WASM_SYM_EXPORTED - The symbol is intended to be exported from  ?DUPLICATE OF EXPORT section?
//  the wasm module to the host environment. This differs from the visibility flags in that it effects the static linker.
// 0x40 / WASM_SYM_EXPLICIT_NAME - The symbol uses an explicit symbol name, ?Only imports
//  rather than reusing the name from a wasm import. This allows it to remap
//  imports from foreign WebAssembly modules into local symbols with different names.
// 0x80 / WASM_SYM_NO_STRIP - The symbol is intended to be included in
//  the linker output, regardless of whether it is used by the program.
//
// 0x100 / WASM_SYM_TLS - The symbol resides in thread local storage.  ?Only data
// 0x200 / WASM_SYM_ABSOLUTE - The symbol represents an absolute address. ?Only data
//  This means it's offset is relative to the start of the wasm memory as opposed to being relative to a data segment.

enum SymbolBinding {
    Weak,
    Local,
    Default,
}

pub struct NameResolver<'src> {
    names: std::collections::HashMap<Cow<'src, str>, AnyEntityRef>,
}

impl<'src> NameResolver<'src> {
    pub fn new() -> Self {
        Self {
            names: std::collections::HashMap::new(),
        }
    }

    pub fn get(&self, name: &str) -> Option<AnyEntityRef> {
        self.names.get(name).copied()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelocRange {
    pub(super) relocs: Range<usize>,
}

impl ReservedValue for RelocRange {
    fn reserved_value() -> Self {
        Self {
            relocs: Range {
                start: usize::MAX,
                end: usize::MIN,
            },
        }
    }
    fn is_reserved_value(&self) -> bool {
        self.relocs.start == usize::MAX && self.relocs.end == usize::MIN
    }
}

#[derive(Debug)]
pub struct FileRelocs {
    // Relocations ordered by offsets in file.
    //
    // Uses `ErasedEntityRef` as index, since relocation entry
    // already contain type information in external tag `SymbolType`
    array: Box<[RelocationEntry<ErasedEntityRef>]>,

    // In what symbol this relocation is placed
    owners: GappedMap<AnyEntityRef, RelocRange>,
}

impl FileRelocs {
    // Resolve relocations symbols (to corresponding entities).
    pub fn build_relocs(
        file_relocs: impl IntoIterator<Item = AnyRelocationEntry>,
        file_db: &FileSymbolDb,
        owners: GappedMap<AnyEntityRef, RelocRange>,
    ) -> Result<Self> {
        let mut array = file_relocs
            .into_iter()
            .map(|r| match r {
                AnyRelocationEntry::Linkage(l) => {
                    let offset_info = file_db
                        .symbol_entity(l.symbol_id)
                        .expect("Malformed wasm: relocation references unknown symbol");
                    RelocationEntry {
                        offset: l.offset,
                        symbol_id: Self::unwrap_entity_ref(offset_info.entity, l.symbol_type),
                        symbol_type: l.symbol_type,
                        encoding: l.encoding,
                        width: l.width,
                        relation: l.relation,
                        addend: Self::checked_addend_increase(
                            l.addend,
                            offset_info.offset_in_entity,
                            l.symbol_type,
                        ),
                    }
                }
                AnyRelocationEntry::Type(t) => RelocationEntry {
                    offset: t.offset,
                    symbol_id: file_db
                        .resolve_type_id(t.index)
                        .expect("Malformed wasm: relocation references unknown type"),
                    symbol_type: SymbolType::TypeIndex,
                    encoding: Encoding::Leb,
                    width: RelocationWidth::Bits32,
                    relation: Relative::None,
                    addend: 0,
                },
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();

        array.sort_by_key(|e| e.offset);

        Ok(Self { array, owners })
    }

    pub fn get(&self, owner: AnyEntityRef) -> &[RelocationEntry<ErasedEntityRef>] {
        let Some(range) = self.owners.get(owner) else {
            return &[];
        };

        &self.array[range.relocs.clone()]
    }

    pub fn get_mut(&mut self, owner: AnyEntityRef) -> &mut [RelocationEntry<ErasedEntityRef>] {
        let Some(range) = self.owners.get(owner) else {
            return &mut [];
        };
        &mut self.array[range.relocs.clone()]
    }
    /// Convert enum to erased form.
    fn unwrap_entity_ref(src: TaggedEntityRef, symbol_type: SymbolType) -> ErasedEntityRef {
        match (src, symbol_type) {
            (
                TaggedEntityRef::Function(f),
                SymbolType::FunctionIndex | SymbolType::FunctionOffset | SymbolType::TableIndex,
            ) => ErasedEntityRef::from_u32(f.as_u32()),
            (TaggedEntityRef::Global(g), SymbolType::GlobalIndex) => {
                ErasedEntityRef::from_u32(g.as_u32())
            }
            (
                TaggedEntityRef::DataSymbol(d),
                SymbolType::MemoryAddrLocrel | SymbolType::MemoryAddr,
            ) => ErasedEntityRef::from_u32(d.as_u32()),
            (TaggedEntityRef::Table(t), SymbolType::TableNumber) => {
                ErasedEntityRef::from_u32(t.as_u32())
            }
            (TaggedEntityRef::Memory(_) | TaggedEntityRef::Tag(_), _) => {
                panic!("Unsupported entity ref for relocation")
            }
            _ => panic!("Mismatched entity ref and symbol type"),
        }
    }
    // Ensure that addend increase is only applied to valid symbol types
    fn checked_addend_increase(addend: i64, increase: u32, symbol_type: SymbolType) -> i64 {
        match symbol_type {
            // only offsets in memory, or in file can be increased
            SymbolType::SectionOffset
            | SymbolType::FunctionOffset
            | SymbolType::MemoryAddrLocrel
            | SymbolType::MemoryAddr => addend + increase as i64,
            _ if addend != 0 || increase != 0 => {
                panic!("Cannot increase addend for this symbol type")
            }
            _ => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SymbolOffset {
    // We can use `AnyEntityRef` or `TaggedEntityRef` but for first one, we also need track `EntitiesCount`
    // thats why we use `TaggedEntityRef` instead.
    // This also will enhance debugging.
    /// Reference to entity in wasm object.
    pub entity: TaggedEntityRef,
    /// If symbol was merged into another entity, this is the offset in that entity.
    /// Only aplicable for data symbols.
    pub offset_in_entity: u32,
    /// If one symbol replaces definition, previous symbol should be marked as "not used",
    /// to remove "relocs" that targets symbol content.
    /// (Like functionoffset, memoryaddrlocrel, etc).
    pub used_defintion: bool,
}
impl SymbolOffset {
    pub fn new(entity: TaggedEntityRef) -> Self {
        Self {
            entity,
            offset_in_entity: 0,
            used_defintion: true,
        }
    }
}

#[derive(Debug)]
pub struct FileSymbolDb {
    /// Map `SymbolId` from linkage symbol table -> `EntityRef` in wasm object.
    ///
    /// Usecases:
    /// - reloc.* section contain `symbol_id` reference to this table.
    /// But for our needs `EntityRef` is used.
    ///
    pub symbols: PrimaryMap<SymbolId, SymbolOffset>,
}

impl FileSymbolDb {
    /// Get symbol entity by its SymbolId
    #[inline]
    pub fn symbol_entity(&self, symbol_id: SymbolId) -> Option<&SymbolOffset> {
        self.symbols.get(symbol_id)
    }

    /// Get `ErasedEntityRef` for `FuncTypeId`.
    #[inline]
    pub fn resolve_type_id(&self, type_id: FnTypeRef) -> Option<ErasedEntityRef> {
        //TODO: currently not supported - so just copy as is.
        Some(ErasedEntityRef::from_u32(type_id.as_u32()))
    }
}
