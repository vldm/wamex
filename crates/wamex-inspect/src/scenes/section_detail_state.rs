use std::collections::HashMap;

use semdump::{DataPart, SemanticDump};
use wamex_object::{
    linkage::reloc::Relative,
    typed::{EntityKind, LoadedFile, Module},
};

use crate::{
    hexdump::{RawBlockView, plain_semantic_dump},
    scene::{InspectTarget, Scene, SectionKind, ViewMode},
    scroll::{ListSelectionState, adjust_scroll, wrap_index},
    source::{
        RawSectionBlock, entity_name, format_size_len, function_type_label, global_type_label,
        memory_type_label, table_type_label,
    },
};

// ─── Display types ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Accent {
    Normal,
    Import,
    Export,
    Error,
    Muted,
    Warning,
}

#[derive(Clone, Debug)]
pub struct ListEntry {
    pub label: String,
    pub accent: Accent,
    pub action: Option<Scene>,
    pub entity: Option<EntityKind>,
    pub inspect_target: Option<InspectTarget>,
    pub detail_lines: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct RelocationLine {
    pub label: String,
    pub relation: Relative,
    pub action: Option<Scene>,
}

#[derive(Clone, Debug)]
pub struct DetailView {
    pub title: String,
    pub info_lines: Vec<String>,
    pub dump_title: Option<String>,
    pub dump: SemanticDump<'static>,
    pub dump_note: Option<String>,
    pub reloc_lines: Vec<RelocationLine>,
}

// ─── SectionDetailState ───────────────────────────────────────────────────────

#[derive(Default)]
pub struct SectionDetailState {
    pub(crate) selection: ListSelectionState,
    /// Index into the App's raw_sections slice recorded when entering this
    /// section from the Overview in Raw mode.  Preserved across reset() so
    /// that go_back / re-entry always shows the right block.
    pub(crate) entered_raw_section_idx: usize,
    /// The section kind that was last entered.  Used to detect same-section
    /// re-entry so scroll state is preserved on go_back + re-drill.
    pub(crate) last_kind: Option<SectionKind>,
}

impl SectionDetailState {
    pub fn selected(&self) -> usize {
        self.selection.selected
    }

    pub fn scroll(&self) -> usize {
        self.selection.scroll
    }

    pub fn set_viewport(&self, h: usize) {
        self.selection.viewport_hpos.set(h);
    }

    pub fn move_selection(&mut self, delta: isize, len: usize) {
        let sel = wrap_index(self.selection.selected, len, delta);
        self.selection.scroll = adjust_scroll(
            self.selection.scroll,
            sel,
            self.selection.viewport_hpos.get(),
        );
        self.selection.selected = sel;
    }

    /// Scroll the raw-hexdump view by `delta` lines, clamped to `[0, max_scroll]`.
    pub fn scroll_raw(&mut self, delta: isize, max_scroll: usize) {
        let new_scroll = (self.selection.scroll as isize + delta)
            .clamp(0, max_scroll as isize) as usize;
        self.selection.scroll = new_scroll;
        self.selection.selected = new_scroll;
    }

