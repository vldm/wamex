//!
//! Wasm module high-level API for simplification of structured reading.
//! The root is `InputObject` struct which gives access to wasm entities in structured way.
//!
//! 1. `ElementTable` provides a way to access wasm table with elements corresponding to this table.
//! 2.
//!

use std::{borrow::Cow, fmt::Debug};

use anyhow::{Result, bail};
use cranelift_entity::{EntityRef, PrimaryMap, SecondaryMap, packed_option::ReservedValue};
pub use entities::*;
use itertools::chain;
use log::warn;
use wasmparser::{ElementItems, TableType, TypeRef};
use yoke::{Yoke, Yokeable};

use crate::{
    linkage::{
        LinkageInfo,
        file_db::{self, FileRelocs},
        reloc::{EntityAddressMode, EntityRelocationEntry},
    },
    raw::{self, ImportId},
    typed::entities::common_index::EntityKind,
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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Locked {}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]

pub enum Building {}

type FileWithData<'src> = Yoke<LoadedFile<'src>, Box<[u8]>>;

///
/// Manages files to create zero-copy wasm parsed module.
///
#[derive(Default)]
pub struct FileLoader {
    files_readers: PrimaryMap<FileId, FileWithData<'static>>,
}
impl FileLoader {
    pub fn new() -> Self {
        Self {
            files_readers: PrimaryMap::new(),
        }
    }
    pub fn load_file(&mut self, path: impl AsRef<std::path::Path>) -> Result<FileId> {
        let data = std::fs::read(path)?.into_boxed_slice();
        let file =
            FileWithData::try_attach_to_cart(data, |data| LoadedFile::from_wasm_bytes(data))?;
        let id = self.files_readers.push(file);
        Ok(id)
    }

    pub(crate) fn load_from_bytes(&mut self, data: Box<[u8]>) -> Result<FileId> {
        let file =
            FileWithData::try_attach_to_cart(data, |data| LoadedFile::from_wasm_bytes(data))?;
        let id = self.files_readers.push(file);
        Ok(id)
    }

    pub(crate) fn first_file_id(&self) -> Option<FileId> {
        self.files_readers.iter().next().map(|(id, _)| id)
    }

    pub fn get_file(&self, file_id: FileId) -> &LoadedFile<'_> {
        self.files_readers.get(file_id).unwrap().get()
    }
}

//
// Wasm module + extra information required for applying relocations of symbols from this module.
//
#[derive(Yokeable)]
pub struct LoadedFile<'src> {
    // used for tests
    #[allow(dead_code, reason = "tests")]
    pub(crate) wasm_reader: raw::ObjectReader<'src>,
    pub file_symbol_db: file_db::FileSymbolDb,
    pub relocs: FileRelocs,
    pub module: Module<'src>,
}

