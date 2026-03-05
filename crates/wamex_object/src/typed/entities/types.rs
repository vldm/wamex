use std::{borrow::Cow, ops::Range};

use cranelift_bitset::CompoundBitSet;
use cranelift_entity::EntityRef;
use smallvec::SmallVec;
use wasmparser::TypeRef;

use super::{FunctionRef, GlobalRef, MemoryRef, TableRef, TagRef};
use crate::{
    SVec,
    emit::modify::{OutputEntityRef, Rewrite},
    linkage::reloc::EntityRelocationEntry,
    raw::{self, FunctionWithBody},
    typed::{self},
};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct ExportEntry<'a, IDX: EntityRef> {
    pub name: Cow<'a, str>,
    pub entity_index: IDX,
}

#[derive(Default, Clone, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct ExportNames<'src> {
    /// Multiple possible reexports names for same entity.
    pub names: Vec<Cow<'src, str>>,
    // NOTE: SVec<T> is invariant over T so we cannot use it here.
}
impl<'src> ExportNames<'src> {
    pub fn add_export(&mut self, name: Cow<'src, str>) {
        if !self.names.contains(&name) {
            self.names.push(name);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct ImportedEntity<'src, Type> {
    pub module: Cow<'src, str>,
    pub name: Cow<'src, str>,
    pub entity_type: Type,

    pub renamed_as: Option<Cow<'src, str>>,
    pub export_as: ExportNames<'src>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefinedEntity<'src, Type> {
    pub entity_type: Type,
    pub body: EntityBody<'src>,
    pub name: Option<Cow<'src, str>>,
    pub export_as: ExportNames<'src>,
}
impl<Type> DefinedEntity<'_, Type> {
    pub fn original_range(&self) -> Range<usize> {
        match &self.body {
            EntityBody::Copied { original_range, .. } => original_range.clone(),
            EntityBody::New { .. } => 0..0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WithoutBody<'src, Type> {
    pub entity_type: Type,
    pub name: Option<Cow<'src, str>>,
    pub export_as: ExportNames<'src>,
}

pub trait WithExtraInfo<'src> {
    fn export_as(&self) -> &ExportNames<'src>;
    fn export_as_mut(&mut self) -> &mut ExportNames<'src>;
    fn name(&self) -> Option<&Cow<'src, str>>;
    fn set_name(&mut self, name: Cow<'src, str>);
}

pub type ImportedFunction<'src> = ImportedEntity<'src, wasmparser::FuncType>;
pub type ImportedTable<'src> = ImportedEntity<'src, wasmparser::TableType>;
pub type ImportedMemory<'src> = ImportedEntity<'src, wasmparser::MemoryType>;
pub type ImportedGlobal<'src> = ImportedEntity<'src, wasmparser::GlobalType>;
pub type ImportedTag<'src> = ImportedEntity<'src, wasmparser::TagType>;
pub type ImportedDataChunk<'src> = ImportedEntity<'src, ()>; // it's untyped chunk of data - so no "type" can be assigned to it.

pub type DefinedFunction<'src> = DefinedEntity<'src, wasmparser::FuncType>; // body - function locals + instructions
pub type DefinedTable<'src> = DefinedEntity<'src, wasmparser::TableType>; // body - init expr (instructions)
pub type DefinedGlobal<'src> = DefinedEntity<'src, wasmparser::GlobalType>; // body - init expr (instructions)
pub type DefinedMemory<'src> = WithoutBody<'src, wasmparser::MemoryType>;
pub type DefinedTag<'src> = WithoutBody<'src, wasmparser::TagType>;
pub type DefinedDataChunk<'src> = DefinedEntity<'src, typed::data::DataChunkType>; // body - bytes in memory

impl<'src, Any> WithExtraInfo<'src> for ImportedEntity<'src, Any> {
    fn export_as(&self) -> &ExportNames<'src> {
        &self.export_as
    }
    fn export_as_mut(&mut self) -> &mut ExportNames<'src> {
        &mut self.export_as
    }
    fn name(&self) -> Option<&Cow<'src, str>> {
        Some(&self.name)
    }
    fn set_name(&mut self, name: Cow<'src, str>) {
        self.name = name;
    }
}

