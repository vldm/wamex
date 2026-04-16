use std::cell::Cell;

/// Per-list selection and scroll state.
#[derive(Default)]
pub struct ListSelectionState {
    pub selected: usize,
    pub scroll: usize,
    pub viewport_hpos: Cell<usize>,
}

/// Move `scroll` the minimum amount so that `selected` stays inside
/// `[scroll, scroll + viewport)`. Returns the unchanged scroll when selected
/// is already visible.
pub fn adjust_scroll(scroll: usize, selected: usize, viewport: usize) -> usize {
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

pub fn wrap_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    (current.cast_signed() + delta)
        .rem_euclid(len.cast_signed())
        .cast_unsigned()
}
