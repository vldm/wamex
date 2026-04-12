use crate::{
    scene::SectionKind,
    scroll::{ListSelectionState, adjust_scroll, wrap_index},
    source::{RawSectionBlock, format_size_len},
};

pub struct OverallState {
    pub(crate) selection: ListSelectionState,
}

impl Default for OverallState {
    fn default() -> Self {
        Self {
            selection: ListSelectionState::default(),
        }
    }
}

impl OverallState {
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
}

// ─── Free functions ───────────────────────────────────────────────────────────

pub fn raw_section_title(block: &RawSectionBlock) -> String {
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

/// Returns the index of the first `RawSectionBlock` whose id matches `kind`.
pub fn section_index_for_kind(raw_sections: &[RawSectionBlock], kind: SectionKind) -> usize {
    raw_sections
        .iter()
        .position(|block| block.section_id == kind.canonical_ids())
        .or_else(|| {
            raw_sections
                .iter()
                .position(|block| kind.is_raw_eq(block.section_id))
        })
        .unwrap_or(0)
}
