use ratatui::layout::Rect;

pub fn content_width(area: Rect) -> usize {
    area.width.saturating_sub(2) as usize
}

pub fn content_height(area: Rect) -> usize {
    area.height.saturating_sub(2) as usize
}

pub fn truncate_text(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }

    let text_width = text.chars().count();
    if text_width <= width {
        return text.to_owned();
    }

    if width <= 3 {
        return ".".repeat(width);
    }

    let mut truncated = text
        .chars()
        .take(width.saturating_sub(3))
        .collect::<String>();
    truncated.push_str("...");
    truncated
}
