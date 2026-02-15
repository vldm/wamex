//!
//! Wasm high-level API for simplification of structured reading.
//! The root is `InputObject` struct which gives access to wasm entities in structured way.
//!
//! 1. `ElementTable` provides a way to access wasm table with elements corresponding to this table.
//! 2.
//!

use std::{cmp::Ordering, collections::BTreeMap, fmt::Debug, ops::Range, vec};

use anyhow::{Context, Result, bail};
use cranelift_entity::{EntityRef, PrimaryMap, SecondaryMap, packed_option::ReservedValue};
pub use entities::*;
use log::warn;
use wasmparser::{ElementItems, SymbolInfo, TypeRef};

use crate::{
    index::{Building, CompoundList, Finished, GappedMap, IdVec, ImportOrDefined, NonDefault},
    read::{
        self,
        common_index::{AnyEntityRef, EntitiesSnapshot, TaggedEntityRef},
        raw::{DefinedFuncId, ElementId, FuncTypeId, ImportId},
        typed::{
            data::DataSymbolRef,
            name_resolver::{LinkageInfo, Relocations},
        },
    },
    symbols::SymbolId,
};

pub mod common_index;
pub mod data;
pub mod elements;
mod entities;
mod name_resolver;

type Bytes = Vec<u8>;

impl_entity_index! {
    pub struct FileId;
}

//
// Wasm module + extra information required for applying relocations of symbols from this module.
//
struct LinkingFile<'src> {
    wasm_reader: read::ObjectReader<'src>,
    pub file_symbol_db: name_resolver::FileSymbolDb,
    pub relocs: Relocations,
    pub module: Module<'src>,
}

impl<'src> LinkingFile<'src> {
    pub fn from_wasm_bytes(wasm_bytes: &'src [u8]) -> Result<Self> {
        let reader = read::ObjectReader::parse(&wasm_bytes)?;
        Self::from_raw_module(reader)
    }
    pub fn from_raw_module(reader: read::ObjectReader<'src>) -> Result<Self> {
        let (module, file_symbol_db) = Module::from_raw_module(&reader)?;
        let file_relocs = LinkageInfo::collect_ordered_relocs(&reader);
        let owners = LinkageInfo::build_owners(&module, EntitiesSnapshot::new(&module));
        let relocs = Relocations::build_relocs(file_relocs, &file_symbol_db, owners)?;

        Ok(Self {
            wasm_reader: reader,
            file_symbol_db,
            relocs,
            module,
        })
    }
}

/// Partially parsed wasm object.
/// It expects that module has valid structure and contains additional custom sections:
/// - name section with function and global names
/// - linking section with symbol information
///
/// Unlike `read::ObjectReader` which is low-level representation of wasm module sections structure,
/// `InputObject` provides higher-level API to access wasm entities like functions and globals, in a way that concatenates imported and defined entities.
/// So user can use type-safe indexes from original module.
/// Additionally, `InputObject` expects that module has linking information and symbol names for entities.
#[derive(Debug)]
pub struct Module<'src, BuilderState = Finished> {
    // symbols: Symbols<'src>,
    // pub to_any_ref: GappedMap<SymbolId, common_index::AnyEntityRef>,

    // wasm entities
    pub functions: entities::Functions<'src, BuilderState>,
    pub tables: entities::Tables<'src, BuilderState>,
    pub memories: entities::Memories<'src, BuilderState>,
    pub globals: entities::Globals<'src, BuilderState>,
    pub tags: entities::Tags<'src, BuilderState>,

    // linkage entity (lack of import part?)
    pub data: IdVec<data::RawDataChunk<'src>>,

    // extra information
    pub indirect_function_table: elements::IndirectFunctionTable,
}