impl<'src, Any> WithExtraInfo<'src> for DefinedEntity<'src, Any> {
    fn export_as(&self) -> &ExportNames<'src> {
        &self.export_as
    }
    fn export_as_mut(&mut self) -> &mut ExportNames<'src> {
        &mut self.export_as
    }
    fn name(&self) -> Option<&Cow<'src, str>> {
        self.name.as_ref()
    }
    fn set_name(&mut self, name: Cow<'src, str>) {
        self.name = Some(name);
    }
}

impl<'src, Any> WithExtraInfo<'src> for WithoutBody<'src, Any> {
    fn export_as(&self) -> &ExportNames<'src> {
        &self.export_as
    }
    fn export_as_mut(&mut self) -> &mut ExportNames<'src> {
        &mut self.export_as
    }
    fn name(&self) -> Option<&Cow<'src, str>> {
        self.name.as_ref()
    }
    fn set_name(&mut self, name: Cow<'src, str>) {
        self.name = Some(name);
    }
}

impl<'src> From<typed::data::RawDataChunk<'src>> for DefinedDataChunk<'src> {
    fn from(v: typed::data::RawDataChunk<'src>) -> DefinedDataChunk<'src> {
        DefinedEntity {
            entity_type: typed::data::DataChunkType {
                segment_id: v.segment_id,
                pow2align: v.pow2align,
            },
            body: EntityBody::Copied {
                original_range: v.original_offset..(v.original_offset + v.data.len()),
                bytes: v.data,
                patches: vec![],
                filtered_relocs: CompoundBitSet::new(),
            },
            name: Some(v.name),
            export_as: ExportNames::default(),
        }
    }
}

impl<'src> From<&FunctionWithBody<'src>> for DefinedFunction<'src> {
    fn from(v: &FunctionWithBody<'src>) -> DefinedFunction<'src> {
        DefinedFunction {
            entity_type: v.func_type.clone(),
            body: EntityBody::Copied {
                original_range: v.body.range(),
                bytes: v.body.as_bytes(),
                patches: vec![],
                filtered_relocs: CompoundBitSet::new(),
            },
            name: None,
            export_as: ExportNames::default(),
        }
    }
}

impl<'src> From<&raw::Table<'src>> for DefinedTable<'src> {
    fn from(v: &raw::Table<'src>) -> DefinedTable<'src> {
        let (body, original_range) = match &v.init {
            wasmparser::TableInit::RefNull => (&[] as &[u8], 0usize..0),
            wasmparser::TableInit::Expr(e) => {
                // e.get_binary_reader().remaining_buffer() - private
                let mut reader = e.get_binary_reader();
                (
                    reader.read_bytes(reader.bytes_remaining()).unwrap(),
                    e.get_binary_reader().range(),
                )
            }
        };

        DefinedEntity {
            entity_type: v.ty,
            body: EntityBody::Copied {
                original_range,
                bytes: body,
                patches: vec![],
                filtered_relocs: CompoundBitSet::new(),
            },
            name: None,
            export_as: ExportNames::default(),
        }
    }
}

impl<'src> From<&raw::Global<'src>> for DefinedGlobal<'src> {
    fn from(v: &raw::Global<'src>) -> DefinedGlobal<'src> {
        let (body, original_range) = {
            // remaining_buffer() - private
            let mut reader = v.init_expr.get_binary_reader();
            (
                reader.read_bytes(reader.bytes_remaining()).unwrap(),
                v.init_expr.get_binary_reader().range(),
            )
        };

        DefinedEntity {
            entity_type: v.ty,
            body: EntityBody::Copied {
                original_range,
                bytes: body,
                patches: vec![],
                filtered_relocs: CompoundBitSet::new(),
            },
            name: None,
            export_as: ExportNames::default(),
        }
    }
}
impl<'src, V: Clone> From<&V> for WithoutBody<'src, V> {
    fn from(v: &V) -> WithoutBody<'src, V> {
        WithoutBody {
            entity_type: v.clone(),
            export_as: ExportNames::default(),
            name: None,
        }
    }
}

