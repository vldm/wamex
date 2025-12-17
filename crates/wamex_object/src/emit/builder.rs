//! Multistage builder for Object file.
//!
//! Phase 1 <create ObjectBuilder>: Declare functions, data/element segments, globals.
//! Phase 2 <ObjectBuilder -> Finalized>: Calculate types, lock functions/globals indexes, finalize data and element segments.
//! Phase 3 <Finalized subroutine>: calculate types, emit (types, functions, tables, memories, globals, exports, start_fn, elem, code, data segments)
//! Phase 4 <Finalized subroutine>: calculate relocations and emit custom sections
//!
//!

use std::collections::BTreeSet;

use anyhow::Result;

use crate::{
    InputObject,
    emit::{
        DefinedFunction, ImportedFunction, SegmentLayout, SubModuleExtra,
        globals::{DefinedGlobal, GlobalImport},
        memory_layout, modify,
    },
    index::{
        DataSegmentId, Id, IdMap, IdMap2, IdVec, IdVec2, ImportsOrDefined, PrimaryKey, SymbolId,
        WithOriginalIndex,
    },
};

///
/// First phase of object creation:
/// At this phase wasm related entities can be declared:
/// - functions (imported or defined)
/// - data chunks (to be placed into data sections under data segments)
/// - global variables imported or defined
///
/// At this phase only imports id are stable, thats why add_imported_* methods return Ids,
pub struct ObjectBuilder<'src> {
    pub globals: ImportsOrDefined<'src, DefinedGlobal<'src>>,
    pub functions: ImportsOrDefined<'src, DefinedFunction>,
    pub data: IdMap2<DataSegmentId, SegmentLayout<'src>>,
}

impl<'src> ObjectBuilder<'src> {
    pub fn new() -> Self {
        Self {
            globals: ImportsOrDefined::new(Vec::new(), Vec::new()),
            functions: ImportsOrDefined::new(Vec::new(), Vec::new()),
            data: IdMap2::new(),
        }
    }

    /// Adds new global variable imported from other module.
    pub fn add_imported_global(
        &mut self,
        global: GlobalImport<'src>,
    ) -> <DefinedGlobal as PrimaryKey>::EntityType {
        self.globals.push_import(global)
    }

    /// Adds new function imported from other module.
    pub fn add_imported_function(
        &mut self,
        func: ImportedFunction<'src>,
    ) -> <DefinedFunction as PrimaryKey>::EntityType {
        self.functions.push_import(func)
    }

    /// Adds new function within this module.
    pub fn add_defined_function(&mut self, func: DefinedFunction) {
        self.functions.defined.push(func)
    }

    /// Adds new global variable defined within this module.
    pub fn add_defined_global(&mut self, global: DefinedGlobal<'src>) {
        self.globals.defined.push(global)
    }

    // TODO: allow define data segment from chunks
    /// Adds a prebuilt data segment
    pub fn add_prebuilt_data_segment(&mut self, segment: SegmentLayout<'src>) {
        self.data.push(segment);
    }

    /// Locks `ObjectBuilder` and produce state with finalized indexes and segments layout.
    /// After this transition no new functions/globals and data chunks can be added.
    /// But function and global indexes are finalized those indexes can be requested for references.
    pub fn lock(
        self,
        // TODO: This context is temporary solution, to keep refactoring scope small (logic just copied as is without structure modification).
        // TO remove this context data segment filling should change.
        ctx: BuilderContextToBeRemoved<'_, 'src>,
    ) -> Object<'src> {
        let mut data_segment_outputs = IdMap2::new();

        let data_segments = &self.data;
        let mem_start = if ctx.is_main() {
            let first_segment = data_segments
                .iter()
                .next()
                .expect("There should be at least one data segment")
                .1;
            first_segment.memory_offset()
        } else {
            0
        };

        // offset of current segment.
        let mut segment_mem_offset = 0;
        log::trace!("Data segments for module: {:#?}", data_segments);
        for (id, segment) in data_segments.iter() {
            let lib_base_global_id = ctx
                .lib_base_import()
                .as_ref()
                .map(|id| id.as_raw_index() as u32);

            let (new_segment_offset, out) =
                segment.to_segment_output(lib_base_global_id, mem_start, segment_mem_offset);
            // TODO: apply relocations to data segment
            segment_mem_offset = new_segment_offset + out.as_raw().len();

            data_segment_outputs.insert(id, out);
        }
        // collect all relocations
        let mut data_relocations = IdMap2::<_, Vec<modify::DataModifyEntry>>::new();

        // TODO: move shift in previous (segment_id, segment) in data_segments.iter()
        for (segment_id, data_segment) in data_segment_outputs.iter() {
            for (symbol_index, sym) in data_segment.symbols() {
                let sym_relocs = ctx
                    .module_info
                    .symbols
                    .get(*symbol_index)
                    .expect("symbol should be valid")
                    .relocs
                    .iter()
                    .map(|reloc| {
                        let relocation_context = modify::RelocationContext {
                            dyn_base: !ctx.is_main()
                                && !ctx.is_static_symbol(SymbolId::from_index(reloc.index)),
                            containing_symbol: Some(modify::DataSymbolWithOffset {
                                storage_segment_id: segment_id,
                                storage_symbol_id: *symbol_index,
                                storage_offset_in_data: reloc.offset, // sym.data_mem_offset as u32,
                            }),
                        };

                        // relocs has offset relative to symbol - update to be relative to segment
                        let mut reloc = reloc.clone();
                        reloc.offset += sym.data_mem_offset as u32;
                        modify::DataModifyEntry::from_relocation_entry(&reloc, &relocation_context)
                    })
                    .collect::<Result<Vec<_>>>()
                    .unwrap();
                data_relocations[segment_id].extend(sym_relocs);
            }
        }
        Object {
            globals: self.globals.lock(),
            functions: self.functions.lock(),
            data: data_segment_outputs,
            data_relocations,
        }
    }
}

pub struct BuilderContextToBeRemoved<'any, 'src> {
    pub module_info: &'any InputObject<'src>,
    pub sub_module_extra: &'any Option<SubModuleExtra>,
    pub static_symbols: &'any BTreeSet<SymbolId>,
}
impl BuilderContextToBeRemoved<'_, '_> {
    fn is_main(&self) -> bool {
        self.sub_module_extra.is_none()
    }
    fn lib_base_import(&self) -> Option<<DefinedGlobal as PrimaryKey>::EntityType> {
        self.sub_module_extra
            .as_ref()
            .map(|extra| extra.self_base.lib_base_id)
    }
    fn is_static_symbol(&self, symbol: SymbolId) -> bool {
        self.static_symbols.contains(&symbol)
    }
}

/// Phase 2 <ObjectBuilder -> Finalized>: Calculate types, lock functions/globals indexes, finalize data segments
/// Phase 3 <Finalized subroutine>: calculate types, emit (types, functions, tables, memories, globals, exports, start_fn, elem, code, data segments)
/// Phase 4 <Finalized subroutine>: calculate relocations and emit custom sections
pub struct Object<'src> {
    pub globals: WithOriginalIndex<'src, DefinedGlobal<'src>>,

    pub functions: WithOriginalIndex<'src, DefinedFunction>,

    pub data: IdMap2<DataSegmentId, memory_layout::DataSegmentOutput>,
    //TODO: Remove data_relocations, instead of DataSegmentOutput use SegmentLayout
    pub data_relocations: IdMap2<DataSegmentId, Vec<modify::DataModifyEntry>>,
    // custom_sections: Vec<CustomSection>,
}