impl<'src> Module<'src> {
    pub fn from_raw_module(
        reader: &read::ObjectReader<'src>,
    ) -> Result<(Self, name_resolver::FileSymbolDb)> {
        //TODO: Maybe we should use `IdMap` here?
        let mut imported_funcs: Vec<ImportId> = Vec::new();
        let mut imported_globals: Vec<ImportId> = Vec::new();

        let imports = entities::read_imports(&reader)?;
        let exports = entities::read_exports(&reader)?;

        let functions = entities::Functions::from_parts(
            CompoundList::new(
                imports.0,
                reader
                    .code
                    .section_payload
                    .defined_funcs
                    .as_values_slice()
                    .to_vec(),
            )
            .into_finished(),
            reader
                .names
                .functions
                .iter()
                // TODO: remove
                .map(|(id, name)| (FunctionRef::from_u32(id.as_u32()), NonDefault::from(*name)))
                .collect(),
            exports.0,
        );
        let tables = entities::Tables::from_parts(
            CompoundList::new(imports.1, reader.tables.as_values_slice().to_vec()).into_finished(),
            reader
                .names
                .tables
                .iter()
                // TODO: remove
                .map(|(id, name)| (TableRef::from_u32(id.as_u32()), NonDefault::from(*name)))
                .collect(),
            exports.1,
        );
        let memories = entities::Memories::from_parts(
            CompoundList::new(imports.2, reader.memories.as_values_slice().to_vec())
                .into_finished(),
            reader
                .names
                .memories
                .iter()
                // TODO: remove
                .map(|(id, name)| (MemoryRef::from_u32(id.as_u32()), NonDefault::from(*name)))
                .collect(),
            exports.2,
        );
        let globals = entities::Globals::from_parts(
            CompoundList::new(imports.3, reader.globals.as_values_slice().to_vec()).into_finished(),
            reader
                .names
                .globals
                .iter()
                // TODO: remove
                .map(|(id, name)| (GlobalRef::from_u32(id.as_u32()), NonDefault::from(*name)))
                .collect(),
            exports.3,
        );

        let tags = entities::Tags::from_parts(
            CompoundList::new(imports.4, reader.tags.as_values_slice().to_vec()).into_finished(),
            reader
                .names
                .tags
                .iter()
                // TODO: remove
                .map(|(id, name)| (TagRef::from_u32(id.as_u32()), NonDefault::from(*name)))
                .collect(),
            exports.4,
        );

        for (import_id, import) in reader.imports.iter() {
            match import.ty {
                TypeRef::Global(_) => {
                    imported_globals.push(import_id);
                    continue;
                }
                TypeRef::Func(_) => {
                    imported_funcs.push(import_id);
                }
                _ => {}
            }
        }

        let (_table_name, table_id) = tables
            .iter()
            .filter_map(|(id, _)| reader.names.tables.get(id).map(|name| (name.into_inner(), id)))
            .find(|(name, _)| *name == "__indirect_function_table")
            .unwrap_or_else(|| {
                assert!(
                    tables.items.defined.len() == 1,
                    "No named __indirect_function_table was found, and there is not one table in the module."
                );
                (
                    "__indirect_function_table",
                    tables.defined_iter().next().unwrap().0,
                )
            });

        let indirect_function_table =
            elements::IndirectFunctionTable::from_reader(&reader, table_id, true)?;

        let LinkageInfo {
            mut file_symbol_db,
            defined_data_symbols,
        } = LinkageInfo::from_reader(&reader);

        let mut chunks = defined_data_symbols
            .chunk_by(|o, a| o.1.segment_id == a.1.segment_id)
            .peekable();

        let data = {
            // todo: make it configurable
            let slice_chunks = true;

            let mut sliced_chunks = IdVec::new();

            for (segment_id, d) in reader.data.data_segments.iter() {
                let pow2align = reader.linking.segments_info[segment_id.index()]
                    .alignment
                    .try_into()
                    .unwrap();
                let segment_chunk =
                    data::RawDataChunk::from_segment(d.data, pow2align, d.range.start);

                if !slice_chunks {
                    warn!("Skipping data segment slicing - working with one chunk per segment");
                    sliced_chunks.push(segment_chunk);
                    continue;
                }

                let Some(chunk) = chunks.peek() else {
                    warn!("No more data symbols, skipping slicing for the rest of segments");
                    sliced_chunks.push(segment_chunk);
                    continue;
                };

                let chunk_segment_id = chunk[0].1.segment_id;
                if chunk_segment_id < segment_id {
                    // data symbols from previous segment (bug)
                    panic!(
                        "Segment id mismatch: expected {:?}, found {:?}. Skipping slicing for this segment.",
                        segment_id, chunk[0].1.segment_id
                    );
                } else if chunk_segment_id > segment_id {
                    // no data symbols for this segment, just skip slicing
                    warn!(
                        "No data symbols for segment {:?}. Skipping slicing for this segment.",
                        segment_id
                    );
                    sliced_chunks.push(segment_chunk);
                    continue;
                }

                let defined_data_symbols = chunks
                    .next()
                    .unwrap()
                    .into_iter()
                    .map(|&(symbol_id, ref symbol_info)| (symbol_id, symbol_info))
                    .collect::<Vec<_>>();

                let sliced = segment_chunk.slice_segment(defined_data_symbols);
                let filtered = data::DataChunk::filter_bounds_in_table(sliced, &mut file_symbol_db);

                sliced_chunks.extend(filtered.into_iter().map(|(_, chunk)| chunk));
            }

            sliced_chunks
        };

        let this = Module {
            indirect_function_table,
            data,
            functions,
            tables,
            memories,
            globals,
            tags,
        };

        Ok((this, file_symbol_db))
    }

