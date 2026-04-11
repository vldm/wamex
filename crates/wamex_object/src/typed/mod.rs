//!
//! Wasm module high-level API for simplification of structured reading.
//! The root is `InputObject` struct which gives access to wasm entities in structured way.
//!
//! 1. `ElementTable` provides a way to access wasm table with elements corresponding to this table.
//! 2.
//!

use std::{borrow::Cow, fmt::Debug};

use anyhow::Result;
use cranelift_entity::PrimaryMap;
pub use entities::*;
use itertools::chain;
use log::warn;
use smallvec::smallvec;
use wasmparser::{FuncType, TableType, TypeRef};
use yoke::{Yoke, Yokeable};

use crate::{
    emit::plan::GotInfo,
    index::Temp,
    layouts::{self, ElementSegmentSpec, FuncLayoutSealed, MemLayoutSealed, VirtualSpaceId},
    linkage::{
        LinkageInfo,
        file_db::{self, FileRelocs},
    },
    raw::{self, FuncTypeId, ImportId, SegmentId},
};

mod entities;
pub mod snapshot;
const INDIRECT_TABLE_NAME: &str = "__indirect_function_table";

const MEM_BASE_NAME: &str = "__memory_base";
const TABLE_BASE_NAME: &str = "__table_base";

impl_entity_index! {
    #[display = "file"]
    pub struct FileId;

    #[display = "sym"]
    pub struct SymbolId;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locked<'src> {
    pub start_function: Option<FunctionRef>,
    pub mem_layout: layouts::MemLayoutSealed<'src>,
    pub function_elements: layouts::FuncLayoutSealed<'src>,
    pub got_info: Option<GotInfo<GlobalRef>>,
}

impl Locked<'_> {
    /// Get segment used as indirect function table, if exist.
    ///
    /// Current implementation will find first active segment.
    pub fn get_indirect_fn_segment(&self) -> Option<SegmentId> {
        self.function_elements
            .segments
            .iter()
            .find(|(_, s)| s.kind.is_active())
            .map(|(id, _)| id)
    }
}

/// This is one of the phases of `Module` creation.
///
/// At this phase one can create new entities with temp indexes,
/// that can be converted to stable indexes after calling `into_locked`.
///
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct Builder<'src> {
    /// List of functions to be called on module start.
    /// Should have `()->void` type and can be defined or imported.
    pub start_functions: Vec<Temp<FunctionRef>>,
    pub mem_layout: layouts::MemLayoutBuilder<'src>,
    pub function_elements: layouts::FuncLayoutBuilder<'src>,
    pub got_info: Option<GotInfo<Temp<GlobalRef>>>,
}

impl Builder<'_> {
    /// Get virtual space used as indirect function table, if exist.
    ///
    /// Current implementation will find first active virtual space.
    pub fn get_indirect_fn_vs(&self) -> Option<VirtualSpaceId> {
        self.function_elements
            .virtual_spaces
            .iter()
            .find(|(_, s)| s.is_active())
            .map(|(id, _)| id)
    }
    pub fn has_got(&self) -> bool {
        self.got_info.is_some()
    }
}

pub trait IsLocked {
    type IsLocked;
}
impl IsLocked for Locked<'_> {
    type IsLocked = SealedState;
}
impl IsLocked for Builder<'_> {
    type IsLocked = BuilderState;
}
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
        Self::load_from_bytes(self, data)
    }

    #[tracing::instrument(skip_all)]
    pub fn load_from_bytes(&mut self, data: Box<[u8]>) -> Result<FileId> {
        let file =
            FileWithData::try_attach_to_cart(data, |data| LoadedFile::from_wasm_bytes(data))?;
        let id = self.files_readers.push(file);
        Ok(id)
    }

    pub fn get_file(&self, file_id: FileId) -> &LoadedFile<'_> {
        self.files_readers.get(file_id).unwrap().get()
    }

    pub fn get_snapshot(&self) -> snapshot::MultiSnapshot {
        snapshot::MultiSnapshot::new(self.files_readers.iter().map(|(_, file)| {
            let file = file.get();
            snapshot::EntitiesSnapshot::new_without_types(&file.module)
                .with_num_type_refs(file.wasm_reader.types.len() as u32)
        }))
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
    #[tracing::instrument(skip_all)]
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

    #[doc(hidden)]
    pub fn raw_reader(&self) -> &raw::ObjectReader<'src> {
        &self.wasm_reader
    }
}

pub type ModuleBuilder<'src> = ModuleGeneric<'src, Builder<'src>>;
pub type Module<'src> = ModuleGeneric<'src, Locked<'src>, SealedState>;

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
pub struct ModuleGeneric<
    'src,
    Phase: IsLocked = Locked<'src>,
    LockedState = <Phase as IsLocked>::IsLocked,
