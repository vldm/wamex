use std::{
    any::Any,
    collections::{BTreeMap, BTreeSet},
    fmt::Debug,
    io::Write,
    ops::{Range, RangeFrom, RangeTo},
};

use anyhow::{Result, bail, ensure};
use cranelift_entity::EntityRef;
use wasm_encoder::{Encode, GlobalType, Instruction};
use wasmparser::Operator;

use crate::{
    InputObject,
    emit::{
        builder,
        globals::DefinedGlobal,
        index_safety::{OutputFuncId, OutputGlobalId},
        modify::{
            ModifyContext, StoreType, SymbolOffset, SymbolOp, init_each_store_var,
            relocation::{self, DataSymbolTag, FunctionIndexTag},
            wasm_emitter::MemArgOffsets,
        },
    },
    helpers::{ShiftMap, ShiftPoint},
    read::Relocation,
    symbols::{
        SymbolId, SymbolKind,
        reloc::{
            AnyRelocationEntry, Encoding, Relative, RelocationEntry, RelocationWidth, SymbolType,
        },
    },
};

///
/// Read-only cursor over a byte slice that focus on specific region.
/// It splits memory into three zones, green, red and grey.
///
/// Green zone is the area for which this cursor was created.
/// Red zone is the area that used by other cursors and therefore "untouchable".
/// grey zone is the area that is outside of both green and red zones,
/// and current that current cursor can extend into if needed.
///
///
/// The aim of this object is to handle overlapping of modification entries.
pub struct Cursor<'any> {
    buffer: &'any [u8],
    green: Range<usize>,
    red_before: RangeTo<usize>,
    red_after: RangeFrom<usize>,
}

impl<'any> Cursor<'any> {
    // Create a new cursor with an empty buffer
    pub fn new(
        buffer: &'any [u8],
        green: Range<usize>,
        red_before: RangeTo<usize>,
        red_after: RangeFrom<usize>,
    ) -> Self {
        Self {
            buffer,
            green,
            red_before,
            red_after,
        }
    }

    // Get the current green zone
    pub fn green_buf(&self) -> &'any [u8] {
        &self.buffer[self.green.clone()]
    }
    // Try to extend the green zone before its current start.
    // By moving the start backwards into grey zone.
    //
    // Returns true if successful, false if blocked by red zone.
    pub fn try_extend_before(&mut self, shift_left: usize) -> Result<()> {
        let new_start = self.green.start.saturating_sub(shift_left);
        if new_start < self.red_before.end {
            return Err(anyhow::anyhow!("Already used by other modification"));
        }
        self.green.start = new_start;
        Ok(())
    }

    // Try to extend the green zone after its current end.
    // By moving the end forward into grey zone.
    // Returns true if successful, false if blocked by red zone.
    pub fn try_extend_after(&mut self, shift_right: usize) -> Result<()> {
        let new_end = self.green.end.saturating_add(shift_right);
        if new_end > self.red_after.start {
            return Err(anyhow::anyhow!("Already used by other modification"));
        }

        self.green.end = new_end;
        Ok(())
    }
}

type SVec<T, const SIZE: usize = 32> = smallvec::SmallVec<[T; SIZE]>;

/// Entity id that can represent either input or output entity, which external entity type tag.
/// Used in relocation entries to remove need's of `SymbolId` and symbol table lookups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErasedEntityId {
    Input(u32),
    Output(u32),
}

// Packed enum of Input| Output entity ids
impl ErasedEntityId {
    pub fn from_output_id(id: u32) -> Self {
        ErasedEntityId::Output(id)
    }
    pub fn from_input_id(id: u32) -> Self {
        ErasedEntityId::Input(id)
    }
}

// TODO: DataId

///
/// Represents a rewrite operation that modifies a specific range of bytes.
/// It can be new instructions or data placement, inside `Symbol`.
/// Or some removing/replacing of existing bytes, that cannot be done via relocations.
///
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewrite {
    pub old_range: std::ops::Range<usize>,
    // TODO: Maybe put in common pool?
    pub new_relocs: SVec<RelocationEntry<ErasedEntityId>, 2>,
    pub new_bytes: SVec<u8>,
}