impl<'src> LoadedFile<'src> {
    pub fn from_wasm_bytes(wasm_bytes: &'src [u8]) -> Result<Self> {
        let reader = raw::ObjectReader::parse(wasm_bytes)?;

        Self::from_raw_module(reader)
    }
    pub fn from_raw_module(reader: raw::ObjectReader<'src>) -> Result<Self> {
        let (module, file_symbol_db) = Module::from_raw_module(&reader)?;
        let file_relocs = LinkageInfo::collect_ordered_relocs(&reader);
        let owners = LinkageInfo::build_regions(&module);
        let relocs = FileRelocs::build_relocs_static(file_relocs, &file_symbol_db, owners)?;

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
pub struct Module<'src, BuilderState = Locked> {
    // wasm entities
    pub functions: entities::Functions<'src, BuilderState>,
    pub tables: entities::Tables<'src, BuilderState>,
    pub memories: entities::Memories<'src, BuilderState>,
    pub globals: entities::Globals<'src, BuilderState>,
    pub tags: entities::Tags<'src, BuilderState>,

    /// linkage entity
    pub data: entities::DataChunks<'src, BuilderState>,
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

        let functions = entities::Functions::new_raw(
            imports.0,
            reader
                .code
                .section_payload
                .defined_funcs
                .values()
                .map(Into::into)
                .collect(),
        )
        .into_finished()
        .extend_with_info(
            reader
                .names
                .functions
                .iter()
                // TODO: remove
                .map(|(id, name)| (id, (*name).into()))
                .collect(),
            exports.0,
        );
        let tables =
            entities::Tables::new_raw(imports.1, reader.tables.values().map(Into::into).collect())
                .into_finished()
                .extend_with_info(
                    reader
                        .names
                        .tables
                        .iter()
                        // TODO: remove
                        .map(|(id, name)| (id, (*name).into()))
                        .collect(),
                    exports.1,
                );
        let memories = entities::Memories::new_raw(
            imports.2,
            reader.memories.values().map(Into::into).collect(),
        )
        .into_finished()
        .extend_with_info(
            reader
                .names
                .memories
                .iter()
                // TODO: remove
                .map(|(id, name)| (id, (*name).into()))
                .collect(),
            exports.2,
        );
        let globals = entities::Globals::new_raw(
            imports.3,
            reader.globals.values().map(Into::into).collect(),
        )
        .into_finished()
        .extend_with_info(
            reader
                .names
                .globals
                .iter()
                // TODO: remove
                .map(|(id, name)| (id, (*name).into()))
                .collect(),
            exports.3,
        );

        let tags =
            entities::Tags::new_raw(imports.4, reader.tags.values().map(Into::into).collect())
                .into_finished()
                .extend_with_info(
                    reader
                        .names
                        .tags
                        .iter()
                        // TODO: remove
                        .map(|(id, name)| (id, (*name).into()))
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

        let (_table_name, table_id) = Self::init_indirect_fn_table(&tables);
        let (_memory_name, memory_id) = Self::init_base_memory(&memories);

        let indirect_function_table =
            elements::IndirectFunctionTable::from_reader(reader, table_id, true)?;

        // TODO: add undefined data symbols as well.
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

            let mut sliced_chunks = entities::DataChunks::default();

            'iter: for (segment_id, d) in reader.data.data_segments.iter() {
                let segment_chunk = 'chunk_segment: {
                    let Some(segment_info) = reader.linking.segments_info.get(segment_id.index())
                    else {
                        warn!(
                            "No segment info for segment {:?}, skipping slicing for this segment.",
                            segment_id
                        );
                        let (name, pow2align) = data::default_segment_info();

                        break 'chunk_segment data::RawDataChunk::from_segment(
                            segment_id,
                            d.data,
                            name,
                            pow2align,
                            d.range.end - d.data.len(),
                        );
                    };
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
                        break 'chunk_segment segment_chunk;
                    }

                    let Some(chunk) = chunks.peek() else {
                        warn!("No more data symbols, skipping slicing for the rest of segments");
                        break 'chunk_segment segment_chunk;
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
                        break 'chunk_segment segment_chunk;
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
                    for (_, chunk) in filtered {
                        sliced_chunks.push_defined(DefinedDataChunk::from(chunk));
                    }

                    continue 'iter;
                };