> {
    // wasm entities
    pub functions: entities::Functions<'src, LockedState>,
    pub tables: entities::Tables<'src, LockedState>,
    pub memories: entities::Memories<'src, LockedState>,
    pub globals: entities::Globals<'src, LockedState>,
    pub tags: entities::Tags<'src, LockedState>,

    /// Extra information that depend on state (data layout, indirect functions, start_functions)
    /// It is extracted in phase, because during build we don't have stable indexes.
    ///
    /// Checkout [`Locked`] and [`Builder`] for details.
    //
    pub extra: Phase,

    ///
    /// Types used by functions or elements table.
    ///
    /// For sealed module - it is just copy of types from original module, that preserves original order.
    /// For builder - it only containes extra types that used either by elements table without corresponding function.
    ///
    /// Most of function types are placed directly in function entities. And deduplicated during emitting.
    /// But some of elements may be not imported/defined and use functions from external modules.
    /// Also for type relocations, we need to provide preserve original order of types.
    ///
    pub extra_types: PrimaryMap<FuncTypeId, FuncType>,
}

impl<'src> Module<'src> {
    #[tracing::instrument(skip_all, name = "Creating typed module")]
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
            reader.code.defined_funcs.values().map(Into::into).collect(),
        )
        .into_finished()
        .extend_with_info(
            reader
                .names
                .functions
                .iter()
                .map(|(id, name)| (id, (*name).into()))
                .collect(),
            exports.0,
        );
        let mut tables =
            entities::Tables::new_raw(imports.1, reader.tables.values().map(Into::into).collect())
                .into_finished()
                .extend_with_info(
                    reader
                        .names
                        .tables
                        .iter()
                        .map(|(id, name)| (id, (*name).into()))
                        .collect(),
                    exports.1,
                );
        let mut memories = entities::Memories::new_raw(
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

        let (table_name, table_id) = Self::try_init_indirect_fn_table(&tables).unwrap_or_else(|| {
                panic!("No named __indirect_function_table was found, and there is not one table in the module.")
        });

        tables.get_entity_mut(table_id).set_name(table_name);

        let (memory_name, memory_id) = Self::try_init_base_memory(&memories).unwrap_or_else(|| {
            panic!("No named __base_memory was found, and there is not one memory in the module.");
        });

        memories.get_entity_mut(memory_id).set_name(memory_name);

        let indirect_fns = FuncLayoutSealed::typed_from_reader(reader)?;

        let g = tracing::info_span!("processing_extra_linkage").entered();

        drop(g);
        let (mem_layout, file_symbol_db_new) = MemLayoutSealed::from_reader(reader)?;

        let this = Module {
            functions,
            tables,
            memories,
            globals,
            tags,
            extra: Locked {
                start_function: reader.code.start_func,
                mem_layout,
                function_elements: indirect_fns,
                // TODO: fill got info based on name/init expression of data/elements
                got_info: None,
            },
            extra_types: reader.types.clone(),
        };

        Ok((this, file_symbol_db_new))
    }

    fn try_init_indirect_fn_table(
        tables: &entities::Tables<'src>,
    ) -> Option<(Cow<'src, str>, TableRef)> {
        tables
            .iter()
            .filter_map(|(id, def)| def.name().cloned().map(|name| (name, id)))
            .find(|(name, _)| *name == INDIRECT_TABLE_NAME)
            .or_else(|| {
                if tables.len() == 1 {
                    Some((INDIRECT_TABLE_NAME.into(), tables.iter().next().unwrap().0))
                } else {
                    None
                }
            })
    }

    fn try_init_base_memory(
        memories: &entities::Memories<'src>,
    ) -> Option<(Cow<'src, str>, MemoryRef)> {
        memories
            .iter()
            .filter_map(|(id, def)| def.name().cloned().map(|name| (name, id)))
            .find(|(name, _)| *name == MEM_BASE_NAME)
            .or_else(|| {
                if memories.len() == 1 {
                    Some((MEM_BASE_NAME.into(), memories.iter().next().unwrap().0))
                } else {
                    None
                }
            })
    }

    /// Get entity name
    pub fn get_name(&self, entity: EntityKind) -> Cow<'src, str> {
        let debug_name = match entity {
            EntityKind::Function(func_id) => self.functions.get_entity(func_id).name().cloned(),
            EntityKind::Global(global_id) => self.globals.get_entity(global_id).name().cloned(),
            EntityKind::Table(table_id) => self.tables.get_entity(table_id).name().cloned(),
            EntityKind::Memory(mem_id) => self.memories.get_entity(mem_id).name().cloned(),
            EntityKind::Tag(tag_id) => self.tags.get_entity(tag_id).name().cloned(),
            EntityKind::DataSymbol(d) => self.extra.mem_layout.get_entity(d).name().cloned(),
            EntityKind::Type(_) => None, // types don't have names in name section
        };
        debug_name.unwrap_or_else(|| format!("{entity}").into())
    }
    /// Get entity type
    pub fn get_type(&self, entity: EntityKind) -> Option<EntityType> {
        Some(match entity {
            EntityKind::Global(g) => EntityType::Global(*self.globals.get_entity(g).get_type()),
            EntityKind::Table(t) => EntityType::Table(*self.tables.get_entity(t).get_type()),
            EntityKind::Memory(m) => EntityType::Memory(*self.memories.get_entity(m).get_type()),
            EntityKind::Tag(t) => EntityType::Tag(*self.tags.get_entity(t).get_type()),
            EntityKind::Function(f) => {
                EntityType::Function(self.functions.get_entity(f).get_type().clone())
            }
            EntityKind::DataSymbol(_) => EntityType::DataSymbol(()),
            EntityKind::Type(_) => return None, // types don't have types
        })
    }
    /// Calculate estimated size of entity.
    pub fn get_body_len(&self, entity: EntityKind) -> usize {
        match entity {
            EntityKind::DataSymbol(d) => self
                .extra
                .mem_layout
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
            .extra
            .mem_layout
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
        let data = self.extra.mem_layout.defined_iter().map(map_body);

        chain!(functions, globals, tables, data)
    }

    pub fn modify_entities_bodies(
        &mut self,
        mut op: impl FnMut(EntityKind, &mut EntityBody<'src>),
    ) {
        fn map_body_mut<'any, 'src, T>(
            (v, def): (impl Into<EntityKind>, &'any mut DefinedEntity<'src, T>),
        ) -> (EntityKind, &'any mut EntityBody<'src>) {
            (v.into(), &mut def.body)
        }
        let functions = self.functions.defined_iter_mut().map(map_body_mut);
        let globals = self.globals.defined_iter_mut().map(map_body_mut);
        let tables = self.tables.defined_iter_mut().map(map_body_mut);
        chain!(functions, globals, tables).for_each(|(d, e)| op(d, e));

        self.extra
            .mem_layout
            .modify_bodies(|data_ref, def| op(data_ref.into(), &mut def.body));
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
        let existing_segment = self.extra.get_indirect_fn_segment().expect("No segment found for indirect function table, call create_empty_indirect_fn_table first");

        let result = file_relocs.list_indirect_fns().into_iter();

        self.extra.function_elements.segments[existing_segment]
            .parts
            .extend(result);
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
            tables: entities::Tables::default(),
            extra: Builder::default(),
            extra_types: PrimaryMap::new(),
        }
    }

    ///
    /// Creates new indirect function table if it doesn't exist.
    ///
    /// With create virtual space and table acording to got info.
    ///
    pub fn create_empty_indirect_fn_table(&mut self) -> crate::index::Temp<TableRef> {
        let indirect_fns = &mut self.extra.function_elements;
        if !indirect_fns.virtual_spaces.is_empty() {
            panic!(
                "Indirect function table is already defined  {:?}.",
                indirect_fns.virtual_spaces
            );
        }
        let table_ref = self
            .tables
            .iter()
            .find(|(_, t)| t.name().is_some_and(|n| n == INDIRECT_TABLE_NAME))
            .map(|(id, _)| id);

        let table_ref = table_ref.unwrap_or_else(|| {
            log::trace!("Creating new indirect function table with name {INDIRECT_TABLE_NAME}");
            self.tables.push_defined(DefinedEntity {
                entity_type: TableType {
                    table64: false,
                    shared: false,
                    initial: 0,
                    maximum: None,
                    element_type: wasmparser::RefType::FUNCREF,
                },
                body: EntityBody::new_empty(smallvec![]),
                name: Some(INDIRECT_TABLE_NAME.into()),
                export_as: ExportNames::new(),
            })
        });

        let location = match self.extra.got_info.as_ref() {
            Some(got_info) => layouts::SegmentPlacement::GotBased {
                global: got_info.table_base,
                offset: 0,
            },
            None => {
                layouts::SegmentPlacement::ConstantOffset(1) // skip reserved 0 for null reference
            }
        };

        let vs = indirect_fns
            .virtual_spaces
            .push(layouts::ElementKind::Active {
                table_ref,
                location,
            });

        if indirect_fns.segments.is_empty() {
            indirect_fns.segments.push(ElementSegmentSpec {
                vs_id: vs,
                name: INDIRECT_TABLE_NAME.into(),
            });
        }
        table_ref
    }
    /// Defines the default memory for the module.
    ///
    /// Panics: if memory_id for base memory was already set.
    pub fn create_base_memory(&mut self) -> crate::index::Temp<MemoryRef> {
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
    /// Also this method init start_fn based on list of start functions provided during building phase
    ///
    /// Panics: if no default memory/indirect function can be found.
    ///
    pub fn into_locked(mut self) -> Module<'src> {
        let tables = self.tables.into_finished();

        let memories = self.memories.into_finished();

        let fn_imports = self.functions.imports_iter().len();
        let fn_defined = self.functions.defined_iter().len();

        let start_fns = self
            .extra
            .start_functions
            .into_iter()
            .map(|temp| temp.to_stable(fn_imports, fn_defined))
            .collect::<Vec<_>>();

        let start_function = (start_fns.len() > 1).then(|| {
            let start_body = Self::generate_start_function(&start_fns);
            self.functions
                .push_defined(start_body)
                .to_stable(fn_imports, fn_defined)
        });

        let functions = self.functions.into_finished();
        let globals = self.globals.into_finished();
        let extra = Locked {
            start_function,
            mem_layout: self.extra.mem_layout.seal_at(
                5, // TODO: Make it less fragile (currently it relies on the fact that we use 5byte encoding for count)
                |temp| memories.stable_id(temp),
            ),
            function_elements: self
                .extra
                .function_elements
                .map_elements(|temp| functions.stable_id(temp))
                .seal(
                    |temp| tables.stable_id(temp),
                    |temp| globals.stable_id(temp),
                ),
            got_info: self.extra.got_info.map(|got_info| GotInfo {
                memory_base: globals.stable_id(got_info.memory_base),
                table_base: globals.stable_id(got_info.table_base),
            }),
        };

        Module {
            tables,
            memories,
            functions,
            globals,
            extra,
            extra_types: self.extra_types,
            tags: self.tags.into_finished(),
        }
    }

    fn generate_start_function(start_functions: &[FunctionRef]) -> DefinedFunction<'src> {
        let mut func = wasm_encoder::Function::new([]);
        let mut ixs = func.instructions();
        for func_ref in start_functions {
            ixs.call(func_ref.as_u32());
        }

        DefinedFunction {
            body: EntityBody::New {
                new_bytes: func.into_raw_body().into(),
                // TODO: add relocs
                new_relocs: smallvec![],
            },
            name: Some("_start".into()),
            entity_type: raw::FuncType::new(None, None),
            export_as: ExportNames::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use smallvec::smallvec;
    use wasmparser::FuncType;

    use super::LoadedFile;
    use crate::{
        index::Temp,
        layouts::ItemType,
        typed::{
            DefinedDataChunk, EntityBody, ExportNames, ImportedFunction, Module, ModuleBuilder,
        },
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
        assert_eq!(input_object.extra.mem_layout.item_places().len(), 127);
        assert_eq!(input_object.functions.len(), 706);

        let get_indirect_fns = |i: &Module| {
            i.extra
                .function_elements
                .segments
                .iter()
                .flat_map(|(_, segment)| segment.parts.iter())
                .map(|(_id, func_ref)| *func_ref)
                .collect::<Vec<_>>()
        };
        let mut indirect_fns = get_indirect_fns(&input_object);
        indirect_fns.sort();
        let len = indirect_fns.len();
        indirect_fns.dedup();
        assert_eq!(len, indirect_fns.len(),);

        input_object
            .extra
            .function_elements
            .segments
            .iter_mut()
            .for_each(|(_, segment)| segment.parts.clear());

        assert_eq!(get_indirect_fns(&input_object).len(), 0);
        input_object.extend_indirect_table_from_relocs(&file.relocs);
        let mut recovered_fns = get_indirect_fns(&input_object);

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
        let mut module = ModuleBuilder::new();
        module.functions.push_import(ImportedFunction {
            module: "env".into(),
            name: "bar".into(),
            renamed_as: None,
            export_as: ExportNames::default(),
            entity_type: FuncType::new(None, None), // void type
        });

        let segment_id = module
            .extra
            .mem_layout
            .try_create_base_segment(Temp::from_defined(0));
        module.extra.mem_layout.push_defined(DefinedDataChunk {
            body: EntityBody::New {
                new_bytes: vec![1, 2, 3].into(),
                new_relocs: smallvec![],
            },
            name: Some("data".into()),
            entity_type: ItemType::data_chunk(segment_id, 0),
            export_as: ExportNames::default(),
        });

        let module = module.into_locked();

        dbg!(&module.functions);
        assert_eq!(module.functions.len(), 1);

        assert_eq!(module.extra.mem_layout.item_places().len(), 1);
    }
}