    pub(crate) fn read_const_expr(offset_expr: &wasmparser::ConstExpr<'_>) -> Result<i32> {
        let mut reader = offset_expr.get_operators_reader();

        let val = match reader.read()? {
            wasmparser::Operator::I32Const { value } => Ok(value),
            op => bail!("Expected only I32.const operator, found: {:?}", op),
        };
        match reader.read()? {
            wasmparser::Operator::End => {}
            op => bail!("Expected End after I32.const: {:?}", op),
        }
        val
    }
    pub fn function_id_iter<'any>(
        &'any self,
    ) -> impl Iterator<Item = FunctionRef> + use<'any, 'src> {
        self.functions.iter_all_ids()
    }

    pub fn is_imported_function(&self, func_id: FunctionRef) -> bool {
        func_id.index() < self.functions.items.imports.len()
    }

    pub fn as_defined_function_id(&self, func_id: FunctionRef) -> Option<DefinedFuncId> {
        if self.is_imported_function(func_id) {
            None
        } else {
            Some(DefinedFuncId::from_u32(
                func_id.index() as u32 - self.functions.items.imports.len() as u32,
            ))
        }
    }

    pub fn get_function_type_id(&self, func_id: FunctionRef) -> FuncTypeId {
        let func = self.functions.items.get_entity(func_id);
        match func {
            ImportOrDefined::Defined(defined) => defined.type_id,
            ImportOrDefined::Import(import) => import.entity_type,
        }
    }

    pub fn find_function_id_by_name(&self, name: &str) -> Option<FunctionRef> {
        let func = self.functions.names.iter().find(|f| **f.1 == name)?;
        Some(func.0)
    }

    pub fn find_global_id_by_name(&self, name: &str) -> Option<GlobalRef> {
        let global = self.globals.names.iter().find(|f| **f.1 == name)?;
        Some(global.0)
    }
}

impl<'src> Module<'src, Building> {
    pub fn new() -> Self {
        Module {
            functions: entities::Functions::new(),
            tables: entities::Tables::new(),
            memories: entities::Memories::new(),
            globals: entities::Globals::new(),
            tags: entities::Tags::new(),
            data: IdVec::new(),
            indirect_function_table: elements::IndirectFunctionTable::new(TableRef::from_u32(0)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LinkingFile, Module};

    // 1. open example.wasm with `InputObject::from_wasm_bytes`
    #[test]
    fn test_example_wasm() {
        let file =
            std::env::var("CARGO_MANIFEST_DIR").unwrap() + "/../wamex-cli/test-data/example.wasm";

        println!("Reading wasm file: {}", file);
        let wasm_bytes = std::fs::read(file).unwrap();
        let file = LinkingFile::from_wasm_bytes(&wasm_bytes).unwrap();
        let input_object = file.module;
        assert_eq!(input_object.data.len(), 127);
        assert_eq!(input_object.functions.len(), 706);
    }

    // 2. Create simple wasm module from scratch
    #[test]
    fn create_from_scratch() {
        let mut module = Module::new();
        todo!();
        // push_function();
        // finalize();
    }
}