impl Rewrite {
    /// Get the size difference between new and old data.
    pub fn size(&self) -> isize {
        self.new_bytes.len() as isize - self.old_range.len() as isize
    }
}

/// Represents a modification entry that describes changes to be made
/// to a specific range of bytes in a WebAssembly module.
///
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModificationEntry<D = ()> {
    /// Indicates if this entry rewrites some of the original bytes.
    /// It could be replacing of
    pub rewrite: Option<Rewrite>,
    // Debug?/Trace info about original relocation
    pub original_reloc: RelocationEntry,
    // if this reloc needs to be handled with extra data
    pub extra_info: D,
}

/// Represents either a modification entry or a relocation entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModifyOrReloc<D> {
    Modify(ModificationEntry<D>),
    OriginalReloc(AnyRelocationEntry),
}

#[derive(Default, Debug, Clone, Copy)]
pub struct ModuleConfig {
    // Is this module is emitting as position-independent code
    pub dyn_base: bool,
}

// Future desing of moidifcation trait:
// 1. setup (Builder, ListOfEntries):
// - can insert some types into builder and get their ids
// - This should return Context
// 2. create_entries(ctx, mut old_relocs) -> [ModificationEntry<D>],
// 3. write_body(old_bytes, [rewrites]) -> new_bytes
// 4. new_bytes.apply_relocs(old_relocs, [new_relocs])
// 5. handle_extra_data(entry, ctx)

/// Implementation of modification routine.
pub trait HandleReloc<'src> {
    type ExtraData;

    /// Make setup specific for module
    ///
    /// e.g. init some funcs/globals/types in builder
    /// that will be used during modification entries creation or application.
    fn setup(
        &mut self,
        _module_config: ModuleConfig,
        _builder: &mut crate::emit::builder::ObjectBuilder<'src>,
    ) -> Result<()> {
        Ok(())
    }

    // TODO: Suport modification that need two or more relocs
    // e.g., for got-relative addressing
    fn create_entry(
        &self,
        relocation_state: &ResolveSymbol<'_, 'src>,
        buffer: Cursor<'src>,
        entry: AnyRelocationEntry,
    ) -> Result<ModifyOrReloc<Self::ExtraData>>;

    fn create_entries(
        &self,
        relocation_state: &ResolveSymbol<'_, 'src>,
        target: RelocTarget<'_, 'src>,
    ) -> Result<Vec<ModifyOrReloc<Self::ExtraData>>> {
        let mut result = vec![];
        let mut entries = target.entries.iter().peekable();
        let mut prev_range = ..0usize;

        while let Some(entry) = entries.next() {
            let red_after = entries
                .peek()
                .map(|e| e.offset() as usize..)
                .unwrap_or(target.src_data.len()..);
            let cursor = Cursor::new(
                target.src_data,
                entry.relocation_range(),
                prev_range,
                red_after,
            );

            result.push(self.create_entry(relocation_state, cursor, *entry)?);

            prev_range = ..entry.offset() as usize;
        }
        Ok(result)
    }

    // Handle extra data after all modifications are applied
    fn finalize(
        &mut self,
        _extra_data: Self::ExtraData,
        _builder: &mut crate::emit::builder::ObjectBuilder<'_>,
    ) -> Result<()> {
        Ok(())
    }
}

pub struct ResolveSymbol<'any, 'src> {
    pub input_module: &'any InputObject<'src>,
}

