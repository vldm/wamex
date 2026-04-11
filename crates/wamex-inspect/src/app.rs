use std::{
    cell::Cell,
    collections::HashMap,
    ops::Range,
    path::{Path, PathBuf},
};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use wamex_object::{
    index::SectionId,
    linkage::reloc::Relative,
    raw::ObjectReader,
    typed::{EntityKind, FileId, FileLoader, LoadedFile, Module},
};

use crate::scene::{InspectTarget, Scene, SectionDetailMode, SectionKind};

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
pub struct HexdumpRow {
    pub offset: usize,
    pub bytes: Vec<(u8, Option<Relative>)>,
}

#[derive(Clone, Debug)]
pub struct RawBlockView {
    pub title: String,
    pub rows: Vec<HexdumpRow>,
}

#[derive(Clone, Debug)]
pub struct DetailView {
    pub title: String,
    pub info_lines: Vec<String>,
    pub dump_title: Option<String>,
    pub dump_rows: Vec<HexdumpRow>,
    pub dump_note: Option<String>,
    pub reloc_lines: Vec<RelocationLine>,
}

#[derive(Clone, Debug)]
pub struct RawSectionBlock {
    section_index: usize,
    section_id: SectionId,
    name: String,
    range: Range<usize>,
    count: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct SectionSummary {
    pub kind: SectionKind,
    pub title: String,
    pub count: usize,
    pub note: String,
}

#[derive(Clone, Debug)]
pub struct RawSummary {
    pub file_size: usize,
    pub target_features: String,
    pub validation_error: Option<String>,
    pub structural_rows: Vec<SectionSummary>,
}

struct SourceFile {
    path: PathBuf,
    bytes: Box<[u8]>,
    raw_sections: Vec<RawSectionBlock>,
    loader: FileLoader,
    file_id: FileId,
    summary: RawSummary,
}

pub struct App {
    source: SourceFile,
    current_scene: Scene,
    scene_stack: Vec<Scene>,
    section_mode: SectionDetailMode,
    show_help: bool,
    should_quit: bool,

    overall_selected: usize,
    overall_scroll: usize,
    overall_viewport: Cell<usize>,

    section_selected: usize,
    section_scroll: usize,
    section_viewport: Cell<usize>,

    detail_scroll: usize,
}

impl App {
    pub fn load(path: PathBuf) -> anyhow::Result<Self> {
        let bytes = std::fs::read(&path)?;
        let file_size = bytes.len();
        let mut loader = FileLoader::new();
        let file_id = loader.load_from_bytes(bytes.clone().into_boxed_slice())?;

        let validation_error = validate_wasm(&bytes);

        let loaded = loader.get_file(file_id);
        let raw_sections = collect_raw_sections(loaded.raw_reader());
        let summary = RawSummary::from_parts(loaded, validation_error, file_size);

        Ok(Self {
            source: SourceFile {
                path,
                bytes: bytes.into_boxed_slice(),
                raw_sections,
                loader,
                file_id,
                summary,
            },
            current_scene: Scene::OverallView,
            scene_stack: Vec::new(),
            section_mode: SectionDetailMode::Structured,
            show_help: false,
            should_quit: false,
            overall_selected: 0,
            overall_scroll: 0,
            overall_viewport: Cell::new(0),
            section_selected: 0,
            section_scroll: 0,
            section_viewport: Cell::new(0),
            detail_scroll: 0,
        })
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }

        if self.show_help {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') => self.show_help = false,
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char(c) if ('1'..='2').contains(&c) => {
                let idx = (c as u8 - b'1') as usize;
                self.replace_scene(self.top_level_scene(idx));
            }
            KeyCode::Left | KeyCode::Char('h') => self.move_scene(-1),
            KeyCode::Right | KeyCode::Char('l') => self.move_scene(1),
            KeyCode::Tab => self.cycle_section_mode(),
            KeyCode::Esc | KeyCode::Backspace => self.go_back(),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::PageUp => self.page_selection(-8),
            KeyCode::PageDown => self.page_selection(8),
            KeyCode::Enter => self.drill_in(),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            _ => {}
        }
    }

