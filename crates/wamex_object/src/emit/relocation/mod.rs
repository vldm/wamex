//! During module creation, we copy entities from input modules to output module.
//! Some methods can be patched in the process.
//! But after emission, we need to fix indexes and offsets in output module, so they will point to correct entities and addresses.
//! This process is called relocation.
//!
//! Relocation is performed in 2 steps:
//! 1. Symbol index resolution - remap relocs to point to correct entities. Currently it consist of 2 sub-steps:
//!    1.1. map from entity -> (file, file_relocs) - find file and relocastions of the entity.
//!    1.2. map from (file, entity) -> output entity - find coresponding entity in output module, which will be used for relocation.
//! 2. Offset calculation/encoding - calculate final offset for each relocation and encode it in output module.
//!
//! ## Design notes for wamex-split:
//!
//! Code can be only in current module, all external fns symbols are either:
//! - direct import
//! - trampoline to some indirect import, which in general needs GOT base, but we use fixed offsets in indirect calls
//!   (part of layout externally calculated).
//!
//! Data cannot be imported directly, but we have "imported" markers for consistency.
//! To process imported relocs we need to know memory layout of ALL external modules deps,
//! and we need to import GOT base for each dep module.
//! All access in should be patched from absolute offsets to GOT+offset - this is done in `modify::code_abs_to_got` module.
//! Access in data is converted to dyn relocate by `modify::data_abs_to_dyn` module (with start fn that init data offsets).
//!
//! The main rule for accessing data entities is:
//! - main module, or current module should be referenced by offset
//! - other modules should be converted to GOT + offset.
//!   for this purposes, we need to have `entity -> (got?,offset)` mapping per each module.
//!
//!

// For relocs, we need to know:
// - is it symbol rel based/or absolute.
// - what GOT entry we need.
//   so before processing relocs we need to provide:
//
// Common:
// - Vec<MemLayout> for each module
// - parents module: IndirectFnLayout
//
// Local info:
// - Map<ImportDataSymbolRef, GotRef> // builded with module itself
//

pub mod encode;
pub mod resolver;

use std::ops::Range;

use cranelift_entity::{PrimaryMap, SecondaryMap, packed_option::ReservedValue};

use crate::{
    emit::plan::GotInfo,
    index::GappedMap,
    layouts::{DataSymbolRef, DataSymbolsOffsets, ElementItemId, Offsets},
    linkage::{
        file_db::FileRelocs,
        reloc::{Encoding, EntityAddressMode, EntityRelocationEntry, Relative, RelocationWidth},
    },
    typed::{EntityKind, FileId, FunctionRef, GlobalRef, ImportOrDefined, Module},
};

/// Composite reference to an entity in some file.
#[derive(derive_more::Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[debug("({}, {})", file_id, entity)]
pub struct EntityLocation<Entity = EntityKind> {
    pub file_id: FileId,
    pub entity: Entity,
}