/// Body of a defined entity
/// For functions it's locals + instructions;
/// For globals/tables it's the initializers;
/// For memories/tags - no body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntityBody<'src> {
    /// Copy of original entity with optional patches.
    Copied {
        /// Range in the original module's binary where the body of this entity is located.
        original_range: Range<usize>,
        /// Original body of the entity, copied from the original module.
        /// This is used as a base for later patching and relocs application.
        /// Relocations are stored separately, to reduce size of the EntityDefinition.
        bytes: &'src [u8],
        /// Patches to apply to the original body.
        patches: Vec<Rewrite>,
        /// Marker that some reloc was removed during patching.
        filtered_relocs: CompoundBitSet,
    },
    /// New body for entities without body in src file.
    // for wamex-split purposes it's IndirectTrampoline.
    New {
        /// Relocations with offsets relative to body start,
        /// and referencing new symbol in output module.
        ///
        /// This kind of body, cannot use entities from input module (like in EntityBody::Copied),
        /// because we doesn't store FileId for them.
        new_relocs: SVec<EntityRelocationEntry, 2>,
        new_bytes: SVec<u8, 32>,
    },
}

impl EntityBody<'_> {
    pub fn len(&self) -> usize {
        match self {
            EntityBody::Copied { bytes, patches, .. } => {
                let start_len = bytes.len() as isize;
                patches
                    .iter()
                    .fold(start_len, |len, patch| len + patch.size()) as usize
            }
            EntityBody::New { new_bytes, .. } => new_bytes.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn iter_chunks(&self) -> impl Iterator<Item = &[u8]> {
        match self {
            EntityBody::Copied { bytes, patches, .. } => IterBytes::new(bytes, patches),
            EntityBody::New { new_bytes, .. } => IterBytes::new(new_bytes, &[]),
        }
    }

    /// Iterate over resulting body bytes, applying patches on the fly.
    pub fn iter_bytes(&self) -> impl Iterator<Item = u8> + '_ {
        match self {
            EntityBody::Copied { bytes, patches, .. } => IterBytes::new(bytes, patches),
            EntityBody::New { new_bytes, .. } => IterBytes::new(new_bytes, &[]),
        }
        .flat_map(|chunk| chunk.iter().copied())
    }
    pub fn original_range(&self) -> Range<usize> {
        match self {
            EntityBody::Copied { original_range, .. } => original_range.clone(),
            EntityBody::New { .. } => 0..0,
        }
    }
}

// Iter that print in place patched body.
#[derive(Debug)]
struct IterBytes<'a> {
    bytes: &'a [u8],
    patches: &'a [Rewrite],
    original_offset: usize,
}
impl<'a> IterBytes<'a> {
    fn new(bytes: &'a [u8], patches: &'a [Rewrite]) -> Self {
        Self {
            bytes,
            patches,
            original_offset: 0,
        }
    }
}

impl<'a> Iterator for IterBytes<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(patch) = self.patches.first() {
            // if in patch range
            if patch.old_range.start >= self.original_offset {
                self.patches = &self.patches[1..];
                self.original_offset = patch.old_range.end;
                return Some(&patch.new_bytes[..]);
            } else {
                let start = self.original_offset;
                let end = patch.old_range.start;
                self.original_offset = patch.old_range.start;
                return Some(&self.bytes[start..end]);
            }
        }
        if self.bytes[self.original_offset..].is_empty() {
            return None;
        }
        let start = self.original_offset;
        self.original_offset = self.bytes.len();
        Some(&self.bytes[start..])
    }
}

pub type Imports<'a> = (
    Vec<ImportedFunction<'a>>,
    Vec<ImportedTable<'a>>,
    Vec<ImportedMemory<'a>>,
    Vec<ImportedGlobal<'a>>,
    Vec<ImportedTag<'a>>,
);

