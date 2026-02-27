//!
//! Wasm module high-level API for simplification of structured reading.
//! The root is `InputObject` struct which gives access to wasm entities in structured way.
//!
//! 1. `ElementTable` provides a way to access wasm table with elements corresponding to this table.
//! 2.
//!

use std::{borrow::Cow, fmt::Debug};

use anyhow::{Result, bail};
use cranelift_entity::{EntityRef, PrimaryMap};
pub use entities::*;
use log::warn;
use wasmparser::{ElementItems, FuncType, TableType, TypeRef};
use yoke::{Yoke, Yokeable};

use crate::{
    index::{Building, CompoundList, Finished, ImportOrDefined, Temp},
    linkage::{
        LinkageInfo,
        file_db::{self, FileRelocs},
    },
    raw::{self, DefinedFuncId, ImportId},
    typed::{data::DataSymbolRef, entities::common_index::EntityKind},
};

pub mod data;
pub mod elements;
mod entities;
impl_entity_index! {
    #[display = "file"]
    pub struct FileId;

    #[display = "sym"]
    pub struct SymbolId;
}

type FileWithData<'src> = Yoke<LinkingFile<'src>, Box<[u8]>>;

///
/// Manages files to create zero-copy wasm parsed module.
///
pub struct FileLoader {
    files_readers: PrimaryMap<FileId, FileWithData<'static>>,
}
impl FileLoader {
    pub fn load_file(&mut self, path: impl AsRef<std::path::Path>) -> Result<FileId> {
        let data = std::fs::read(path)?.into_boxed_slice();
        let file =
            FileWithData::try_attach_to_cart(data, |data| LinkingFile::from_wasm_bytes(data))?;
        let id = self.files_readers.push(file);
        Ok(id)
    }

    pub fn get_file(&self, file_id: FileId) -> &LinkingFile<'_> {
        self.files_readers.get(file_id).unwrap().get()
    }
}

//
// Wasm module + extra information required for applying relocations of symbols from this module.
//
#[derive(Yokeable)]
pub struct LinkingFile<'src> {
    // used for tests
    #[allow(dead_code, reason = "tests")]
    pub(crate) wasm_reader: raw::ObjectReader<'src>,
    pub file_symbol_db: file_db::FileSymbolDb,
    pub relocs: FileRelocs,
    pub module: Module<'src>,
}

impl<'src> LinkingFile<'src> {
    pub fn from_wasm_bytes(wasm_bytes: &'src [u8]) -> Result<Self> {
        let reader = raw::ObjectReader::parse(wasm_bytes)?;

        Self::from_raw_module(reader)
    }
    pub fn from_raw_module(reader: raw::ObjectReader<'src>) -> Result<Self> {
        let (module, file_symbol_db) = Module::from_raw_module(&reader)?;
        let file_relocs = LinkageInfo::collect_ordered_relocs(&reader);
        let owners = LinkageInfo::build_regions(&module);
        let relocs = FileRelocs::build_relocs(file_relocs, &file_symbol_db, owners)?;

        Ok(Self {
            wasm_reader: reader,
            file_symbol_db,
            relocs,
            module,
        })
    }
}

pub type ModuleBuilder<'src> = Module<'src, Building>;

/// Partially parsed wasm object.
/// It expects that module has valid structure and contains additional custom sections:
/// - name section with function and global names
/// - linking section with symbol information
///
/// Unlike `raw::ObjectReader` which is low-level representation of wasm module sections structure,
/// `Module` provides higher-level API to access wasm entities like functions and globals, in a way that concatenates imported and defined entities.
/// So user can use type-safe indexes from original module.
/// Additionally, `Module` expects that module has linking information and symbol names for entities.
/// Also `Module` can be built from scratch using `ModuleBuilder` API, during this build Temp indexes are returned, which
/// can be converted to stable after calling `into_finished()`.
/// The temp indexes are used to automatically shift defined entities after new imports are added.
///
#[derive(Debug)]
pub struct Module<'src, BuilderState = Finished> {
    // wasm entities
    pub functions: entities::Functions<'src, BuilderState>,
    pub tables: entities::Tables<'src, BuilderState>,
    pub memories: entities::Memories<'src, BuilderState>,
    pub globals: entities::Globals<'src, BuilderState>,
    pub tags: entities::Tags<'src, BuilderState>,

    /// linkage entity
    // (lack of import part?)
    pub data: PrimaryMap<DataSymbolRef, data::RawDataChunk<'src>>,
    // extra information
    pub indirect_function_table: elements::IndirectFunctionTable,
    pub mem_spec: data::MemSpec<'src>,
    /// List of functions to be called on module start.
    /// Should have `()->void` type and can be defined or imported.
    pub start_functions: Vec<FunctionRef>,
}

