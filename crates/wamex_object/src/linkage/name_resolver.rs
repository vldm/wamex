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

use std::borrow::Cow;

use crate::typed::{EntityKind, FileId};

#[derive(Clone, Copy, Debug)]
enum SymbolBinding {
    Weak,
    Local,
    Default,
}

#[derive(Clone, Copy, Debug)]
pub struct SymbolRecord {
    file: FileId,
    chosen_entity: EntityKind,
    binding: SymbolBinding,
    visible: bool,
    no_strip: bool,
    // other_entities: Vec<TaggedEntityRef>,
}

#[derive(Default, Clone, Debug)]
pub struct NameResolver<'src> {
    names: std::collections::HashMap<Cow<'src, str>, SymbolRecord>,
}

impl<'src> NameResolver<'src> {
    pub fn new() -> Self {
        Self {
            names: std::collections::HashMap::new(),
        }
    }

    pub fn get(&self, name: &str) -> Option<SymbolRecord> {
        self.names.get(name).copied()
    }
}
