use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use wamex_object::typed::{EntityKind, FileLoader, LoadedFile, Module};

use crate::{
    hexdump::RawBlockView,
    scene::{InspectTarget, Scene, SectionKind, ViewMode},
    scenes::{
        overall_state::{OverallState, raw_section_title},
        section_detail_state::{
            DetailView, ListEntry, RelocationLine, SectionDetailState, detail_view,
            detail_view_at_index, hexdump_rows, raw_blocks, raw_preview, reloc_lines,
            section_entries, selected_len,
        },
    },
    source::{
        RawSectionBlock, RawSummary, SectionSummary, SourceFile, StructuralRow,
        build_structural_overview, collect_raw_sections, validate_wasm,
    },
};

pub struct App {
    source: SourceFile,
    current_scene: Scene,

    mode: ViewMode,
    overall: OverallState,
    section_detail: SectionDetailState,

    show_help: bool,
    show_preview: bool,
    should_quit: bool,
}

impl App {
    pub fn load(path: PathBuf) -> anyhow::Result<Self> {
        let bytes = std::fs::read(&path)?;
        let file_size = bytes.len();
        let mut file_loader = FileLoader::new();
        let file_id = file_loader.load_from_bytes(bytes.clone().into_boxed_slice())?;

        let validation_error = validate_wasm(&bytes);

        let loaded = file_loader.get_file(file_id);
        let raw_sections = collect_raw_sections(loaded.raw_reader());
        let summary = RawSummary::from_parts(loaded, validation_error, file_size);
        let structural_overview = build_structural_overview(loaded);

        Ok(Self {
            source: SourceFile {
                path,
                bytes: bytes.into_boxed_slice(),
                raw_sections,
                loader: file_loader,
                file_id,
                summary,
                structural_overview,
            },
            current_scene: Scene::OverallView,
            mode: ViewMode::Structured,
            show_help: false,
            show_preview: true,
            should_quit: false,
            overall: OverallState::default(),
            section_detail: SectionDetailState::default(),
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
                self.jump_to_scene(idx);
            }
            KeyCode::Char('p' | 'P') => self.show_preview = !self.show_preview,
            KeyCode::Char('s' | 'S') => match self.current_scene {
                Scene::OverallView | Scene::SectionDetail(_) => self.cycle_mode(),
                Scene::Detail(kind, _) => {
                    self.current_scene = Scene::SectionDetail(kind);
                    self.cycle_mode();
                }
            },
            KeyCode::Esc | KeyCode::Backspace => self.go_back(),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-8),
            KeyCode::PageDown => self.move_selection(8),
            KeyCode::Enter => self.drill_in(),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            _ => {}
        }
    }

    // ─── State accessors ─────────────────────────────────────────────────────

    pub fn current_scene(&self) -> &Scene {
        &self.current_scene
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn show_help(&self) -> bool {
        self.show_help
    }

    pub fn show_preview(&self) -> bool {
        self.show_preview
    }

    pub fn mode(&self) -> ViewMode {
        self.mode
    }

    pub fn is_structured_mode(&self) -> bool {
        self.mode == ViewMode::Structured
    }

    // ─── Source accessors ────────────────────────────────────────────────────

    pub fn path(&self) -> &Path {
        self.source.path()
    }

    pub fn summary(&self) -> &RawSummary {
        self.source.summary()
    }

    pub fn loaded(&self) -> &LoadedFile<'_> {
        self.source.loaded()
    }

    pub fn module(&self) -> &Module<'_> {
        &self.source.loader.get_file(self.source.file_id).module
    }

    pub(crate) fn raw_sections(&self) -> &[RawSectionBlock] {
        &self.source.raw_sections
    }

    pub fn structural_overview(&self) -> &[StructuralRow] {
        &self.source.structural_overview
    }

    // ─── Overall-view delegates ──────────────────────────────────────────────

    pub fn overall_selected(&self) -> usize {
        self.overall.selected()
    }

    pub fn overall_scroll(&self) -> usize {
        self.overall.scroll()
    }

    pub fn set_overall_viewport(&self, h: usize) {
        self.overall.set_viewport(h);
    }

    pub fn raw_section_title(&self, block: &RawSectionBlock) -> String {
        raw_section_title(block)
    }

    // ─── Section-detail delegates ────────────────────────────────────────────

    pub fn section_selected(&self) -> usize {
        self.section_detail.selected()
    }

    pub fn section_scroll(&self) -> usize {
        self.section_detail.scroll()
    }

    pub fn set_section_viewport(&self, h: usize) {
        self.section_detail.set_viewport(h);
    }

    pub fn section_label(&self, kind: SectionKind) -> String {
        format!("[{}] {}", kind.canonical_label(), kind.title())
    }

    pub fn section_selected_len(&self, kind: SectionKind) -> usize {
        let loaded = self.source.loaded();
        selected_len(
            &self.source.bytes,
            &self.source.raw_sections,
            self.module(),
            loaded,
            self.mode,
            kind,
        )
    }

    pub fn section_entries(&self, kind: SectionKind) -> Vec<ListEntry> {
        let loaded = self.source.loaded();
        section_entries(self.module(), &loaded, kind)
    }

    pub fn structured_preview_entries(&self, kind: SectionKind) -> Vec<ListEntry> {
        self.section_entries(kind)
    }

    pub fn raw_blocks(&self, kind: SectionKind) -> Vec<RawBlockView> {
        raw_blocks(&self.source.bytes, &self.source.raw_sections, kind)
    }

    pub fn raw_preview(&self, kind: SectionKind) -> Option<RawBlockView> {
        raw_preview(
            &self.source.bytes,
            &self.source.raw_sections,
            &self.section_detail,
            kind,
        )
    }

    pub fn detail_view(&self, kind: SectionKind) -> Option<DetailView> {
        let loaded = self.source.loaded();
        detail_view(self.module(), &loaded, &self.section_detail, kind)
    }

    pub fn reloc_lines(&self, entity: EntityKind) -> Vec<RelocationLine> {
        let loaded = self.source.loaded();
        reloc_lines(self.module(), &loaded, entity)
    }

    pub fn hexdump_rows(&self, target: InspectTarget) -> Option<(String, semdump::SemanticDump<'static>)> {
        let loaded = self.source.loaded();
        hexdump_rows(self.module(), &loaded, target)
    }

    pub fn detail_view_for(&self, kind: SectionKind, idx: usize) -> Option<DetailView> {
        let loaded = self.source.loaded();
        detail_view_at_index(self.module(), &loaded, kind, idx)
    }

    pub fn raw_block_at(&self, kind: SectionKind, idx: usize) -> Option<RawBlockView> {
        self.raw_blocks(kind).into_iter().nth(idx)
    }

    pub fn overview_raw_preview(&self) -> Option<RawBlockView> {
        use crate::hexdump::plain_semantic_dump;
        let block = self.source.raw_sections.get(self.overall.selected())?;
        Some(RawBlockView {
            title: raw_section_title(block),
            dump: plain_semantic_dump(&self.source.bytes[block.range.clone()], block.range.start),
        })
    }

    pub fn overview_structural_preview_section(&self) -> SectionKind {
        self.source
            .structural_overview
            .get(self.overall.selected())
            .and_then(|row| row.kind)
            .or_else(|| self.selected_overall_section_kind())
            .unwrap_or(SectionKind::Types)
    }

    // ─── Derived section helpers ─────────────────────────────────────────────

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
            Scene::SectionDetail(kind) | Scene::Detail(kind, _) => kind,
            Scene::OverallView => self.default_section_kind(),
        }
    }

    pub fn structured_section_summary(&self, kind: SectionKind) -> Option<&SectionSummary> {
        self.summary()
            .structural_rows
            .iter()
            .find(|row| row.kind == kind)
    }

    pub fn status_notice(&self) -> Option<String> {
        let kind = match self.current_scene {
            Scene::SectionDetail(kind) | Scene::Detail(kind, _) => kind,
            Scene::OverallView => return None,
        };
        if self.mode != ViewMode::Structured {
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

    // ─── Scene navigation ────────────────────────────────────────────────────

    fn move_selection(&mut self, delta: isize) {
        match self.current_scene {
            Scene::OverallView => match self.mode {
                ViewMode::Raw => {
                    let len = self.source.raw_sections.len();
                    self.overall.move_selection(delta, len);
                }
                ViewMode::Structured => {
                    // Collect indices of selectable (non-separator) rows
                    let selectable: Vec<usize> = self
                        .source
                        .structural_overview
                        .iter()
                        .enumerate()
                        .filter(|(_, r)| r.kind.is_some())
                        .map(|(i, _)| i)
                        .collect();
                    if selectable.is_empty() {
                        return;
                    }
                    // Find position of `selected` within selectable slice
                    let cur_sel = self.overall.selected();
                    let pos = selectable.iter().position(|&i| i == cur_sel).unwrap_or(0);
                    let next_pos =
                        (pos as isize + delta).clamp(0, selectable.len() as isize - 1) as usize;
                    let new_selected = selectable[next_pos];
                    self.overall.move_selection(
                        new_selected as isize - cur_sel as isize,
                        self.source.structural_overview.len(),
                    );
                }
            },
            Scene::SectionDetail(kind) => {
                let len = self.section_selected_len(kind);
                self.section_detail.move_selection(delta, len);
            }
            Scene::Detail(_, _) => {} // Detail has no list cursor
        }
    }

    fn drill_in(&mut self) {
        match self.current_scene.clone() {
            Scene::OverallView => match self.mode {
                ViewMode::Raw => {
                    if let Some(kind) = self.selected_overall_section_kind() {
                        self.current_scene = Scene::SectionDetail(kind);
                        self.section_detail.reset();
                    }
                }
                ViewMode::Structured => {
                    let sel = self.overall.selected();
                    if let Some(kind) = self
                        .source
                        .structural_overview
                        .get(sel)
                        .and_then(|r| r.kind)
                    {
                        self.current_scene = Scene::SectionDetail(kind);
                        self.section_detail.reset();
                    }
                }
            },
            Scene::SectionDetail(kind) => {
                let idx = self.section_detail.selected();
                let len = self.section_selected_len(kind);
                if len > 0 && idx < len {
                    self.current_scene = Scene::Detail(kind, idx);
                }
            }
            Scene::Detail(_, _) => {} // no further drilling from Detail
        }
    }

    fn go_back(&mut self) {
        self.current_scene = match self.current_scene {
            Scene::Detail(kind, _) => Scene::SectionDetail(kind),
            Scene::SectionDetail(_) | Scene::OverallView => Scene::OverallView,
        };
    }

    fn cycle_mode(&mut self) {
        self.mode = self.mode.next();
        match self.current_scene {
            Scene::OverallView => {
                self.overall.selection.selected = 0;
                self.overall.selection.scroll = 0;
            }
            Scene::SectionDetail(_) | Scene::Detail(_, _) => {
                self.section_detail.reset();
            }
        }
    }

    fn jump_to_scene(&mut self, idx: usize) {
        let new_scene = match idx {
            0 => Scene::OverallView,
            1 => Scene::SectionDetail(self.default_section_kind()),
            _ => return,
        };
        if self.current_scene == new_scene {
            return;
        }
        let reset = match (&self.current_scene, &new_scene) {
            (Scene::SectionDetail(old), Scene::SectionDetail(new)) => old != new,
            (_, Scene::SectionDetail(_)) => true,
            _ => false,
        };
        self.current_scene = new_scene;
        if reset {
            self.section_detail.reset();
        }
    }

    fn selected_overall_section_kind(&self) -> Option<SectionKind> {
        self.source
            .raw_sections
            .get(self.overall.selected())
            .and_then(|block| SectionKind::from_section_id(block.section_id))
    }
}

fn move_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    let next = current as isize + delta;
    next.clamp(0, len.saturating_sub(1) as isize) as usize
}