                sliced_chunks.push_defined(DefinedDataChunk::from(segment_chunk));
            }

            sliced_chunks.into_finished()
        };

        let mem_spec = data::MemSpec::from_reader(reader, memory_id)?;

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

    fn init_indirect_fn_table(tables: &entities::Tables<'src>) -> (Cow<'src, str>, TableRef) {
        tables
            .iter()
            .filter_map(|(id, def)| def.name().cloned().map(|name| (name, id)))
            .find(|(name, _)| *name == "__indirect_function_table")
            .unwrap_or_else(|| {
                assert!(
                    tables.len() == 1,
                    "No named __indirect_function_table was found, and there is not one table in the module."
                );
                (
                    "__indirect_function_table".into(),
                    tables.iter().next().unwrap().0,
                )
            })
    }

    fn init_base_memory(memories: &entities::Memories<'src>) -> (Cow<'src, str>, MemoryRef) {
        memories
            .iter()
            .filter_map(|(id, def)| def.name().cloned().map(|name| (name, id)))
            .find(|(name, _)| *name == "__base_memory")
            .unwrap_or_else(|| {
                assert!(
                    memories.len() == 1,
                    "No named __base_memory was found, and there is not one memory in the module."
                );
                ("__base_memory".into(), memories.iter().next().unwrap().0)
            })
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
    /// Get entity name
    pub fn get_name(&self, entity: EntityKind) -> Cow<'src, str> {
        let debug_name = match entity {
            EntityKind::Function(func_id) => self.functions.get_entity(func_id).name().cloned(),
            EntityKind::Global(global_id) => self.globals.get_entity(global_id).name().cloned(),
            EntityKind::Table(table_id) => self.tables.get_entity(table_id).name().cloned(),
            EntityKind::Memory(mem_id) => self.memories.get_entity(mem_id).name().cloned(),
            EntityKind::Tag(tag_id) => self.tags.get_entity(tag_id).name().cloned(),
            EntityKind::DataSymbol(d) => self.data.get_entity(d).name().cloned(),
            EntityKind::Type(_) => None, // types don't have names in name section
        };

        debug_name.unwrap_or_else(|| format!("{entity}").into())
    }
    /// Calculate estimated size of entity.
    pub fn get_body_len(&self, entity: EntityKind) -> usize {
        match entity {
            EntityKind::DataSymbol(d) => self
                .data
                .get_entity(d)
                .to_defined()
                .map(|defined| defined.body.len()),
            EntityKind::Function(func_id) => self
                .functions
                .get_entity(func_id)
                .to_defined()
                .map(|defined| defined.body.len()),
            _ => None,
        }
        .unwrap_or_default()
    }
    /// Get entity export names
    pub fn entities_exports(&self) -> impl Iterator<Item = EntityKind> + '_ {
        let functions = self
            .functions
            .iter()
            .filter(|(_, e)| !e.export_as().names.is_empty())
            .map(|(r, _)| r.into());
        let globals = self
            .globals
            .iter()
            .filter(|(_, e)| !e.export_as().names.is_empty())
            .map(|(r, _)| r.into());
        let tables = self
            .tables
            .iter()
            .filter(|(_, e)| !e.export_as().names.is_empty())
            .map(|(r, _)| r.into());
        let memories = self
            .memories
            .iter()
            .filter(|(_, e)| !e.export_as().names.is_empty())
            .map(|(r, _)| r.into());
        let tags = self
            .tags
            .iter()
            .filter(|(_, e)| !e.export_as().names.is_empty())
            .map(|(r, _)| r.into());
        let data = self
            .data
            .iter()
            .filter(|(_, e)| !e.export_as().names.is_empty())
            .map(|(r, _)| r.into());
        chain!(functions, globals, tables, memories, tags, data)
    }

    pub fn entities_bodies(&self) -> impl Iterator<Item = (EntityKind, &EntityBody<'src>)> + '_ {
        fn map_body<'any, 'src, T>(
            (v, def): (impl Into<EntityKind>, &'any DefinedEntity<'src, T>),
        ) -> (EntityKind, &'any EntityBody<'src>) {
            (v.into(), &def.body)
        }
        let functions = self.functions.defined_iter().map(map_body);
        let globals = self.globals.defined_iter().map(map_body);
        let tables = self.tables.defined_iter().map(map_body);
        let data = self.data.defined_iter().map(map_body);

        chain!(functions, globals, tables, data)
    }

    pub fn entities_bodies_mut(
        &mut self,
    ) -> impl Iterator<Item = (EntityKind, &mut EntityBody<'src>)> + '_ {
        fn map_body_mut<'any, 'src, T>(
            (v, def): (impl Into<EntityKind>, &'any mut DefinedEntity<'src, T>),
        ) -> (EntityKind, &'any mut EntityBody<'src>) {
            (v.into(), &mut def.body)
        }
        let functions = self.functions.defined_iter_mut().map(map_body_mut);
        let globals = self.globals.defined_iter_mut().map(map_body_mut);
        let tables = self.tables.defined_iter_mut().map(map_body_mut);
        let data = self.data.defined_iter_mut().map(map_body_mut);

        chain!(functions, globals, tables, data)
    }

    pub fn find_function_id_by_name(&self, name: &str) -> Option<FunctionRef> {
        let func = self
            .functions
            .iter()
            .find(|(_, e)| e.name().is_some_and(|n| n == name))?;
        Some(func.0)
    }

    pub fn find_global_id_by_name(&self, name: &str) -> Option<GlobalRef> {
        let global = self
            .globals
            .iter()
            .find(|(_, e)| e.name().is_some_and(|n| n == name))?;
        Some(global.0)
    }

    pub fn extend_indirect_table_from_relocs(&mut self, file_relocs: &FileRelocs) {
        let mut result: SecondaryMap<FunctionRef, bool> = SecondaryMap::new();

        let mut visit_reloc = |reloc: &EntityRelocationEntry| {
            if let EntityKind::Function(func_ref) = reloc.symbol_id
                && reloc.symbol_op == EntityAddressMode::RuntimeAddr
            {
                result[func_ref] = true;
            }
        };
        for (_, reloc) in file_relocs.iter_relocs() {
            for reloc in reloc.iter() {
                visit_reloc(reloc);
            }
        }

        let result = result.iter().filter_map(
            |(func_ref, is_indirect)| {
                if *is_indirect { Some(func_ref) } else { None }
            },
        );

        self.indirect_function_table.extend(result);
    }
}

impl<'src> ModuleBuilder<'src> {
    /// Creates a basic module, suitable for pushing entities.
    /// Note: this module doesn't have default memory and indirect function table,
    ///  so they should be created using [`create_base_memory`] and [`create_empty_indirect_fn_table`]
    ///  methods before pushing entities that reference them.
    ///
    /// To finalize indexes, call [`into_finished`] method.
    #[allow(
        clippy::new_without_default,
        reason = "it's api only for building state, so make it default might be confusing"
    )]
    pub fn new() -> Self {
        ModuleBuilder {
            functions: entities::Functions::default(),
            memories: entities::Memories::default(),
            globals: entities::Globals::default(),
            tags: entities::Tags::default(),
            data: entities::DataChunks::default(),
            mem_spec: data::MemSpec::default(),
            tables: entities::Tables::default(),
            indirect_function_table: elements::IndirectFunctionTable::new(
                TableRef::reserved_value(),
            ),
            start_functions: Vec::default(),
        }
    }

    /// Defines the default indirect function table for the module.
    ///
    /// Panics: if table_id for indirect function table was already set.
    pub fn create_empty_indirect_fn_table(&mut self) -> crate::index::Temp<TableRef> {
        if !self.indirect_function_table.table_id.is_reserved_value() {
            panic!(
                "Indirect function table is already defined with id {:?}.",
                self.indirect_function_table.table_id
            );
        }
        self.tables.push_defined(&raw::Table {
            ty: TableType {
                table64: false,
                shared: false,
                initial: 0,
                maximum: None,
                element_type: wasmparser::RefType::FUNCREF,
            },
            // initialized using element segments later aka <indirect_function_table>
            init: wasmparser::TableInit::RefNull,
        })
    }
    /// Defines the default memory for the module.
    ///
    /// Panics: if memory_id for base memory was already set.
    pub fn create_base_memory(&mut self) -> crate::index::Temp<MemoryRef> {
        if self.mem_spec.mem_id.is_reserved_value() {
            panic!(
                "Base memory is already defined with id {:?}.",
                self.mem_spec.mem_id
            );
        }
        self.memories.push_defined(WithoutBody {
            entity_type: raw::MemoryType {
                memory64: false,
                shared: false,
                initial: 1,
                maximum: None,
                page_size_log2: None,
            },
            export_as: ExportNames::default(),
            name: None,
        })
    }

    /// Finalize building module, converting all entities to finished state and making them ready for use.
    /// This allows converting temp indexes that was provided during buiding phase to stable indexes, that will be present in final module.
    ///
    /// This method will init default memory and indirect function table if they were not initialized during building phase,
    /// so it's requiered to either call [`create_empty_indirect_fn_table`] and [`create_base_memory`]
    /// or push tables/memories before calling this method.
    ///
    /// Panics: if no default memory/indirect function can be found.
    ///
    pub fn into_locked(self) -> Module<'src, Locked> {
        let tables = self.tables.into_finished();
        let mut indirect_function_table = self.indirect_function_table;
        if indirect_function_table.table_id.is_reserved_value() {
            let (_table_name, table_id) = Module::<'src, Locked>::init_indirect_fn_table(&tables);
            indirect_function_table.table_id = table_id;
        }
        let memories = self.memories.into_finished();
        let mut mem_spec = self.mem_spec;
        if mem_spec.mem_id.is_reserved_value() {
            let (_memory_name, memory_id) = Module::<'src, Locked>::init_base_memory(&memories);
            mem_spec.mem_id = memory_id;
        }

        Module {
            tables,
            memories,
            mem_spec,
            indirect_function_table,
            start_functions: self.start_functions,
            functions: self.functions.into_finished(),
            globals: self.globals.into_finished(),
            tags: self.tags.into_finished(),
            data: self.data.into_finished(),
        }
    }
}