    pub fn reset(&mut self) {
        self.selection.selected = 0;
        self.selection.scroll = 0;
        // entered_raw_section_idx is intentionally kept across resets so that
        // the OverallView preview shows the correct block after go_back.
    }
}

// ─── Free functions (data queries) ───────────────────────────────────────────

pub fn selected_len(
    source_bytes: &[u8],
    raw_sections: &[RawSectionBlock],
    module: &Module<'_>,
    loaded: &LoadedFile<'_>,
    mode: ViewMode,
    kind: SectionKind,
) -> usize {
    match mode {
        ViewMode::Raw => raw_blocks(source_bytes, raw_sections, kind).len(),
        ViewMode::Structured => section_entries(module, loaded, kind).len(),
    }
}

pub fn raw_blocks(
    source_bytes: &[u8],
    raw_sections: &[RawSectionBlock],
    kind: SectionKind,
) -> Vec<RawBlockView> {
    raw_sections
        .iter()
        .filter(|block| kind.is_raw_eq(block.section_id))
        .map(|block| RawBlockView {
            title: raw_section_block_title(block),
            dump: plain_semantic_dump(&source_bytes[block.range.clone()], block.range.start),
        })
        .collect()
}

fn raw_section_block_title(block: &RawSectionBlock) -> String {
    let size = block.range.end.saturating_sub(block.range.start);
    format!(
        "[{:>2}] {:<32}  0x{:08x}..0x{:08x}  size: {}{}",
        block.section_id,
        block.name,
        block.range.start,
        block.range.end,
        format_size_len(size),
        block
            .count
            .map(|count| format!("  count: {count}"))
            .unwrap_or_default(),
    )
}

pub fn raw_preview(
    source_bytes: &[u8],
    raw_sections: &[RawSectionBlock],
    state: &SectionDetailState,
    kind: SectionKind,
) -> Option<RawBlockView> {
    raw_blocks(source_bytes, raw_sections, kind)
        .into_iter()
        .nth(state.selected())
}

pub fn section_entries(
    module: &Module<'_>,
    loaded: &LoadedFile<'_>,
    kind: SectionKind,
) -> Vec<ListEntry> {
    match kind {
        SectionKind::Types => type_entries(module),
        SectionKind::Imports => import_entries(module),
        SectionKind::Functions => function_entries(module, loaded),
        SectionKind::Tables => table_entries(module),
        SectionKind::Memories => memory_entries(module),
        SectionKind::Globals => global_entries(module),
        SectionKind::Exports => export_entries(module),
        SectionKind::Start => start_entries(module),
        SectionKind::Elements => element_entries(module),
        SectionKind::Data => data_entries(module),
        SectionKind::Tags => tag_entries(module),
    }
}

pub fn detail_view(
    module: &Module<'_>,
    loaded: &LoadedFile<'_>,
    state: &SectionDetailState,
    kind: SectionKind,
) -> Option<DetailView> {
    detail_view_at_index(module, loaded, kind, state.selected())
}

pub fn detail_view_at_index(
    module: &Module<'_>,
    loaded: &LoadedFile<'_>,
    kind: SectionKind,
    idx: usize,
) -> Option<DetailView> {
    let entry = section_entries(module, loaded, kind).into_iter().nth(idx)?;

    let dump = entry
        .inspect_target
        .and_then(|target| hexdump_rows(module, loaded, target).map(|(_, dump)| dump))
        .unwrap_or_default();
    let dump_title = entry.inspect_target.map(|target| match target {
        InspectTarget::Function(_) => "Function body".to_owned(),
        InspectTarget::Data(_) => "Data bytes".to_owned(),
    });
    let dump_note = match entry.inspect_target {
        Some(InspectTarget::Function(_)) => {
            Some("Disassembly later; showing body bytes for now.".to_owned())
        }
        Some(InspectTarget::Data(_)) => Some("Relocation bytes highlighted below.".to_owned()),
        None => None,
    };
    let reloc_lines = entry
        .entity
        .map(|entity| reloc_lines(module, loaded, entity))
        .unwrap_or_default();

    Some(DetailView {
        title: entry.label,
        info_lines: entry.detail_lines,
        dump_title,
        dump,
        dump_note,
        reloc_lines,
    })
}

pub fn reloc_lines(
    module: &Module<'_>,
    loaded: &LoadedFile<'_>,
    entity: EntityKind,
) -> Vec<RelocationLine> {
    let relocs = loaded
        .relocs
        .iter_relocs()
        .find_map(|(owner, relocs)| (owner == entity).then_some(relocs))
        .unwrap_or_default();

    relocs
        .iter()
        .map(|reloc| RelocationLine {
            label: format!(
                "@0x{offset:04x} {encoding:?}{width:?} {op:?}+{relation:?} -> {target} addend {addend:+#x}",
                offset = reloc.offset,
                encoding = reloc.encoding,
                width = reloc.width,
                op = reloc.symbol_op,
                relation = reloc.relation,
                target = entity_label(module, reloc.symbol_id),
                addend = reloc.addend,
            ),
            relation: reloc.relation,
            action: scene_for_entity(reloc.symbol_id),
        })
        .collect()
}

pub fn hexdump_rows(
    module: &Module<'_>,
    loaded: &LoadedFile<'_>,
    target: InspectTarget,
) -> Option<(String, SemanticDump<'static>)> {
    let entity = match target {
        InspectTarget::Function(func_ref) => EntityKind::Function(func_ref),
        InspectTarget::Data(data_ref) => EntityKind::DataSymbol(data_ref),
    };

    let (bytes, reloc_base) = entity_bytes_and_base(module, target)?;
    let relocs = loaded
        .relocs
        .iter_relocs()
        .find_map(|(owner, relocs)| (owner == entity).then_some(relocs))
        .unwrap_or_default();

    let bytes_len = bytes.len();
    let mut part = DataPart::from_bytes(bytes);
    for reloc in relocs {
        // Reloc offsets are section-relative; adjust to entity-local by subtracting the
        // body's start offset within the section (same as SymbolDebug::shift_left).
        let Some(local_offset) = (reloc.offset as usize).checked_sub(reloc_base) else {
            continue;
        };
        let range = local_offset..(local_offset + reloc.extent());
        if range.end <= bytes_len {
            part.push_ref(range, entity_label(module, reloc.symbol_id));
        }
    }

    let mut dump = SemanticDump::new(0);
    dump.push_part(part);

    Some((entity_label(module, entity), dump))
}

pub fn entity_label(module: &Module<'_>, entity: EntityKind) -> String {
    match entity {
        EntityKind::Function(func_ref) => {
            let entity = module.functions.try_get_entity(func_ref);
            let name = entity
                .as_ref()
                .and_then(|entry| entry.name())
                .map_or("<anon>", |name| name.as_ref());
            format!("func {} {}", func_ref.as_u32(), name)
        }
        EntityKind::DataSymbol(data_ref) => {
            if let Some(import) = module.extra.mem_layout.external().get(data_ref) {
                return format!(
                    "data {} {}::{}",
                    data_ref.as_u32(),
                    import.module,
                    import.name
                );
            }
            if let Some(place) = module.extra.mem_layout.item_places().get(data_ref) {
                let segment = &module.extra.mem_layout.segments()[place.segment_id];
                let item = &segment.parts[place.part_id];
                return format!(
                    "data {} {}",
                    data_ref.as_u32(),
                    entity_name(item.defined_entity.name.as_ref())
                );
            }
            format!("data {} <unknown>", data_ref.as_u32())
        }
        EntityKind::Global(global_ref) => {
            let entity = module.globals.try_get_entity(global_ref);
            format!(
                "global {} {}",
                global_ref.as_u32(),
                entity
                    .as_ref()
                    .and_then(|entry| entry.name())
                    .map_or("<anon>", |name| name.as_ref())
            )
        }
        EntityKind::Table(table_ref) => {
            let entity = module.tables.try_get_entity(table_ref);
            format!(
                "table {} {}",
                table_ref.as_u32(),
                entity
                    .as_ref()
                    .and_then(|entry| entry.name())
                    .map_or("<anon>", |name| name.as_ref())
            )
        }
        EntityKind::Memory(memory_ref) => {
            let entity = module.memories.try_get_entity(memory_ref);
            format!(
                "memory {} {}",
                memory_ref.as_u32(),
                entity
                    .as_ref()
                    .and_then(|entry| entry.name())
                    .map_or("<anon>", |name| name.as_ref())
            )
        }
        EntityKind::Tag(tag_ref) => {
            let entity = module.tags.try_get_entity(tag_ref);
            format!(
                "tag {} {}",
                tag_ref.as_u32(),
                entity
                    .as_ref()
                    .and_then(|entry| entry.name())
                    .map_or("<anon>", |name| name.as_ref())
            )
        }
        EntityKind::Type(type_ref) => format!("type {}", type_ref.as_u32()),
    }
}

pub fn scene_for_entity(entity: EntityKind) -> Option<Scene> {
    match entity {
        EntityKind::Function(_) => Some(Scene::SectionDetail(SectionKind::Functions)),
        EntityKind::DataSymbol(_) => Some(Scene::SectionDetail(SectionKind::Data)),
        EntityKind::Global(_) => Some(Scene::SectionDetail(SectionKind::Globals)),
        EntityKind::Table(_) => Some(Scene::SectionDetail(SectionKind::Tables)),
        EntityKind::Memory(_) => Some(Scene::SectionDetail(SectionKind::Memories)),
        EntityKind::Tag(_) => Some(Scene::SectionDetail(SectionKind::Tags)),
        EntityKind::Type(_) => Some(Scene::SectionDetail(SectionKind::Types)),
    }
}

pub fn inspect_target_for_entity(module: &Module<'_>, entity: EntityKind) -> Option<InspectTarget> {
    match entity {
        EntityKind::Function(func_ref) => {
            module
                .functions
                .try_get_entity(func_ref)
                .and_then(|entity| {
                    entity
                        .to_defined()
                        .map(|_| InspectTarget::Function(func_ref))
                })
        }
        EntityKind::DataSymbol(data_ref) => module
            .extra
            .mem_layout
            .item_places()
            .get(data_ref)
            .map(|_| InspectTarget::Data(data_ref)),
        _ => None,
    }
}

fn entity_bytes_and_base(module: &Module<'_>, target: InspectTarget) -> Option<(Vec<u8>, usize)> {
    match target {
        InspectTarget::Function(func_ref) => module
            .functions
            .try_get_entity(func_ref)
            .and_then(|entity| entity.to_defined())
            .map(|defined| {
                let base = defined.body.original_range().start;
                (defined.body.iter_bytes().collect(), base)
            }),
        InspectTarget::Data(data_ref) => {
            let place = module.extra.mem_layout.item_places().get(data_ref)?;
            let segment = &module.extra.mem_layout.segments()[place.segment_id];
            let item = &segment.parts[place.part_id];
            let base = item.defined_entity.body.original_range().start;
            Some((item.defined_entity.body.iter_bytes().collect(), base))
        }
    }
}

fn reloc_count_map(loaded: &LoadedFile<'_>) -> HashMap<EntityKind, usize> {
    loaded
        .relocs
        .iter_relocs()
        .map(|(entity, relocs)| (entity, relocs.len()))
        .collect()
}

// ─── Per-section-kind entry builders ─────────────────────────────────────────

fn type_entries(module: &Module<'_>) -> Vec<ListEntry> {
    module
        .extra_types
        .iter()
        .map(|(type_ref, func_type)| ListEntry {
            label: format!(
                "[type {}] {}",
                type_ref.as_u32(),
                function_type_label(func_type)
            ),
            accent: Accent::Muted,
            action: None,
            entity: Some(EntityKind::Type(type_ref)),
            inspect_target: None,
            detail_lines: vec![
                format!("canonical type index: {}", type_ref.as_u32()),
                format!("signature: {}", function_type_label(func_type)),
            ],
        })
        .collect()
}

fn import_entries(module: &Module<'_>) -> Vec<ListEntry> {
    let mut lines = Vec::new();

    for (func_ref, import) in module.functions.imports_iter() {
        lines.push(ListEntry {
            label: format!(
                "[func {}] {}::{} {}",
                func_ref.as_u32(),
                import.module,
                import.name,
                function_type_label(&import.entity_type)
            ),
            accent: Accent::Import,
            action: None,
            entity: Some(EntityKind::Function(func_ref)),
            inspect_target: None,
            detail_lines: vec![
                format!("kind: imported function"),
                format!("module: {}", import.module),
                format!("name: {}", import.name),
                format!("type: {}", function_type_label(&import.entity_type)),
            ],
        });
    }
    for (table_ref, import) in module.tables.imports_iter() {
        lines.push(ListEntry {
            label: format!(
                "[table {}] {}::{} {:?}",
                table_ref.as_u32(),
                import.module,
                import.name,
                import.entity_type
            ),
            accent: Accent::Import,
            action: None,
            entity: Some(EntityKind::Table(table_ref)),
            inspect_target: None,
            detail_lines: vec![
                "kind: imported table".to_owned(),
                format!("module: {}", import.module),
                format!("name: {}", import.name),
                format!("type: {:?}", import.entity_type),
            ],
        });
    }
    for (memory_ref, import) in module.memories.imports_iter() {
        lines.push(ListEntry {
            label: format!(
                "[memory {}] {}::{} {:?}",
                memory_ref.as_u32(),
                import.module,
                import.name,
                import.entity_type
            ),
            accent: Accent::Import,
            action: None,
            entity: Some(EntityKind::Memory(memory_ref)),
            inspect_target: None,
            detail_lines: vec![
                "kind: imported memory".to_owned(),
                format!("module: {}", import.module),
                format!("name: {}", import.name),
                format!("type: {:?}", import.entity_type),
            ],
        });
    }
    for (global_ref, import) in module.globals.imports_iter() {
        lines.push(ListEntry {
            label: format!(
                "[global {}] {}::{} {:?}",
                global_ref.as_u32(),
                import.module,
                import.name,
                import.entity_type
            ),
            accent: Accent::Import,
            action: None,
            entity: Some(EntityKind::Global(global_ref)),
            inspect_target: None,
            detail_lines: vec![
                "kind: imported global".to_owned(),
                format!("module: {}", import.module),
                format!("name: {}", import.name),
                format!("type: {:?}", import.entity_type),
            ],
        });
    }
    for (tag_ref, import) in module.tags.imports_iter() {
        lines.push(ListEntry {
            label: format!(
                "[tag {}] {}::{} {:?}",
                tag_ref.as_u32(),
                import.module,
                import.name,
                import.entity_type
            ),
            accent: Accent::Import,
            action: None,
            entity: Some(EntityKind::Tag(tag_ref)),
            inspect_target: None,
            detail_lines: vec![
                "kind: imported tag".to_owned(),
                format!("module: {}", import.module),
                format!("name: {}", import.name),
                format!("type: {:?}", import.entity_type),
            ],
        });
    }
    for (data_ref, import) in module.extra.mem_layout.external().iter() {
        lines.push(ListEntry {
            label: format!(
                "[data {}] {}::{} external data",
                data_ref.as_u32(),
                import.module,
                import.name
            ),
            accent: Accent::Import,
            action: None,
            entity: Some(EntityKind::DataSymbol(data_ref)),
            inspect_target: None,
            detail_lines: vec![
                "kind: imported data".to_owned(),
                format!("module: {}", import.module),
                format!("name: {}", import.name),
            ],
        });
    }

    lines
}

fn export_entries(module: &Module<'_>) -> Vec<ListEntry> {
    let mut lines = Vec::new();

    for (func_ref, name) in module.functions.exports_iter() {
        lines.push(ListEntry {
            label: format!("[func {}] {}", func_ref.as_u32(), name),
            accent: Accent::Export,
            action: scene_for_entity(EntityKind::Function(func_ref)),
            entity: Some(EntityKind::Function(func_ref)),
            inspect_target: inspect_target_for_entity(module, EntityKind::Function(func_ref)),
            detail_lines: vec![
                format!("export name: {}", name),
                format!(
                    "target: {}",
                    entity_label(module, EntityKind::Function(func_ref))
                ),
            ],
        });
    }
    for (table_ref, name) in module.tables.exports_iter() {
        lines.push(ListEntry {
            label: format!("[table {}] {}", table_ref.as_u32(), name),
            accent: Accent::Export,
            action: scene_for_entity(EntityKind::Table(table_ref)),
            entity: Some(EntityKind::Table(table_ref)),
            inspect_target: None,
            detail_lines: vec![
                format!("export name: {}", name),
                format!(
                    "target: {}",
                    entity_label(module, EntityKind::Table(table_ref))
                ),
            ],
        });
    }
    for (memory_ref, name) in module.memories.exports_iter() {
        lines.push(ListEntry {
            label: format!("[memory {}] {}", memory_ref.as_u32(), name),
            accent: Accent::Export,
            action: scene_for_entity(EntityKind::Memory(memory_ref)),
            entity: Some(EntityKind::Memory(memory_ref)),
            inspect_target: None,
            detail_lines: vec![
                format!("export name: {}", name),
                format!(
                    "target: {}",
                    entity_label(module, EntityKind::Memory(memory_ref))
                ),
            ],
        });
    }
    for (global_ref, name) in module.globals.exports_iter() {
        lines.push(ListEntry {
            label: format!("[global {}] {}", global_ref.as_u32(), name),
            accent: Accent::Export,
            action: scene_for_entity(EntityKind::Global(global_ref)),
            entity: Some(EntityKind::Global(global_ref)),
            inspect_target: None,
            detail_lines: vec![
                format!("export name: {}", name),
                format!(
                    "target: {}",
                    entity_label(module, EntityKind::Global(global_ref))
                ),
            ],
        });
    }
    for (tag_ref, name) in module.tags.exports_iter() {
        lines.push(ListEntry {
            label: format!("[tag {}] {}", tag_ref.as_u32(), name),
            accent: Accent::Export,
            action: scene_for_entity(EntityKind::Tag(tag_ref)),
            entity: Some(EntityKind::Tag(tag_ref)),
            inspect_target: None,
            detail_lines: vec![
                format!("export name: {}", name),
                format!("target: {}", entity_label(module, EntityKind::Tag(tag_ref))),
            ],
        });
    }

    lines
}

fn function_entries(module: &Module<'_>, loaded: &LoadedFile<'_>) -> Vec<ListEntry> {
    let relocs = reloc_count_map(loaded);

    module
        .functions
        .iter()
        .map(|(func_ref, entity)| {
            let is_defined = entity.to_defined().is_some();
            let exports_count = entity.export_as().names.len();
            let reloc_count = relocs
                .get(&EntityKind::Function(func_ref))
                .copied()
                .unwrap_or_default();

            let exports = entity.export_as().names.join(", ");

            ListEntry {
                label: format!(
                    "[func {}] {}  {}  relocs={} exports={}",
                    func_ref.as_u32(),
                    entity_name(entity.name()),
                    function_type_label(entity.get_type()),
                    reloc_count,
                    exports_count,
                ),
                accent: if is_defined {
                    Accent::Normal
                } else {
                    Accent::Import
                },
                action: None,
                entity: Some(EntityKind::Function(func_ref)),
                inspect_target: is_defined.then_some(InspectTarget::Function(func_ref)),
                detail_lines: vec![
                    format!("kind: {}", if is_defined { "defined" } else { "imported" }),
                    format!("name: {}", entity_name(entity.name())),
                    format!("type: {}", function_type_label(entity.get_type())),
                    format!("exports: {}", exports_count),
                    format!("relocations: {}", reloc_count),
                    format!(
                        "body bytes: {}",
                        entity.to_defined().map_or(0, |defined| defined.body.len())
                    ),
                ],
            }
        })
        .collect()
}

fn table_entries(module: &Module<'_>) -> Vec<ListEntry> {
    module
        .tables
        .iter()
        .map(|(table_ref, entity)| ListEntry {
            label: format!(
                "[table {}] {}  {}",
                table_ref,
                entity_name(entity.name()),
                table_type_label(entity.get_type()),
            ),
            accent: if entity.to_defined().is_some() {
                Accent::Normal
            } else {
                Accent::Import
            },
            action: None,
            entity: Some(EntityKind::Table(table_ref)),
            inspect_target: None,
            detail_lines: vec![
                format!("name: {}", entity_name(entity.name())),
                format!("type: {}", table_type_label(entity.get_type())),
                format!(
                    "kind: {}",
                    if entity.to_defined().is_some() {
                        "defined"
                    } else {
                        "imported"
                    }
                ),
            ],
        })
        .collect()
}

fn memory_entries(module: &Module<'_>) -> Vec<ListEntry> {
    module
        .memories
        .iter()
        .map(|(memory_ref, entity)| ListEntry {
            label: format!(
                "[memory {}] {}  {}",
                memory_ref.as_u32(),
                entity_name(entity.name()),
                memory_type_label(entity.get_type()),
            ),
            accent: if entity.to_defined().is_some() {
                Accent::Normal
            } else {
                Accent::Import
            },
            action: None,
            entity: Some(EntityKind::Memory(memory_ref)),
            inspect_target: None,
            detail_lines: vec![
                format!("name: {}", entity_name(entity.name())),
                format!("type: {}", memory_type_label(entity.get_type())),
                format!(
                    "kind: {}",
                    if entity.to_defined().is_some() {
                        "defined"
                    } else {
                        "imported"
                    }
                ),
            ],
        })
        .collect()
}

fn global_entries(module: &Module<'_>) -> Vec<ListEntry> {
    module
        .globals
        .iter()
        .map(|(global_ref, entity)| ListEntry {
            label: format!(
                "[global {}] {} {}",
                global_ref.as_u32(),
                entity_name(entity.name()),
                global_type_label(*entity.get_type()),
            ),
            accent: if entity.to_defined().is_some() {
                Accent::Normal
            } else {
                Accent::Import
            },
            action: None,
            entity: Some(EntityKind::Global(global_ref)),
            inspect_target: None,
            detail_lines: vec![
                format!("name: {}", entity_name(entity.name())),
                format!("type: {}", global_type_label(*entity.get_type())),
                format!(
                    "kind: {}",
                    if entity.to_defined().is_some() {
                        "defined"
                    } else {
                        "imported"
                    }
                ),
            ],
        })
        .collect()
}

fn tag_entries(module: &Module<'_>) -> Vec<ListEntry> {
    module
        .tags
        .iter()
        .map(|(tag_ref, entity)| ListEntry {
            label: format!(
                "[tag {}] {}  {:?}",
                tag_ref.as_u32(),
                entity_name(entity.name()),
                entity.get_type(),
            ),
            accent: if entity.to_defined().is_some() {
                Accent::Normal
            } else {
                Accent::Import
            },
            action: None,
            entity: Some(EntityKind::Tag(tag_ref)),
            inspect_target: None,
            detail_lines: vec![
                format!("name: {}", entity_name(entity.name())),
                format!("type: {:?}", entity.get_type()),
                format!(
                    "kind: {}",
                    if entity.to_defined().is_some() {
                        "defined"
                    } else {
                        "imported"
                    }
                ),
            ],
        })
        .collect()
}

fn element_entries(module: &Module<'_>) -> Vec<ListEntry> {
    module
        .extra
        .function_elements
        .segments
        .iter()
        .map(|(segment_id, segment)| ListEntry {
            label: format!(
                "[elem {}] kind={:?} name={} items={}",
                segment_id.as_u32(),
                segment.kind,
                segment.name,
                segment.parts.len(),
            ),
            accent: Accent::Muted,
            action: None,
            entity: None,
            inspect_target: None,
            detail_lines: vec![
                format!("segment: {}", segment_id.as_u32()),
                format!("name: {}", segment.name),
                format!("kind: {:?}", segment.kind),
                format!("items: {}", segment.parts.len()),
            ],
        })
        .collect()
}

fn data_entries(module: &Module<'_>) -> Vec<ListEntry> {
    let mut entries = Vec::new();

    for (data_ref, import) in module.extra.mem_layout.external().iter() {
        entries.push(ListEntry {
            label: format!(
                "[data {}] import {}::{}",
                data_ref.as_u32(),
                import.module,
                import.name
            ),
            accent: Accent::Import,
            action: None,
            entity: Some(EntityKind::DataSymbol(data_ref)),
            inspect_target: None,
            detail_lines: vec![
                "kind: imported data".to_owned(),
                format!("module: {}", import.module),
                format!("name: {}", import.name),
            ],
        });
    }

    for (data_ref, place) in module.extra.mem_layout.item_places().iter() {
        let segment = &module.extra.mem_layout.segments()[place.segment_id];
        let item = &segment.parts[place.part_id];
        let size = item.defined_entity.body.len();
        entries.push(ListEntry {
            label: format!(
                "[data {}] {}  seg={} off={} size={} va={:?}",
                data_ref.as_u32(),
                entity_name(item.defined_entity.name.as_ref()),
                place.segment_id.as_u32(),
                place.offsets.section_offset,
                size,
                segment.va_address,
            ),
            accent: Accent::Normal,
            action: None,
            entity: Some(EntityKind::DataSymbol(data_ref)),
            inspect_target: Some(InspectTarget::Data(data_ref)),
            detail_lines: vec![
                format!("name: {}", entity_name(item.defined_entity.name.as_ref())),
                format!("segment: {}", place.segment_id.as_u32()),
                format!("section offset: {}", place.offsets.section_offset),
                format!("virtual address: {}", place.offsets.va_address),
                format!("size: {}", size),
            ],
        });
    }

    entries
}

fn start_entries(module: &Module<'_>) -> Vec<ListEntry> {
    let Some(func_ref) = module.extra.start_function else {
        return vec![ListEntry {
            label: "No start section".to_owned(),
            accent: Accent::Muted,
            action: None,
            entity: None,
            inspect_target: None,
            detail_lines: vec!["Module has no start section.".to_owned()],
        }];
    };

    vec![ListEntry {
        label: format!(
            "start -> {}",
            entity_label(module, EntityKind::Function(func_ref))
        ),
        accent: Accent::Warning,
        action: Some(Scene::SectionDetail(SectionKind::Functions)),
        entity: Some(EntityKind::Function(func_ref)),
        inspect_target: inspect_target_for_entity(module, EntityKind::Function(func_ref)),
        detail_lines: vec![
            format!(
                "start function: {}",
                entity_label(module, EntityKind::Function(func_ref))
            ),
            "Enter to jump to Functions section.".to_owned(),
        ],
    }]
}