impl<'src> Module<'src> {
    pub fn from_raw_module(
        reader: &raw::ObjectReader<'src>,
    ) -> Result<(Self, file_db::FileSymbolDb)> {
        //TODO: Maybe we should use `IdMap` here?
        let mut imported_funcs: Vec<ImportId> = Vec::new();
        let mut imported_globals: Vec<ImportId> = Vec::new();

        let imports = entities::read_imports(reader)?;
        let exports = entities::read_exports(reader)?;

        let functions = entities::Functions::from_parts(
            CompoundList::new(
                imports.0,
                reader
                    .code
                    .section_payload
                    .defined_funcs
                    .values()
                    .map(Into::into)
                    .collect(),
            )
            .into_finished(),
            reader
                .names
                .functions
                .iter()
                // TODO: remove
                .map(|(id, name)| (FunctionRef::from_u32(id.as_u32()), *name))
                .collect(),
            exports.0,
        );
        let tables = entities::Tables::from_parts(
            CompoundList::new(imports.1, reader.tables.values().map(Into::into).collect())
                .into_finished(),
            reader
                .names
                .tables
                .iter()
                // TODO: remove
                .map(|(id, name)| (TableRef::from_u32(id.as_u32()), *name))
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
                .map(|(id, name)| (MemoryRef::from_u32(id.as_u32()), *name))
                .collect(),
            exports.2,
        );
        let globals = entities::Globals::from_parts(
            CompoundList::new(imports.3, reader.globals.values().map(Into::into).collect())
                .into_finished(),
            reader
                .names
                .globals
                .iter()
                // TODO: remove
                .map(|(id, name)| (GlobalRef::from_u32(id.as_u32()), *name))
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
                .map(|(id, name)| (TagRef::from_u32(id.as_u32()), *name))
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
            elements::IndirectFunctionTable::from_reader(reader, table_id, true)?;

        let LinkageInfo {
            mut file_symbol_db,
            defined_data_symbols,
        } = LinkageInfo::from_reader(reader);

        let mut chunks = defined_data_symbols
            .chunk_by(|o, a| o.1.segment_id == a.1.segment_id)
            .peekable();

        // todo!("check that after filtering symbols in file_symbol_db we also have shifts");

        let data = {
            // todo: make it configurable
            let slice_chunks = true;

            let mut sliced_chunks = PrimaryMap::new();

            for (segment_id, d) in reader.data.data_segments.iter() {
                let segment_info = reader.linking.segments_info[segment_id.index()];
                let pow2align = segment_info.alignment.try_into().unwrap();
                let segment_name = segment_info.name;
                // Range.start is point to <length> field of data segment.
                let data_start = d.range.end - d.data.len();
                let segment_chunk = data::RawDataChunk::from_segment(
                    segment_id,
                    d.data,
                    segment_name.into(),
                    pow2align,
                    data_start,
                );

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
                    .iter()
                    .map(|&(symbol_id, ref symbol_info)| (symbol_id, symbol_info))
                    .collect::<Vec<_>>();

                let sliced = segment_chunk.slice_segment(defined_data_symbols);
                // LLVM provides data symbols in random order, sometimes one symbol can be a part of another symbol.
                let filtered =
                    data::DataChunk::canonicalize_data_symbols(sliced, &mut file_symbol_db);
                sliced_chunks.extend(filtered.into_iter().map(|(_, v)| v));
            }

            sliced_chunks
        };

        let mem_spec = data::MemSpec::from_reader(reader)?;

        let this = Module {
            indirect_function_table,
            data,
            functions,
            tables,
            memories,
            globals,
            tags,
            mem_spec,
            start_functions: reader.code.start_func.into_iter().collect(),
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
    // Get entity name
    pub fn get_name(&self, entity: EntityKind) -> Cow<'_, str> {
        let debug_name = match entity {
            EntityKind::Function(func_id) => self.functions.names.get(func_id).map(|n| &n[..]),
            EntityKind::Global(global_id) => self.globals.names.get(global_id).map(|n| &n[..]),
            EntityKind::Table(table_id) => self.tables.names.get(table_id).map(|n| &n[..]),
            EntityKind::Memory(mem_id) => self.memories.names.get(mem_id).map(|n| &n[..]),
            EntityKind::Tag(tag_id) => self.tags.names.get(tag_id).map(|n| &n[..]),
            EntityKind::DataSymbol(d) => self.data.get(d).map(|v| &v.name[..]),
            EntityKind::Type(_) => None, // types don't have names in name section
        };
        debug_name
            .map(Cow::Borrowed)
            .unwrap_or_else(|| format!("{entity}").into())
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

    pub fn get_function_type(&self, func_id: FunctionRef) -> &FuncType {
        let func = self.functions.items.get_entity(func_id);
        match func {
            ImportOrDefined::Defined(defined) => &defined.entity_type,
            ImportOrDefined::Import(import) => &import.entity_type,
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

impl<'src> ModuleBuilder<'src> {
    #[allow(
        clippy::new_without_default,
        reason = "it's api only for building state, so make it default might be confusing"
    )]
    pub fn new() -> Self {
        let mut tables = entities::Tables::default();
        let _table_ref = tables.items.push_defined(&raw::Table {
            ty: TableType {
                table64: false,
                shared: false,
                initial: 0,
                maximum: None,
                element_type: wasmparser::RefType::FUNCREF,
            },
            // initialized using element segments later aka <indirect_function_table>
            init: wasmparser::TableInit::RefNull,
        });

        ModuleBuilder {
            functions: entities::Functions::default(),
            memories: entities::Memories::default(),
            globals: entities::Globals::default(),
            tags: entities::Tags::default(),
            data: PrimaryMap::default(),
            mem_spec: data::MemSpec::default(),
            tables,
            // TODO: When building IndirectFunctionTable provide Temp<TableRef> instead of TableRef.
            indirect_function_table: elements::IndirectFunctionTable::new(TableRef::from_u32(0)),
            start_functions: Vec::default(),
        }
    }

    /// Finalize building module, converting all entities to finished state and making them ready for use.
    pub fn into_finished(self) -> Module<'src, Finished> {
        Module {
            functions: self.functions.into_finished(),
            tables: self.tables.into_finished(),
            memories: self.memories.into_finished(),
            globals: self.globals.into_finished(),
            tags: self.tags.into_finished(),
            data: self.data,
            mem_spec: self.mem_spec,
            indirect_function_table: self.indirect_function_table,
            start_functions: self.start_functions,
        }
    }

    /// Add imported global to the module, returning its reference.
    pub fn add_imported_global(&mut self, import: ImportedGlobal<'src>) -> Temp<GlobalRef> {
        self.globals.items.push_import(import)
    }

    /// Add defined global to the module, returning its reference.
    pub fn add_defined_global(&mut self, global: DefinedGlobal<'src>) -> Temp<GlobalRef> {
        self.globals.items.push_defined(global)
    }

    /// Add imported function to the module, returning its reference.
    pub fn add_imported_function(&mut self, import: ImportedFunction<'src>) -> Temp<FunctionRef> {
        self.functions.items.push_import(import)
    }

    /// Add defined function to the module, returning its reference.
    pub fn add_defined_function(&mut self, func: DefinedFunction<'src>) -> Temp<FunctionRef> {
        self.functions.items.push_defined(func)
    }
}

#[cfg(test)]
mod tests {
    use wasmparser::FuncType;

    use super::{LinkingFile, Module};
    use crate::typed::ImportedFunction;

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
        module.add_imported_function(ImportedFunction {
            module: "env".into(),
            name: "bar".into(),
            entity_type: FuncType::new(None, None), // void type
        });
        let module = module.into_finished();

        assert_eq!(module.functions.len(), 1);

        todo!("Add data and defined function/global");
    }
}