impl<'any, 'src> ResolveSymbol<'any, 'src> {
    fn from_input_id(&self, symbol_id: SymbolId, symbol_type: SymbolType) -> ErasedEntityId {
        let rec = self
            .input_module
            .symbols
            .get(symbol_id)
            .expect("Invalid symbol id");
        match (symbol_type, rec.kind) {
            (SymbolType::GlobalIndex, SymbolKind::Global(g)) => {
                ErasedEntityId::from_input_id(g.as_u32())
            }
            (
                SymbolType::FunctionIndex | SymbolType::TableIndex | SymbolType::FunctionOffset,
                SymbolKind::Func { input_id },
            ) => ErasedEntityId::from_input_id(input_id.as_u32()),
            (
                SymbolType::MemoryAddr | SymbolType::MemoryAddrLocrel,
                SymbolKind::DataDefined { .. },
            ) => ErasedEntityId::from_input_id(
                self.input_module
                    .data_symbols
                    .get(&symbol_id)
                    .unwrap()
                    .as_u32(),
            ),
            (SymbolType::TableNumber, SymbolKind::Table(t)) => {
                ErasedEntityId::from_input_id(t.as_u32())
            }
            (SymbolType::EventIndex, _) => {
                panic!("Events are not yet supported")
            }
            _ => {
                panic!(
                    "Unsupported symbol type for relocation state from input id, expected kind {:?}, got {:?}",
                    symbol_type, rec.kind
                )
            }
        }
    }