#[cfg(test)]
mod tests {
    use wasmparser::FuncType;

    use super::{LoadedFile, Module};
    use crate::{
        index::GappedMap,
        typed::{ExportNames, ImportedFunction},
    };

    // 1. open example.wasm with `InputObject::from_wasm_bytes`
    #[test]
    fn test_example_wasm() {
        let file =
            std::env::var("CARGO_MANIFEST_DIR").unwrap() + "/../wamex-cli/test-data/example.wasm";

        println!("Reading wasm file: {}", file);
        let wasm_bytes = std::fs::read(file).unwrap();
        let file = LoadedFile::from_wasm_bytes(&wasm_bytes).unwrap();
        let mut input_object = file.module;
        assert_eq!(input_object.data.len(), 127);
        assert_eq!(input_object.functions.len(), 706);

        let mut indirect_fns = input_object
            .indirect_function_table
            .items
            .iter()
            .map(|(_id, func_ref)| *func_ref)
            .collect::<Vec<_>>();
        indirect_fns.sort();
        let len = indirect_fns.len();
        indirect_fns.dedup();
        assert_eq!(len, indirect_fns.len(),);

        input_object.indirect_function_table.items = GappedMap::new();

        input_object.extend_indirect_table_from_relocs(&file.relocs);
        let mut recovered_fns = input_object
            .indirect_function_table
            .items
            .iter()
            .map(|(_id, func_ref)| *func_ref)
            .collect::<Vec<_>>();
        assert_eq!(len, recovered_fns.len());
        // order might differ, but the content should be the same
        recovered_fns.sort();

        assert_eq!(indirect_fns, recovered_fns);

        // dbg!(&input_object.functions);
        // check exports imports of module
        let exports = input_object.functions.exports_iter().collect::<Vec<_>>();
        assert_eq!(exports.len(), 740);
    }

    // 2. Create simple wasm module from scratch
    #[test]
    fn create_from_scratch() {
        let mut module = Module::new();
        module.functions.push_import(ImportedFunction {
            module: "env".into(),
            name: "bar".into(),
            renamed_as: None,
            export_as: ExportNames::default(),
            entity_type: FuncType::new(None, None), // void type
        });
        let module = module.into_locked();

        assert_eq!(module.functions.len(), 1);

        todo!("Add data and defined function/global");
    }
}