pub fn read_imports<'a>(reader: &crate::raw::ObjectReader<'a>) -> crate::Result<Imports<'a>> {
    let mut imported_funcs: Vec<ImportedFunction<'a>> = Vec::new();
    let mut imported_tables: Vec<ImportedTable<'a>> = Vec::new();
    let mut imported_memories: Vec<ImportedMemory<'a>> = Vec::new();
    let mut imported_globals: Vec<ImportedGlobal<'a>> = Vec::new();
    let mut imported_tags: Vec<ImportedTag<'a>> = Vec::new();
    for (_import_id, import) in reader.imports.iter() {
        match import.ty {
            TypeRef::Func(num) => {
                let id = raw::FuncTypeId::from_u32(num);

                imported_funcs.push(ImportedFunction {
                    module: import.module.into(),
                    name: import.name.into(),
                    entity_type: reader.types[id].clone(),
                    renamed_as: None,
                    export_as: ExportNames::default(),
                });
            }
            TypeRef::Table(ref table_type) => {
                imported_tables.push(ImportedTable {
                    module: import.module.into(),
                    name: import.name.into(),
                    entity_type: *table_type,
                    renamed_as: None,
                    export_as: ExportNames::default(),
                });
            }
            TypeRef::Memory(ref memory_type) => {
                imported_memories.push(ImportedMemory {
                    module: import.module.into(),
                    name: import.name.into(),
                    entity_type: *memory_type,
                    renamed_as: None,
                    export_as: ExportNames::default(),
                });
            }
            TypeRef::Global(ref global_type) => {
                imported_globals.push(ImportedGlobal {
                    module: import.module.into(),
                    name: import.name.into(),
                    entity_type: *global_type,
                    renamed_as: None,
                    export_as: ExportNames::default(),
                });
            }
            TypeRef::Tag(ref tag_type) => {
                imported_tags.push(ImportedTag {
                    module: import.module.into(),
                    name: import.name.into(),
                    entity_type: *tag_type,
                    renamed_as: None,
                    export_as: ExportNames::default(),
                });
            }
        }
    }
    Ok((
        imported_funcs,
        imported_tables,
        imported_memories,
        imported_globals,
        imported_tags,
    ))
}

pub type Exports<'a> = (
    Vec<ExportEntry<'a, FunctionRef>>,
    Vec<ExportEntry<'a, TableRef>>,
    Vec<ExportEntry<'a, MemoryRef>>,
    Vec<ExportEntry<'a, GlobalRef>>,
    Vec<ExportEntry<'a, TagRef>>,
);

pub fn read_exports<'a>(reader: &crate::raw::ObjectReader<'a>) -> crate::Result<Exports<'a>> {
    let mut func_exports: Vec<ExportEntry<'a, FunctionRef>> = Vec::new();
    let mut table_exports: Vec<ExportEntry<'a, TableRef>> = Vec::new();
    let mut memory_exports: Vec<ExportEntry<'a, MemoryRef>> = Vec::new();
    let mut global_exports: Vec<ExportEntry<'a, GlobalRef>> = Vec::new();
    let mut tag_exports: Vec<ExportEntry<'a, TagRef>> = Vec::new();
    for (_export_id, export) in reader.exports.iter() {
        match export.kind {
            wasmparser::ExternalKind::Func => {
                func_exports.push(ExportEntry {
                    name: export.name.into(),
                    entity_index: FunctionRef::from_u32(export.index),
                });
            }
            wasmparser::ExternalKind::Table => {
                table_exports.push(ExportEntry {
                    name: export.name.into(),
                    entity_index: TableRef::from_u32(export.index),
                });
            }
            wasmparser::ExternalKind::Memory => {
                memory_exports.push(ExportEntry {
                    name: export.name.into(),
                    entity_index: MemoryRef::from_u32(export.index),
                });
            }
            wasmparser::ExternalKind::Global => {
                global_exports.push(ExportEntry {
                    name: export.name.into(),
                    entity_index: GlobalRef::from_u32(export.index),
                });
            }
            wasmparser::ExternalKind::Tag => {
                tag_exports.push(ExportEntry {
                    name: export.name.into(),
                    entity_index: TagRef::from_u32(export.index),
                });
            }
        }
    }
    Ok((
        func_exports,
        table_exports,
        memory_exports,
        global_exports,
        tag_exports,
    ))
}

const _ASSERT_SIZE: () = {
    assert!(std::mem::size_of::<EntityRelocationEntry>() == 24);
    assert!(std::mem::size_of::<EntityBody>() == 112);
};