    fn resolve_relocation_entry(
        &self,
        entry: &AnyRelocationEntry,
    ) -> Result<RelocationEntry<ErasedEntityId>> {
        Ok(match entry {
            AnyRelocationEntry::Linkage(entry) => RelocationEntry {
                symbol_id: self.from_input_id(entry.symbol_id, entry.symbol_type),
                offset: entry.offset,
                encoding: entry.encoding,
                width: entry.width,
                relation: entry.relation,
                symbol_type: entry.symbol_type,
                addend: entry.addend,
            },
            AnyRelocationEntry::Type(entry) => RelocationEntry {
                symbol_id: ErasedEntityId::from_input_id(entry.index.as_u32()),
                offset: entry.offset,
                encoding: Encoding::Leb,
                width: RelocationWidth::Bits32,
                relation: Relative::None,
                symbol_type: SymbolType::TypeIndex,
                addend: 0,
            },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModificationsResult<'src> {
    pub src_data: &'src [u8],
    // TODO: We can do calculate size of destination buffer using original size + sum(new_size - old_size)
    // pub final_size: u32,
    pub patches: Vec<Rewrite>,
    // Relocs with ids converted to entity space in output module
    pub resolved_relocs: Vec<RelocationEntry<ErasedEntityId>>,
}
impl<'src> ModificationsResult<'src> {
    /// Create a new `ModificationsResult` with no modifications, but with original relocs resolved to entity ids.
    pub fn no_modifications(
        relocation_state: &ResolveSymbol<'_, 'src>,
        target: RelocTarget<'_, 'src>,
    ) -> Self {
        Self {
            src_data: target.src_data,
            patches: vec![],
            resolved_relocs: target
                .entries
                .iter()
                .map(|e| relocation_state.resolve_relocation_entry(e))
                .collect::<Result<Vec<_>>>()
                .unwrap(),
        }
    }

    /// Write byte using the modifications to the given writer.
    /// Returns shifted relocations.
    pub fn write_bytes(
        &self,
        writer: &mut impl Write,
    ) -> Result<Vec<RelocationEntry<ErasedEntityId>>> {
        let mut relocs = Vec::with_capacity(self.resolved_relocs.len() + self.patches.len() * 3); // rough estimate

        let mut shift_map = ShiftMap::new();
        let mut src_offset = 0usize;
        for patch in &self.patches {
            // write unchanged bytes before patch
            if patch.old_range.start > src_offset {
                writer.write_all(&self.src_data[src_offset..patch.old_range.start])?;
            }

            // Apply shifts to patch relocations (this is done before adding patch shift point)
            for reloc in &patch.new_relocs {
                let shifted_offset = shift_map
                    .get_shifted_offset(reloc.offset)
                    .expect("new relocation cannot be in removed area");
                relocs.push(RelocationEntry {
                    offset: shifted_offset,
                    ..reloc.clone()
                });
            }
            // then add new shift point
            shift_map.add_shift_point(ShiftPoint {
                at: patch.old_range.end as u32,
                shift: patch.size() as i32,
            });

            // write new bytes
            writer.write_all(&patch.new_bytes)?;
            src_offset = patch.old_range.end;
        }
        // write remaining bytes
        if src_offset < self.src_data.len() {
            writer.write_all(&self.src_data[src_offset..])?;
        }

        // Now we can add pre-existing relocs with shifts applied
        for reloc in &self.resolved_relocs {
            let shifted_offset = shift_map
                .get_shifted_offset(reloc.offset)
                .expect("relocation cannot be in removed area");
            relocs.push(RelocationEntry {
                offset: shifted_offset,
                ..reloc.clone()
            });
        }
        Ok(relocs)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RelocTarget<'any, 'src> {
    pub src_data: &'src [u8],
    pub entries: &'any [AnyRelocationEntry],
}

// Erased version of `HandleReloc` that stores `ExtraData` in self.
// While `HandleReloc` allows implementing different strategies (multithreading, etc) this one is internal helper for `SequencedHandler`.
pub trait HandleRelocErased<'src> {
    // Setup module-specific data in builder
    fn setup(&mut self, builder: &mut crate::emit::builder::ObjectBuilder<'src>) -> Result<()>;
    // Build modifications for given relocation entries and source data
    fn build_modifications(
        &mut self,
        relocation_state: &ResolveSymbol<'_, 'src>,
        target: RelocTarget<'_, 'src>,
    ) -> Result<ModificationsResult<'src>>;
    // Handle extra data after all modifications are applied
    fn finalize(&mut self, builder: &mut crate::emit::builder::ObjectBuilder<'src>) -> Result<()>;
}

pub struct SequencedHandler<'src, C: HandleReloc<'src>> {
    pub config: ModuleConfig,
    pub handler: C,
    pub artifacts: Vec<C::ExtraData>,

    _pd: std::marker::PhantomData<&'src ()>,
}

impl<'src, C: HandleReloc<'src>> SequencedHandler<'src, C> {
    pub fn new(config: ModuleConfig, context: C) -> Self {
        Self {
            config,
            handler: context,
            artifacts: vec![],
            _pd: std::marker::PhantomData,
        }
    }
}

impl<'src, C: HandleReloc<'src>> HandleRelocErased<'src> for SequencedHandler<'src, C>
where
    C::ExtraData: Debug,
{
    fn setup(&mut self, builder: &mut crate::emit::builder::ObjectBuilder<'src>) -> Result<()> {
        self.handler.setup(self.config.clone(), builder)
    }

    fn build_modifications(
        &mut self,
        relocation_state: &ResolveSymbol<'_, 'src>,
        target: RelocTarget<'_, 'src>,
    ) -> Result<ModificationsResult<'src>> {
        let modifications = self.handler.create_entries(relocation_state, target)?;
        let mut patches = vec![];
        let mut resolved_relocs = vec![];

        for modification in modifications {
            match modification {
                ModifyOrReloc::Modify(mod_entry) => {
                    if let Some(rewrite) = mod_entry.rewrite {
                        patches.push(rewrite);
                    }
                    self.artifacts.push(mod_entry.extra_info);
                }
                ModifyOrReloc::OriginalReloc(orig_reloc) => {
                    let resolved_reloc = relocation_state.resolve_relocation_entry(&orig_reloc)?;
                    resolved_relocs.push(resolved_reloc);
                }
            }
        }
        Ok(ModificationsResult {
            src_data: target.src_data,
            patches,
            resolved_relocs,
        })
    }

    fn finalize(&mut self, builder: &mut crate::emit::builder::ObjectBuilder<'src>) -> Result<()> {
        for extra_data in self.artifacts.drain(..) {
            self.handler.finalize(extra_data, builder)?;
        }
        Ok(())
    }
}

// implementations

pub type CodeModifyResult<'src> = ModificationsResult<'src>;
#[derive(Debug)]
pub struct CodeRelocationHandler {
    pub global_base: OutputGlobalId,
    // Temporary globals for constant extraction
    pub global_tmps: BTreeMap<StoreType, OutputGlobalId>,
    // // Function name for debug purposes
    // pub function_name: &'any str,
    // relocation_state: RelocateState<'any, 'any>,
    // Current instruction
    // pub instruction: wasmparser::Operator<'any>,
    pub always_static_symbols: BTreeSet<SymbolId>,
    // TODO: remove
    // // Submodule GOT and Table base globals
    // pub lib_base_id: Option<OutputGlobalId>,
    // pub table_base_id: Option<OutputGlobalId>,

    // output writer
    // pub writer: &'any mut Vec<u8>,
    dyn_base: bool,
}

impl CodeRelocationHandler {
    pub fn new(always_static_symbols: &BTreeSet<SymbolId>) -> Self {
        Self {
            global_base: OutputGlobalId::new(0),
            global_tmps: BTreeMap::new(),
            always_static_symbols: always_static_symbols.clone(),
            dyn_base: false,
        }
    }
}

impl<'src> HandleReloc<'src> for CodeRelocationHandler {
    type ExtraData = ();
    fn setup(
        &mut self,
        module_config: ModuleConfig,
        builder: &mut crate::emit::builder::ObjectBuilder<'src>,
        /* extra info ?*/
    ) -> Result<()> {
        if !module_config.dyn_base {
            return Ok(());
        }
        debug_assert!(self.global_tmps.is_empty());
        // TODO: ensure_global_got_base exist
        for (store_type, val_type) in init_each_store_var() {
            //TODO: Don't assert that this id won't shift
            let global_id = builder.globals.imports.len() + builder.globals.defined.len();
            self.global_tmps
                .insert(store_type, OutputGlobalId::new(global_id));

            builder.add_defined_global(DefinedGlobal::WithConstructor(GlobalType {
                val_type,
                mutable: true,
                shared: false,
            }));
        }

        self.dyn_base = module_config.dyn_base;
        Ok(())
    }

    fn create_entry(
        &self,
        relocation_state: &ResolveSymbol<'_, 'src>,
        buffer: Cursor<'src>,
        entry: AnyRelocationEntry,
    ) -> Result<ModifyOrReloc<Self::ExtraData>> {
        // Only apply if dynamic base is enabled
        if !self.dyn_base {
            return Ok(ModifyOrReloc::OriginalReloc(entry));
        }
        match entry {
            AnyRelocationEntry::Linkage(entry) => {
                // TODO: move outside of this creation
                Self::check_whitelisted_code_relocation(&entry)?;

                match entry.symbol_type {
                    SymbolType::TableIndex | SymbolType::MemoryAddr
                        if !self.always_static_symbols.contains(&entry.symbol_id) =>
                    {
                        return self
                            .new_entry(relocation_state, buffer, entry)
                            .map(ModifyOrReloc::Modify);
                    }
                    _ => {}
                }
            }
            AnyRelocationEntry::Type(_) => {
                // bail!("Type relocations are not supported for constant extraction");
            }
        }

        Ok(ModifyOrReloc::OriginalReloc(entry))
    }
}

impl<'src> CodeRelocationHandler {
    fn new_entry(
        &self,
        relocation_state: &ResolveSymbol<'_, 'src>,
        mut buffer: Cursor<'src>,
        entry: RelocationEntry,
    ) -> Result<ModificationEntry<()>> {
        assert!(matches!(
            (entry.symbol_type, entry.encoding),
            (SymbolType::TableIndex, Encoding::Sleb)
                | (SymbolType::MemoryAddr, Encoding::Sleb)
                | (SymbolType::MemoryAddr, Encoding::Leb)
        ));

        let ix_size = match entry.encoding {
            Encoding::Leb => 2,  // memoryaddr_leb
            Encoding::Sleb => 1, // memoryaddr_sleb | tableindex_sleb
            _ => {
                bail!("Unsupported relocation type")
            }
        };
        buffer.try_extend_before(ix_size)?;

        let instruction = {
            let bin_reader = wasmparser::BinaryReader::new(&buffer.green_buf(), 0);
            let mut op_reader = wasmparser::OperatorsReader::new(bin_reader);
            let instr = op_reader.read()?;
            if !op_reader.eof() {
                bail!("Unexpected extra operators after reading instruction")
            }
            instr
        };
        Ok(ModificationEntry {
            rewrite: Some(self.generate_patch(relocation_state, entry, instruction)?),
            original_reloc: entry,
            extra_info: (),
        })
    }
    fn generate_patch(
        &self,
        relocation_state: &ResolveSymbol<'_, 'src>,
        entry: RelocationEntry,
        instruction: wasmparser::Operator<'src>,
    ) -> Result<Rewrite> {
        let rewrite = match (entry.symbol_type, entry.encoding) {
            (SymbolType::MemoryAddr, Encoding::Leb) => {
                self.replace_memory_offset_with_global_get(relocation_state, entry, instruction)?
            }
            (SymbolType::MemoryAddr, Encoding::Sleb) => {
                self.replace_const_get_with_global_get(relocation_state, entry, instruction)?
            }
            (SymbolType::TableIndex, Encoding::Sleb) => {
                self.replace_const_get_with_global_get(relocation_state, entry, instruction)?
            }
            _ => {
                bail!("Unsupported relocation type")
            }
        };

        Ok(rewrite)
    }

    // Simple replace of i32.const with global.get + i32.add
    // Retuns size of the replacement
    fn replace_const_get_with_global_get(
        &self,
        relocation_state: &ResolveSymbol<'_, 'src>,
        old_entry: RelocationEntry,
        instruction: wasmparser::Operator<'src>,
    ) -> Result<Rewrite> {
        ensure!(
            matches!(instruction, Operator::I32Const { .. }),
            "Unsupported relocation operand"
        );

        let got_offset = 0i32;
        let got_global_index = self.global_base;
        let mut new_bytes = SVec::new();
        let mut new_relocs = SVec::new();

        let mut writer = super::wasm_emitter::Encoder::new(
            &mut new_bytes,
            old_entry.relocation_range().start as u32,
        );

        log::trace!(
            "Replacing {src_ix:?} with gapped entry (global.get {got_ix} + i32.const {offset})",
            src_ix = instruction,
            got_ix = got_global_index,
            offset = got_offset
        );

        let got_rel_offset = writer.global_get(got_global_index.as_u32())?;
        let const_rel_offset = writer.i32_const(got_offset)?;
        writer.i32_add()?;

        // TODO: Return new list of relocations to GlobalGet and I32Const (GlobalIndexLeb + MemoryAddrLeb | TableIndexLeb)
        new_relocs.push(RelocationEntry {
            symbol_id: ErasedEntityId::from_output_id(got_global_index.as_u32()), // to symbol_id
            offset: got_rel_offset as u32,
            encoding: Encoding::Leb,
            width: RelocationWidth::Bits32,
            relation: Relative::None,
            symbol_type: SymbolType::GlobalIndex,
            addend: 0,
        });

        new_relocs.push(RelocationEntry {
            // TODO: Handle old memory index
            symbol_id: relocation_state.from_input_id(old_entry.symbol_id, old_entry.symbol_type),
            offset: const_rel_offset as u32,
            relation: Relative::Got,

            encoding: old_entry.encoding,
            width: old_entry.width,
            symbol_type: old_entry.symbol_type,
            addend: old_entry.addend,
        });
        Ok(Rewrite {
            old_range: old_entry.relocation_range(),
            new_relocs,
            new_bytes,
        })
    }

    // replaces *.store and *.load family with multiple operations with `global` variable:
    // example for store:
    // > f32.store ($local_var + offset)    :stack[dyn_offset -> value]
    // <
    // < global.set $f32_global_index       :stack[dyn_offset]                  // $f32_global_index = value
    // < global.get $lib_base               :stack[dyn_offset -> $lib_base]
    // < i32.add :stack[modified_offset]                                        // dyn_offset += $lib_base
    // < global.get $f32_global_index       :stack[modified_offset -> value]    // return back <value> to stack
    // < f32.store <extra_offset>                                               // original f32.store with <offset of data symbol in memory>
    // For load:
    // > i32.load_u8 ($local_var + offset)  :stack[dyn_offset]
    // < global.get $global_var_index       :stack[dyn_offset -> $global_var_index]
    // < i32.add                            :stack[modified_offset]             // dyn_offset += $global_var_index
    // < i32.load_u8 <extra_offset>                                             // original i32.load_u8 with <offset of data symbol in memory>

    /// Returns size of the replacement.
    fn replace_memory_offset_with_global_get(
        &self,
        relocation_state: &ResolveSymbol<'_, 'src>,
        entry: RelocationEntry,
        instruction: wasmparser::Operator<'_>,
    ) -> Result<Rewrite> {
        let got_offset = 0;
        let got_global_index = self.global_base;

        let mut new_bytes = SVec::new();
        let mut new_relocs = SVec::new();

        let mut writer = super::wasm_emitter::Encoder::new(
            &mut new_bytes,
            entry.relocation_range().start as u32,
        );
        let store = Self::store_type(&instruction)?;

        log::trace!(
            "Replacing {orig_ix:?} with [global.get {got_global_index} + i32.const {got_offset:?} + ix] global_store:{store:?}",
            orig_ix = instruction,
        );

        // TODO: Replace with local?
        // store <value> to temp global
        if let Some(store_type) = &store {
            let global_id = *self.global_tmps.get(store_type).unwrap();
            let reloc_offset = writer.global_get(global_id.as_u32())?;
            new_relocs.push(RelocationEntry {
                symbol_id: ErasedEntityId::from_output_id(global_id.as_u32()),
                offset: reloc_offset as u32,
                encoding: Encoding::Leb,
                symbol_type: SymbolType::GlobalIndex,
                width: RelocationWidth::Bits32,
                relation: Relative::None,
                addend: 0,
            })
        }
        let got_offset = writer.global_get(got_global_index.as_u32())?;
        writer.i32_add()?; // add offset from global_index variable to the dyn_offset part of instruction
        // restore <value> from temp global
        if let Some(store_type) = &store {
            let global_id = *self.global_tmps.get(store_type).unwrap();
            let reloc_offset = writer.global_get(global_id.as_u32())?;
            new_relocs.push(RelocationEntry {
                symbol_id: ErasedEntityId::from_output_id(global_id.as_u32()),
                offset: reloc_offset as u32,
                encoding: Encoding::Leb,
                symbol_type: SymbolType::GlobalIndex,
                width: RelocationWidth::Bits32,
                relation: Relative::None,
                addend: 0,
            })
        }
        let mem_offsets = self.encode_store_ix(&mut writer, &instruction)?; // And now write original instruction

        new_relocs.push(RelocationEntry {
            //TODO: Convert to OutputSymbolId
            symbol_id: relocation_state.from_input_id(entry.symbol_id, entry.symbol_type),
            offset: mem_offsets.offset as u32,
            relation: Relative::Got,

            encoding: entry.encoding,
            width: entry.width,
            symbol_type: entry.symbol_type,
            addend: entry.addend,
        });

        Ok(Rewrite {
            new_bytes,
            new_relocs,
            old_range: entry.relocation_range(),
        })
    }

    fn store_type(ix: &Operator) -> Result<Option<StoreType>> {
        Ok(match ix {
            Operator::F32Load { .. }
            | Operator::F64Load { .. }
            | Operator::I32Load { .. }
            | Operator::I64Load { .. }
            | Operator::I32Load8U { .. }
            | Operator::I32Load8S { .. }
            | Operator::I32Load16U { .. }
            | Operator::I32Load16S { .. }
            | Operator::I64Load8U { .. }
            | Operator::I64Load8S { .. }
            | Operator::I64Load16U { .. }
            | Operator::I64Load16S { .. }
            | Operator::I64Load32U { .. }
            | Operator::I64Load32S { .. } => None,

            Operator::I32Store { .. } => Some(StoreType::I32),
            Operator::I64Store { .. } => Some(StoreType::I64),
            Operator::I32Store8 { .. } => Some(StoreType::I32),
            Operator::I32Store16 { .. } => Some(StoreType::I32),
            Operator::I64Store8 { .. } => Some(StoreType::I64),
            Operator::I64Store16 { .. } => Some(StoreType::I64),
            Operator::I64Store32 { .. } => Some(StoreType::I64),
            _ => {
                bail!("Unsupported relocation instruction: {instr:?}", instr = ix);
            }
        })
    }

    fn encode_store_ix<W: Write>(
        &self,
        writer: &mut super::wasm_emitter::Encoder<W>,
        ix: &Operator,
    ) -> Result<MemArgOffsets> {
        let fix_offset = |memarg: &wasmparser::MemArg| wasm_encoder::MemArg {
            align: memarg.align as u32,
            memory_index: memarg.memory,
            offset: memarg.offset as u64,
        };
        let res = match ix {
            Operator::F32Load { memarg } => writer.f32_load(fix_offset(memarg))?,
            Operator::F64Load { memarg } => writer.f64_load(fix_offset(memarg))?,
            Operator::I32Load { memarg } => writer.i32_load(fix_offset(memarg))?,
            Operator::I64Load { memarg } => writer.i64_load(fix_offset(memarg))?,
            Operator::I32Load8U { memarg } => writer.i32_load8_u(fix_offset(memarg))?,
            Operator::I32Load8S { memarg } => writer.i32_load8_s(fix_offset(memarg))?,
            Operator::I32Load16U { memarg } => writer.i32_load16_u(fix_offset(memarg))?,
            Operator::I32Load16S { memarg } => writer.i32_load16_s(fix_offset(memarg))?,
            Operator::I64Load8U { memarg } => writer.i64_load8_u(fix_offset(memarg))?,
            Operator::I64Load8S { memarg } => writer.i64_load8_s(fix_offset(memarg))?,
            Operator::I64Load16U { memarg } => writer.i64_load16_u(fix_offset(memarg))?,
            Operator::I64Load16S { memarg } => writer.i64_load16_s(fix_offset(memarg))?,
            Operator::I64Load32U { memarg } => writer.i64_load32_u(fix_offset(memarg))?,
            Operator::I64Load32S { memarg } => writer.i64_load32_s(fix_offset(memarg))?,

            Operator::I32Store { memarg } => writer.i32_store(fix_offset(memarg))?,
            Operator::I64Store { memarg } => writer.i64_store(fix_offset(memarg))?,
            Operator::I32Store8 { memarg } => writer.i32_store8(fix_offset(memarg))?,
            Operator::I32Store16 { memarg } => writer.i32_store16(fix_offset(memarg))?,
            Operator::I64Store8 { memarg } => writer.i64_store8(fix_offset(memarg))?,
            Operator::I64Store16 { memarg } => writer.i64_store16(fix_offset(memarg))?,
            Operator::I64Store32 { memarg } => writer.i64_store32(fix_offset(memarg))?,
            _ => {
                bail!("Unsupported relocation instruction: {instr:?}", instr = ix);
            }
        };
        Ok(res)
    }

    pub(crate) fn check_whitelisted_code_relocation(entry: &RelocationEntry) -> Result<()> {
        if matches!(entry.width, RelocationWidth::Bits64) {
            bail!("U64 memory pointers is currently not supported")
        }
        if !matches!(entry.relation, Relative::None) {
            bail!("Relocation memory pointers is currently not supported")
        }
        if matches!(
            entry.symbol_type,
            SymbolType::SectionOffset | SymbolType::FunctionOffset | SymbolType::MemoryAddrLocrel
        ) {
            bail!("Non supported symbol type for code relocation")
        }
        Ok(())
    }
}