    pub fn current_scene(&self) -> &Scene {
        &self.current_scene
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn show_help(&self) -> bool {
        self.show_help
    }

    pub fn section_mode(&self) -> SectionDetailMode {
        self.section_mode
    }

    pub fn path(&self) -> &Path {
        &self.source.path
    }

    pub fn summary(&self) -> &RawSummary {
        &self.source.summary
    }

    pub fn loaded(&self) -> &LoadedFile<'_> {
        self.source.loader.get_file(self.source.file_id)
    }

    pub fn module(&self) -> &Module<'_> {
        &self.loaded().module
    }

    pub fn overall_selected(&self) -> usize {
        self.overall_selected
    }

    pub fn overall_scroll(&self) -> usize {
        self.overall_scroll
    }

    /// Called by the renderer to record the visible height of the overview list.
    pub fn set_overall_viewport(&self, h: usize) {
        self.overall_viewport.set(h);
    }

    pub fn section_selected(&self) -> usize {
        self.section_selected
    }

    pub fn section_scroll(&self) -> usize {
        self.section_scroll
    }

    /// Called by the renderer to record the visible height of the section list.
    pub fn set_section_viewport(&self, h: usize) {
        self.section_viewport.set(h);
    }

    pub fn detail_scroll(&self) -> usize {
        self.detail_scroll
    }

    pub fn default_section_kind(&self) -> SectionKind {
        self.selected_overall_section_kind()
            .or_else(|| {
                self.source
                    .raw_sections
                    .iter()
                    .find_map(|block| SectionKind::from_section_id(block.section_id))
            })
            .unwrap_or(SectionKind::Types)
    }

    pub fn current_section_kind(&self) -> SectionKind {
        match self.current_scene {
            Scene::SectionDetail(kind) => kind,
            Scene::OverallView => self.default_section_kind(),
        }
    }

    pub fn structured_section_summary(&self, kind: SectionKind) -> Option<&SectionSummary> {
        self.summary()
            .structural_rows
            .iter()
            .find(|row| row.kind == kind)
    }

    pub fn structured_preview_entries(&self, kind: SectionKind) -> Vec<ListEntry> {
        self.section_entries(kind)
    }

    pub fn status_notice(&self) -> Option<String> {
        let Scene::SectionDetail(kind) = self.current_scene else {
            return None;
        };

        if self.section_mode != SectionDetailMode::Structured {
            return None;
        }

        self.section_notice(kind).map(str::to_owned)
    }

    pub fn section_notice(&self, kind: SectionKind) -> Option<&'static str> {
        if self.is_partial_section(kind) {
            Some("imports/exports here are structural only; raw sections may contain more detail")
        } else {
            None
        }
    }

    pub fn is_partial_section(&self, kind: SectionKind) -> bool {
        matches!(kind, SectionKind::Imports | SectionKind::Exports)
    }

    pub fn raw_section_title(&self, block: &RawSectionBlock) -> String {
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

    pub(crate) fn raw_sections(&self) -> &[RawSectionBlock] {
        &self.source.raw_sections
    }

    pub fn raw_preview(&self, kind: SectionKind) -> Option<RawBlockView> {
        self.raw_blocks(kind).into_iter().nth(self.section_selected)
    }

    pub fn section_label(&self, kind: SectionKind) -> String {
        format!("[{}] {}", kind.canonical_label(), kind.title())
    }

    pub fn section_selected_len(&self, kind: SectionKind) -> usize {
        match self.section_mode {
            SectionDetailMode::Raw => self.raw_blocks(kind).len(),
            SectionDetailMode::Structured => self.section_entries(kind).len(),
        }
    }

    pub fn section_entries(&self, kind: SectionKind) -> Vec<ListEntry> {
        match kind {
            SectionKind::Types => self.type_entries(),
            SectionKind::Imports => self.import_entries(),
            SectionKind::Functions => self.function_entries(),
            SectionKind::Tables => self.table_entries(),
            SectionKind::Memories => self.memory_entries(),
            SectionKind::Globals => self.global_entries(),
            SectionKind::Exports => self.export_entries(),
            SectionKind::Start => self.start_entries(),
            SectionKind::Elements => self.element_entries(),
            SectionKind::Data => self.data_entries(),
            SectionKind::Tags => self.tag_entries(),
        }
    }

    pub fn raw_blocks(&self, kind: SectionKind) -> Vec<RawBlockView> {
        self.source
            .raw_sections
            .iter()
            .filter(|block| kind.is_raw_eq(block.section_id))
            .map(|block| RawBlockView {
                title: self.raw_section_title(block),
                rows: plain_hexdump_rows(
                    &self.source.bytes[block.range.clone()],
                    block.range.start,
                ),
            })
            .collect()
    }

    pub fn detail_view(&self, kind: SectionKind) -> Option<DetailView> {
        let entry = self.selected_entry(kind)?;
        let dump_rows = entry
            .inspect_target
            .and_then(|target| self.hexdump_rows(target).map(|(_, rows)| rows))
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
            .map(|entity| self.reloc_lines(entity))
            .unwrap_or_default();

        Some(DetailView {
            title: entry.label,
            info_lines: entry.detail_lines,
            dump_title,
            dump_rows,
            dump_note,
            reloc_lines,
        })
    }

    pub fn reloc_lines(&self, entity: EntityKind) -> Vec<RelocationLine> {
        let relocs = self
            .loaded()
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
                    target = self.entity_label(reloc.symbol_id),
                    addend = reloc.addend,
                ),
                relation: reloc.relation,
                action: self.scene_for_entity(reloc.symbol_id),
            })
            .collect()
    }

    pub fn hexdump_rows(&self, target: InspectTarget) -> Option<(String, Vec<HexdumpRow>)> {
        let entity = match target {
            InspectTarget::Function(func_ref) => EntityKind::Function(func_ref),
            InspectTarget::Data(data_ref) => EntityKind::DataSymbol(data_ref),
        };

        let bytes = self.entity_bytes(target)?;
        let relocs = self
            .loaded()
            .relocs
            .iter_relocs()
            .find_map(|(owner, relocs)| (owner == entity).then_some(relocs))
            .unwrap_or_default();

        let rows = bytes
            .chunks(16)
            .enumerate()
            .map(|(row_idx, chunk)| {
                let start = row_idx * 16;
                let bytes = chunk
                    .iter()
                    .enumerate()
                    .map(|(offset, byte)| {
                        let absolute = start + offset;
                        let relation = relocs
                            .iter()
                            .find(|reloc| reloc.relocation_range().contains(&absolute))
                            .map(|reloc| reloc.relation);
                        (*byte, relation)
                    })
                    .collect();

                HexdumpRow {
                    offset: start,
                    bytes,
                }
            })
            .collect();

        Some((self.entity_label(entity), rows))
    }

    fn selected_entry(&self, kind: SectionKind) -> Option<ListEntry> {
        self.section_entries(kind)
            .into_iter()
            .nth(self.section_selected)
    }

    fn selected_overall_section_kind(&self) -> Option<SectionKind> {
        self.source
            .raw_sections
            .get(self.overall_selected)
            .and_then(|block| SectionKind::from_section_id(block.section_id))
    }

    fn type_entries(&self) -> Vec<ListEntry> {
        self.module()
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

    fn import_entries(&self) -> Vec<ListEntry> {
        let mut lines = Vec::new();

        for (func_ref, import) in self.module().functions.imports_iter() {
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
        for (table_ref, import) in self.module().tables.imports_iter() {
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
        for (memory_ref, import) in self.module().memories.imports_iter() {
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
        for (global_ref, import) in self.module().globals.imports_iter() {
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
        for (tag_ref, import) in self.module().tags.imports_iter() {
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
        for (data_ref, import) in self.module().extra.mem_layout.external().iter() {
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

    fn export_entries(&self) -> Vec<ListEntry> {
        let mut lines = Vec::new();

        for (func_ref, name) in self.module().functions.exports_iter() {
            lines.push(ListEntry {
                label: format!("[func {}] {}", func_ref.as_u32(), name),
                accent: Accent::Export,
                action: self.scene_for_entity(EntityKind::Function(func_ref)),
                entity: Some(EntityKind::Function(func_ref)),
                inspect_target: self.inspect_target_for_entity(EntityKind::Function(func_ref)),
                detail_lines: vec![
                    format!("export name: {}", name),
                    format!(
                        "target: {}",
                        self.entity_label(EntityKind::Function(func_ref))
                    ),
                ],
            });
        }
        for (table_ref, name) in self.module().tables.exports_iter() {
            lines.push(ListEntry {
                label: format!("[table {}] {}", table_ref.as_u32(), name),
                accent: Accent::Export,
                action: self.scene_for_entity(EntityKind::Table(table_ref)),
                entity: Some(EntityKind::Table(table_ref)),
                inspect_target: None,
                detail_lines: vec![
                    format!("export name: {}", name),
                    format!(
                        "target: {}",
                        self.entity_label(EntityKind::Table(table_ref))
                    ),
                ],
            });
        }
        for (memory_ref, name) in self.module().memories.exports_iter() {
            lines.push(ListEntry {
                label: format!("[memory {}] {}", memory_ref.as_u32(), name),
                accent: Accent::Export,
                action: self.scene_for_entity(EntityKind::Memory(memory_ref)),
                entity: Some(EntityKind::Memory(memory_ref)),
                inspect_target: None,
                detail_lines: vec![
                    format!("export name: {}", name),
                    format!(
                        "target: {}",
                        self.entity_label(EntityKind::Memory(memory_ref))
                    ),
                ],
            });
        }
        for (global_ref, name) in self.module().globals.exports_iter() {
            lines.push(ListEntry {
                label: format!("[global {}] {}", global_ref.as_u32(), name),
                accent: Accent::Export,
                action: self.scene_for_entity(EntityKind::Global(global_ref)),
                entity: Some(EntityKind::Global(global_ref)),
                inspect_target: None,
                detail_lines: vec![
                    format!("export name: {}", name),
                    format!(
                        "target: {}",
                        self.entity_label(EntityKind::Global(global_ref))
                    ),
                ],
            });
        }
        for (tag_ref, name) in self.module().tags.exports_iter() {
            lines.push(ListEntry {
                label: format!("[tag {}] {}", tag_ref.as_u32(), name),
                accent: Accent::Export,
                action: self.scene_for_entity(EntityKind::Tag(tag_ref)),
                entity: Some(EntityKind::Tag(tag_ref)),
                inspect_target: None,
                detail_lines: vec![
                    format!("export name: {}", name),
                    format!("target: {}", self.entity_label(EntityKind::Tag(tag_ref))),
                ],
            });
        }

        lines
    }

    fn function_entries(&self) -> Vec<ListEntry> {
        let relocs = self.reloc_count_map();

        self.module()
            .functions
            .iter()
            .map(|(func_ref, entity)| {
                let is_defined = entity.to_defined().is_some();
                let exports = entity.export_as().names.len();
                let reloc_count = relocs
                    .get(&EntityKind::Function(func_ref))
                    .copied()
                    .unwrap_or_default();

                ListEntry {
                    label: format!(
                        "[func {}] {}  {}  relocs={} exports={}",
                        func_ref.as_u32(),
                        entity_name(entity.name()),
                        function_type_label(entity.get_type()),
                        reloc_count,
                        exports,
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
                        format!("exports: {}", exports),
                        format!("relocations: {}", reloc_count),
                        format!(
                            "body bytes: {}",
                            entity
                                .to_defined()
                                .map(|defined| defined.body.len())
                                .unwrap_or(0)
                        ),
                    ],
                }
            })
            .collect()
    }

    fn table_entries(&self) -> Vec<ListEntry> {
        self.module()
            .tables
            .iter()
            .map(|(table_ref, entity)| ListEntry {
                label: format!(
                    "[table {}] {}  {:?}",
                    table_ref.as_u32(),
                    entity_name(entity.name()),
                    entity.get_type(),
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

    fn element_entries(&self) -> Vec<ListEntry> {
        self.module()
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

    fn tag_entries(&self) -> Vec<ListEntry> {
        self.module()
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

    fn global_entries(&self) -> Vec<ListEntry> {
        self.module()
            .globals
            .iter()
            .map(|(global_ref, entity)| ListEntry {
                label: format!(
                    "[global {}] {}  {:?}",
                    global_ref.as_u32(),
                    entity_name(entity.name()),
                    entity.get_type(),
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

    fn memory_entries(&self) -> Vec<ListEntry> {
        self.module()
            .memories
            .iter()
            .map(|(memory_ref, entity)| ListEntry {
                label: format!(
                    "[memory {}] {}  {:?}",
                    memory_ref.as_u32(),
                    entity_name(entity.name()),
                    entity.get_type(),
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

    fn data_entries(&self) -> Vec<ListEntry> {
        let mut entries = Vec::new();

        for (data_ref, import) in self.module().extra.mem_layout.external().iter() {
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

        for (data_ref, place) in self.module().extra.mem_layout.item_places().iter() {
            let segment = &self.module().extra.mem_layout.segments()[place.segment_id];
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

    fn start_entries(&self) -> Vec<ListEntry> {
        let Some(func_ref) = self.module().extra.start_function else {
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
                self.entity_label(EntityKind::Function(func_ref))
            ),
            accent: Accent::Warning,
            action: Some(Scene::SectionDetail(SectionKind::Functions)),
            entity: Some(EntityKind::Function(func_ref)),
            inspect_target: self.inspect_target_for_entity(EntityKind::Function(func_ref)),
            detail_lines: vec![
                format!(
                    "start function: {}",
                    self.entity_label(EntityKind::Function(func_ref))
                ),
                "Enter to jump to Functions section.".to_owned(),
            ],
        }]
    }

    fn reloc_count_map(&self) -> HashMap<EntityKind, usize> {
        self.loaded()
            .relocs
            .iter_relocs()
            .map(|(entity, relocs)| (entity, relocs.len()))
            .collect()
    }

    fn move_selection(&mut self, delta: isize) {
        match self.current_scene {
            Scene::OverallView => {
                let len = self.source.raw_sections.len();
                let sel = wrap_index(self.overall_selected, len, delta);
                self.overall_scroll =
                    adjust_scroll(self.overall_scroll, sel, self.overall_viewport.get());
                self.overall_selected = sel;
            }
            Scene::SectionDetail(kind) => {
                let len = self.section_selected_len(kind);
                let sel = wrap_index(self.section_selected, len, delta);
                self.section_scroll =
                    adjust_scroll(self.section_scroll, sel, self.section_viewport.get());
                self.section_selected = sel;
            }
        }
    }

    fn page_selection(&mut self, delta: isize) {
        self.move_selection(delta);
    }

    fn drill_in(&mut self) {
        match self.current_scene.clone() {
            Scene::OverallView => {
                if let Some(kind) = self.selected_overall_section_kind() {
                    self.open_scene(Scene::SectionDetail(kind));
                }
            }
            Scene::SectionDetail(kind) => {
                if self.section_mode == SectionDetailMode::Structured
                    && let Some(entry) = self.selected_entry(kind)
                    && let Some(scene) = entry.action
                {
                    self.open_scene(scene);
                }
            }
        }
    }

    fn go_back(&mut self) {
        if let Some(previous) = self.scene_stack.pop() {
            self.current_scene = previous;
        }
    }

    fn open_scene(&mut self, scene: Scene) {
        if self.current_scene != scene {
            self.scene_stack.push(self.current_scene.clone());
            let reset = match (&self.current_scene, &scene) {
                (Scene::SectionDetail(old), Scene::SectionDetail(new)) => old != new,
                (Scene::OverallView, Scene::SectionDetail(_)) => true,
                _ => false,
            };
            self.current_scene = scene;
            if let Scene::SectionDetail(kind) = self.current_scene {
                self.overall_selected = self.section_index_for_kind(kind);
            }
            if reset {
                self.reset_section_state();
            }
        }
    }

    fn replace_scene(&mut self, scene: Scene) {
        let reset = match (&self.current_scene, &scene) {
            (Scene::SectionDetail(old), Scene::SectionDetail(new)) => old != new,
            (Scene::OverallView, Scene::SectionDetail(_)) => true,
            _ => false,
        };
        self.current_scene = scene;
        if let Scene::SectionDetail(kind) = self.current_scene {
            self.overall_selected = self.section_index_for_kind(kind);
        }
        if reset {
            self.reset_section_state();
        }
    }

    fn reset_section_state(&mut self) {
        self.section_selected = 0;
        self.section_scroll = 0;
        self.detail_scroll = 0;
    }

    fn top_level_scene(&self, idx: usize) -> Scene {
        match idx {
            0 => Scene::OverallView,
            1 => Scene::SectionDetail(self.default_section_kind()),
            _ => Scene::OverallView,
        }
    }

    fn move_scene(&mut self, delta: isize) {
        let next = move_index(self.current_scene.tab_index(), 2, delta);
        self.replace_scene(self.top_level_scene(next));
    }

    fn cycle_section_mode(&mut self) {
        if let Scene::SectionDetail(kind) = self.current_scene {
            self.section_mode = self.section_mode.next();
            self.detail_scroll = 0;
            self.section_selected = self
                .section_selected
                .min(self.section_selected_len(kind).saturating_sub(1));
        }
    }

    fn scene_for_entity(&self, entity: EntityKind) -> Option<Scene> {
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

    fn section_index_for_kind(&self, kind: SectionKind) -> usize {
        self.source
            .raw_sections
            .iter()
            .position(|block| block.section_id == kind.canonical_ids())
            .or_else(|| {
                self.source
                    .raw_sections
                    .iter()
                    .position(|block| kind.is_raw_eq(block.section_id))
            })
            .unwrap_or(0)
    }

    fn inspect_target_for_entity(&self, entity: EntityKind) -> Option<InspectTarget> {
        match entity {
            EntityKind::Function(func_ref) => self
                .module()
                .functions
                .try_get_entity(func_ref)
                .and_then(|entity| {
                    entity
                        .to_defined()
                        .map(|_| InspectTarget::Function(func_ref))
                }),
            EntityKind::DataSymbol(data_ref) => self
                .module()
                .extra
                .mem_layout
                .item_places()
                .get(data_ref)
                .map(|_| InspectTarget::Data(data_ref)),
            _ => None,
        }
    }

    fn entity_label(&self, entity: EntityKind) -> String {
        match entity {
            EntityKind::Function(func_ref) => {
                let entity = self.module().functions.try_get_entity(func_ref);
                let name = entity
                    .as_ref()
                    .and_then(|entry| entry.name())
                    .map(|name| name.as_ref())
                    .unwrap_or("<anon>");
                format!("func {} {}", func_ref.as_u32(), name)
            }
            EntityKind::DataSymbol(data_ref) => {
                if let Some(import) = self.module().extra.mem_layout.external().get(data_ref) {
                    return format!(
                        "data {} {}::{}",
                        data_ref.as_u32(),
                        import.module,
                        import.name
                    );
                }
                if let Some(place) = self.module().extra.mem_layout.item_places().get(data_ref) {
                    let segment = &self.module().extra.mem_layout.segments()[place.segment_id];
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
                let entity = self.module().globals.try_get_entity(global_ref);
                format!(
                    "global {} {}",
                    global_ref.as_u32(),
                    entity
                        .as_ref()
                        .and_then(|entry| entry.name())
                        .map(|name| name.as_ref())
                        .unwrap_or("<anon>")
                )
            }
            EntityKind::Table(table_ref) => {
                let entity = self.module().tables.try_get_entity(table_ref);
                format!(
                    "table {} {}",
                    table_ref.as_u32(),
                    entity
                        .as_ref()
                        .and_then(|entry| entry.name())
                        .map(|name| name.as_ref())
                        .unwrap_or("<anon>")
                )
            }
            EntityKind::Memory(memory_ref) => {
                let entity = self.module().memories.try_get_entity(memory_ref);
                format!(
                    "memory {} {}",
                    memory_ref.as_u32(),
                    entity
                        .as_ref()
                        .and_then(|entry| entry.name())
                        .map(|name| name.as_ref())
                        .unwrap_or("<anon>")
                )
            }
            EntityKind::Tag(tag_ref) => {
                let entity = self.module().tags.try_get_entity(tag_ref);
                format!(
                    "tag {} {}",
                    tag_ref.as_u32(),
                    entity
                        .as_ref()
                        .and_then(|entry| entry.name())
                        .map(|name| name.as_ref())
                        .unwrap_or("<anon>")
                )
            }
            EntityKind::Type(type_ref) => format!("type {}", type_ref.as_u32()),
        }
    }

    fn entity_bytes(&self, target: InspectTarget) -> Option<Vec<u8>> {
        match target {
            InspectTarget::Function(func_ref) => self
                .module()
                .functions
                .try_get_entity(func_ref)
                .and_then(|entity| entity.to_defined())
                .map(|defined| defined.body.iter_bytes().collect()),
            InspectTarget::Data(data_ref) => {
                let place = self.module().extra.mem_layout.item_places().get(data_ref)?;
                let segment = &self.module().extra.mem_layout.segments()[place.segment_id];
                let item = &segment.parts[place.part_id];
                Some(item.defined_entity.body.iter_bytes().collect())
            }
        }
    }
}

impl RawSummary {
    fn from_parts(
        loaded: &LoadedFile<'_>,
        validation_error: Option<String>,
        file_size: usize,
    ) -> Self {
        let module = &loaded.module;
        let raw = loaded.raw_reader();

        let data_count =
            module.extra.mem_layout.item_places().len() + module.extra.mem_layout.external().len();
        let structural_rows = vec![
            SectionSummary {
                kind: SectionKind::Types,
                title: "Types".to_owned(),
                count: raw.types.len(),
                note: format!("{} canonical function signatures", raw.types.len()),
            },
            SectionSummary {
                kind: SectionKind::Imports,
                title: "Imports".to_owned(),
                count: raw.imports.len(),
                note: format!("{} linker imports", raw.imports.len()),
            },
            SectionSummary {
                kind: SectionKind::Functions,
                title: "Functions".to_owned(),
                count: module.functions.len(),
                note: format!(
                    "{} imports, {} bodies",
                    module.functions.imports_iter().len(),
                    module.functions.defined_iter().len(),
                ),
            },
            SectionSummary {
                kind: SectionKind::Tables,
                title: "Tables".to_owned(),
                count: module.tables.len(),
                note: format!(
                    "{} imports, {} defined",
                    module.tables.imports_iter().len(),
                    module.tables.defined_iter().len(),
                ),
            },
            SectionSummary {
                kind: SectionKind::Memories,
                title: "Memories".to_owned(),
                count: module.memories.len(),
                note: format!(
                    "{} imports, {} defined",
                    module.memories.imports_iter().len(),
                    module.memories.defined_iter().len(),
                ),
            },
            SectionSummary {
                kind: SectionKind::Globals,
                title: "Globals".to_owned(),
                count: module.globals.len(),
                note: format!(
                    "{} imports, {} defined",
                    module.globals.imports_iter().len(),
                    module.globals.defined_iter().len(),
                ),
            },
            SectionSummary {
                kind: SectionKind::Exports,
                title: "Exports".to_owned(),
                count: raw.exports.len(),
                note: format!("{} exported entities", raw.exports.len()),
            },
            SectionSummary {
                kind: SectionKind::Start,
                title: "Start".to_owned(),
                count: usize::from(module.extra.start_function.is_some()),
                note: module
                    .extra
                    .start_function
                    .map(|func_ref| format!("starts at func {}", func_ref.as_u32()))
                    .unwrap_or_else(|| "no start section".to_owned()),
            },
            SectionSummary {
                kind: SectionKind::Elements,
                title: "Elements".to_owned(),
                count: raw.elements.len(),
                note: format!("{} element segments", raw.elements.len()),
            },
            SectionSummary {
                kind: SectionKind::Data,
                title: "Data".to_owned(),
                count: data_count,
                note: format!(
                    "{} raw segments, {} data symbols",
                    raw.data.data_segments.len(),
                    data_count,
                ),
            },
            SectionSummary {
                kind: SectionKind::Tags,
                title: "Tags".to_owned(),
                count: module.tags.len(),
                note: format!(
                    "{} imports, {} defined",
                    module.tags.imports_iter().len(),
                    module.tags.defined_iter().len(),
                ),
            },
        ];

        Self {
            file_size,
            target_features: target_features_label(raw),
            validation_error,
            structural_rows,
        }
    }
}

fn move_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }

    let next = current as isize + delta;
    next.clamp(0, len.saturating_sub(1) as isize) as usize
}

fn wrap_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }

    (current as isize + delta).rem_euclid(len as isize) as usize
}

/// Move `scroll` the minimum amount so that `selected` stays inside
/// `[scroll, scroll + viewport)`. Returns the unchanged scroll when selected
/// is already visible.
fn adjust_scroll(scroll: usize, selected: usize, viewport: usize) -> usize {
    if viewport == 0 {
        return 0;
    }
    if selected < scroll {
        selected
    } else if selected >= scroll + viewport {
        selected + 1 - viewport
    } else {
        scroll
    }
}

fn validate_wasm(bytes: &[u8]) -> Option<String> {
    wasmparser::Validator::new()
        .validate_all(bytes)
        .err()
        .map(|err| err.message().to_owned())
}

fn entity_name(name: Option<impl AsRef<str>>) -> String {
    name.map(|name| name.as_ref().to_owned())
        .unwrap_or_else(|| "<anon>".to_owned())
}

fn format_size_len(bytes: usize) -> String {
    format!(
        "{:<10} ({})",
        format_size_units(bytes),
        format_hex_len(bytes)
    )
}

fn format_size_units(bytes: usize) -> String {
    if bytes >= 1 << 30 {
        format!("{:.2} GB", bytes as f64 / (1 << 30) as f64)
    } else if bytes >= 1 << 20 {
        format!("{:.2} MB", bytes as f64 / (1 << 20) as f64)
    } else if bytes >= 1 << 10 {
        format!("{:.2} KB", bytes as f64 / (1 << 10) as f64)
    } else {
        format!("{} B", bytes)
    }
}

fn format_hex_len(bytes: usize) -> String {
    let width = format!("{bytes:x}").len().max(8);
    format!("0x{bytes:0width$x}")
}

fn function_type_label(func_type: &wasmparser::FuncType) -> String {
    let params = func_type
        .params()
        .iter()
        .map(val_type_label)
        .collect::<Vec<_>>()
        .join(", ");
    let results = func_type
        .results()
        .iter()
        .map(val_type_label)
        .collect::<Vec<_>>();

    if results.is_empty() {
        format!("fn({params})")
    } else {
        format!("fn({params}) -> {}", results.join(", "))
    }
}

fn val_type_label(ty: &wasmparser::ValType) -> String {
    format!("{ty:?}").to_lowercase()
}

fn target_features_label(raw: &ObjectReader<'_>) -> String {
    let features = &raw.target_features.features;
    let mut enabled = Vec::new();

    if features.mutable_global {
        enabled.push("mutable-globals");
    }
    if features.multi_value {
        enabled.push("multivalue");
    }
    if features.sign_extension {
        enabled.push("sign-ext");
    }
    if features.extended_const {
        enabled.push("extended-const");
    }
    if features.reference_types {
        enabled.push("reference-types");
    }
    if features.saturating_float_to_int {
        enabled.push("nontrapping-fptoint");
    }
    if features.bulk_memory {
        enabled.push("bulk-memory");
    }
    if features.bulk_memory_opt {
        enabled.push("bulk-memory-opt");
    }
    if features.call_indirect_overlong {
        enabled.push("call-indirect-overlong");
    }

    if enabled.is_empty() {
        "none".to_owned()
    } else {
        enabled.join(", ")
    }
}

fn plain_hexdump_rows(bytes: &[u8], base_offset: usize) -> Vec<HexdumpRow> {
    bytes
        .chunks(16)
        .enumerate()
        .map(|(row_idx, chunk)| HexdumpRow {
            offset: base_offset + row_idx * 16,
            bytes: chunk.iter().map(|byte| (*byte, None)).collect(),
        })
        .collect()
}

fn collect_raw_sections(raw: &ObjectReader<'_>) -> Vec<RawSectionBlock> {
    raw.section_headers
        .iter()
        .map(|header| {
            let mut range = header.content_range.clone();
            range.start = header.raw_start;
            raw_block(
                header.index,
                header.id,
                header.name.as_ref(),
                range,
                header.count,
            )
        })
        .collect()
}

fn raw_block(
    section_index: usize,
    section_id: SectionId,
    name: &str,
    range: Range<usize>,
    count: Option<usize>,
) -> RawSectionBlock {
    RawSectionBlock {
        section_index,
        section_id,
        name: name.to_owned(),
        range,
        count,
    }
}