impl<Entity> EntityLocation<Entity> {
    pub fn from_parts(file_id: FileId, entity: Entity) -> Self {
        Self { file_id, entity }
    }
    pub fn other_entity(&self, entity: Entity) -> Self {
        Self {
            file_id: self.file_id,
            entity,
        }
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct FunctionInfo {
    pub code_offset: u32,
    pub indirect_table_index: Option<ElementItemId>,
}

/// Module layout suitable for applying relocates.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModuleLayout {
    pub code_section: Range<usize>,
    pub data_section: Range<usize>,
    /// Mapping from function reference to its offset in code section.
    pub functions_mapping: SecondaryMap<FunctionRef, FunctionInfo>,
    /// Mapping of module data symbols, to their offsets.
    pub data_mapping: DataSymbolsOffsets,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportedDataDep {
    /// Module where data chunk is defined
    pub output_location: Offsets,
    /// GOT base of module where data chunk is defined
    /// None if data chunk is defined in absolute address space (e.g. main module)
    pub got_entry: Option<GlobalRef>,
}

pub struct RelocationState<'any, 'src> {
    current_module: &'any Module<'src>,
    current_module_layout: &'any ModuleLayout,
    current_got: Option<GotInfo>,
    imported_data: GappedMap<DataSymbolRef, ImportedDataDep>,
    // Needed for building ImportedData + Offset relocs
    all_modules_layout: &'any PrimaryMap<FileId, ModuleLayout>,
}

impl<'any, 'src> RelocationState<'any, 'src> {
    pub fn new(
        current_module: &'any Module<'src>,
        current_module_layout: &'any ModuleLayout,
        current_got: Option<GotInfo>,
        imported_data: GappedMap<DataSymbolRef, ImportedDataDep>,
        all_modules_layout: &'any PrimaryMap<FileId, ModuleLayout>,
    ) -> Self {
        Self {
            current_module,
            current_module_layout,
            current_got,
            imported_data,
            all_modules_layout,
        }
    }
    pub fn shift_offsets_and_apply_relocs(&self, module_bytes: &mut [u8], relocs: &mut FileRelocs) {
        //1. fixup relocs ranges (from entity-relative to section-relative)
        self.shift_reloc_offsets(relocs);
        //2. apply code relocs
        let code_relocs = relocs.get_code_section_relocs();
        let code_section = &mut module_bytes[self.current_module_layout.code_section.clone()];
        if log::Level::Debug <= log::max_level() {
            let mut code_start = self.current_module_layout.code_section.start;
            use crate::layouts::data::hexdump::SymbolDebugExt;
            let mut res = String::new();
            crate::layouts::data::hexdump::SectionDebug {
                name: "Code section",
                bytes: code_section,
                relocs: code_relocs,
            }
            .debug_symbol_ext(&mut res, &mut code_start, true);
            log::debug!("Applying code relocs: {res}",);
        }

        self.apply_relocations(code_section, code_relocs);
        //3. apply data relocs
        let data_relocs = relocs.get_data_section_relocs();
        let data_section = &mut module_bytes[self.current_module_layout.data_section.clone()];

        if log::Level::Debug <= log::max_level() {
            use crate::layouts::hexdump::SymbolDebugExt;
            let mut data_start = self.current_module_layout.data_section.start;
            let mut res = String::new();
            crate::layouts::hexdump::SectionDebug {
                name: "Data section",
                bytes: data_section,
                relocs: data_relocs,
            }
            .debug_symbol_ext(&mut res, &mut data_start, true);
            log::debug!("Applying data relocs: {res}",);
        }
        self.apply_relocations(data_section, data_relocs);
    }

    fn shift_reloc_offsets(&self, relocs: &mut FileRelocs) {
        for (onwer, relocs) in relocs.iter_relocs_mut() {
            match onwer {
                EntityKind::Function(func) => {
                    let func_info = self.current_module_layout.functions_mapping[func];
                    for reloc in relocs {
                        reloc.offset += func_info.code_offset;
                    }
                }
                EntityKind::DataSymbol(data_symbol) => {
                    let data_offset = self.current_module_layout.data_mapping[data_symbol]
                        .expect("Relocation refers to data symbol outside of module layout")
                        .offsets
                        .section_offset;
                    for reloc in relocs {
                        reloc.offset += data_offset as u32;
                    }
                }
                e => panic!("Relocation owner of type {e:?} is not supported"),
            }
        }
    }

    fn apply_relocations(&self, section: &mut [u8], relocs: &[EntityRelocationEntry]) {
        for reloc in relocs {
            self.apply_relocation(section, *reloc);
        }
    }
    fn apply_relocation(&self, data: &mut [u8], reloc: EntityRelocationEntry) {
        let target_range = reloc.relocation_range();

        // TODO: Check whitelisted relocation combinations.

        let value = match reloc.symbol_op {
            EntityAddressMode::StaticIndex => {
                debug_assert!(matches!(reloc.relation, Relative::None));
                reloc.symbol_id.to_inner_u32()
            }
            EntityAddressMode::RuntimeAddr => match reloc.symbol_id {
                // for fn import and defined should have entry in indirect function table (if used in call_indirect)
                EntityKind::Function(f) => self.current_module_layout.functions_mapping[f]
                    .indirect_table_index
                    .expect("Relocation refers to function without indirect table entry")
                    .as_u32(),
                EntityKind::DataSymbol(d) => {
                    if self
                        .current_module
                        .extra
                        .mem_layout
                        .get_entity(d)
                        .to_external()
                        .is_some()
                    {
                        self.imported_data
                            .get(d)
                            .expect("Cannot find imported data symbol for relocation: {reloc:?}")
                            .output_location
                            .va_address as u32
                    } else {
                        self.current_module_layout.data_mapping[d]
                            .expect("Relocation refers to data symbol outside of module layout")
                            .offsets
                            .va_address as u32
                    }
                }
                ty => panic!("Relocation for symbol type {ty:?} doesn't have runtime addr"),
            },
            // GlobalIndex of GOT for specific symbol.
            EntityAddressMode::BaseStaticIndex => {
                match reloc.symbol_id {
                    EntityKind::DataSymbol(d) => {
                        let got = match self.current_module.extra.mem_layout.get_entity(d) {
                            ImportOrDefined::Defined(_) => {
                                // if defined then it's our got entry.
                                &self.current_got
                                .as_ref()
                                .expect("Current module doesn't have GOT, but relocation requires it")
                                .memory_base
                            }
                            _ => {
                                // if imported - find it's got.
                                let imported_data = &self.imported_data.get(d).unwrap_or_else(|| {
                                    panic!("Cannot find imported data symbol for relocation: {reloc:?}")
                                });
                                // TODO: find why module that was processed by got converter wasn't found in used_modules.
                                &imported_data
                                    .got_entry
                                    .expect("GOT entry must exist for relocation with base")
                            }
                        };
                        got.as_u32()
                    }
                    EntityKind::Function(f) => {
                        let got = match self.current_module.functions.get_entity(f) {
                            ImportOrDefined::Defined(_) => {
                                // if defined then it's our got entry.
                                &self.current_got
                                .as_ref()
                                .expect("Current module doesn't have GOT, but relocation requires it")
                                .memory_base
                            }
                            _ => {
                                panic!("Got based relocation for func {f:?} not implemented.")
                            }
                        };
                        got.as_u32()
                    }
                    ty => panic!(
                        "Relocation for symbol type {ty:?} cannot have base static index or not implemented."
                    ),
                }
            }
            EntityAddressMode::FileOffset => {
                todo!()
            }
        };

        Self::encode(
            &mut data[target_range],
            (value as i64 + reloc.addend)
                .try_into()
                .expect("Failed to apply addend (overflow)"),
            reloc.encoding,
            reloc.width,
        );
    }

    fn encode(target: &mut [u8], value: u32, encoding: Encoding, width: RelocationWidth) {
        use encode::*;
        match (encoding, width) {
            (Encoding::Fixed, RelocationWidth::Bits32) => {
                encode_u32(value, target.try_into().unwrap())
            }
            (Encoding::Leb, RelocationWidth::Bits32) => {
                encode_leb128_u32_5byte(value, target.try_into().unwrap())
            }
            (Encoding::Sleb, RelocationWidth::Bits32) => {
                encode_leb128_i32_5byte(value as i32, target.try_into().unwrap())
            }
            (Encoding::Fixed, RelocationWidth::Bits64) => {
                encode_u64(value as u64, target.try_into().unwrap())
            }
            (Encoding::Leb, RelocationWidth::Bits64) => {
                encode_leb128_u64_10byte(value as u64, target.try_into().unwrap())
            }
            (Encoding::Sleb, RelocationWidth::Bits64) => {
                encode_leb128_i64_10byte(value as i64, target.try_into().unwrap())
            }
        }
    }
}

impl<E: ReservedValue> ReservedValue for EntityLocation<E> {
    fn is_reserved_value(&self) -> bool {
        self.file_id.is_reserved_value() && self.entity.is_reserved_value()
    }

    fn reserved_value() -> Self {
        EntityLocation {
            file_id: ReservedValue::reserved_value(),
            entity: E::reserved_value(),
        }
    }
}

impl Default for EntityLocation {
    fn default() -> Self {
        Self::reserved_value()
    }
}

impl ReservedValue for ImportedDataDep {
    fn is_reserved_value(&self) -> bool {
        self.got_entry
            .as_ref()
            .is_some_and(ReservedValue::is_reserved_value)
            && self.output_location.is_reserved_value()
    }
    fn reserved_value() -> Self {
        ImportedDataDep {
            got_entry: Some(ReservedValue::reserved_value()),
            output_location: ReservedValue::reserved_value(),
        }
    }
}

impl Default for ImportedDataDep {
    fn default() -> Self {
        Self::reserved_value()
    }
}
