use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use wamex_object::typed::{EntityKind, FileLoader, LoadedFile, Module};

use crate::{
    hexdump::{HexdumpRow, RawBlockView},
    scene::{InspectTarget, Scene, SectionDetailMode, SectionKind},
    scenes::{
        overall_state::{OverallState, raw_section_title, section_index_for_kind},
        section_detail_state::{
            Accent, DetailView, ListEntry, RelocationLine, SectionDetailState, detail_view,
            entity_label, hexdump_rows, raw_blocks, raw_preview, reloc_lines, scene_for_entity,
            section_entries, selected_len,
        },
    },
    source::{
        RawSectionBlock, RawSummary, SectionSummary, SourceFile, collect_raw_sections,
        validate_wasm,
    },
};

pub struct App {
    source: SourceFile,
    current_scene: Scene,
    scene_stack: Vec<Scene>,

    overall: OverallState,
    section_detail: SectionDetailState,

    show_help: bool,
    should_quit: bool,
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
        let summary = RawSummary::from_parts(&loaded, validation_error, file_size);

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
            show_help: false,
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
                self.replace_scene(self.top_level_scene(idx));
            }
            KeyCode::Left | KeyCode::Char('h') => self.move_scene(-1),
            KeyCode::Right | KeyCode::Char('l') => self.move_scene(1),
            KeyCode::Tab => self.cycle_section_mode(),
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

    pub fn section_mode(&self) -> SectionDetailMode {
        self.section_detail.mode()
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
            &loaded,
            &self.section_detail,
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

    pub fn hexdump_rows(&self, target: InspectTarget) -> Option<(String, Vec<HexdumpRow>)> {
        let loaded = self.source.loaded();
        hexdump_rows(self.module(), &loaded, target)
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

    pub fn status_notice(&self) -> Option<String> {
        let Scene::SectionDetail(kind) = self.current_scene else {
            return None;
        };
        if self.section_detail.mode() != SectionDetailMode::Structured {
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
            Scene::OverallView => {
                let len = self.source.raw_sections.len();
                self.overall.move_selection(delta, len);
            }
            Scene::SectionDetail(kind) => {
                let len = self.section_selected_len(kind);
                self.section_detail.move_selection(delta, len);
            }
        }
    }

    fn drill_in(&mut self) {
        match self.current_scene.clone() {
            Scene::OverallView => {
                if let Some(kind) = self.selected_overall_section_kind() {
                    self.open_scene(Scene::SectionDetail(kind));
                }
            }
            Scene::SectionDetail(kind) => {
                if self.section_detail.mode() == SectionDetailMode::Structured
                    && let Some(entry) = self
                        .section_entries(kind)
                        .into_iter()
                        .nth(self.section_detail.selected())
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
                self.overall.selection.selected =
                    section_index_for_kind(&self.source.raw_sections, kind);
            }
            if reset {
                self.section_detail.reset();
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
            self.overall.selection.selected =
                section_index_for_kind(&self.source.raw_sections, kind);
        }
        if reset {
            self.section_detail.reset();
        }
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
            let current_len = self.section_selected_len(kind);
            self.section_detail.cycle_mode(kind, current_len);
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
