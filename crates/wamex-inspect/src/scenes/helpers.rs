use ratatui::{
    layout::Rect,
    prelude::*,
};

use crate::{HexdumpRow, theme};

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

pub fn render_hexdump_row(row: HexdumpRow) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("{:08x}  ", row.offset),
        Style::default().fg(Color::DarkGray),
    )];

    for (idx, (byte, relation)) in row.bytes.iter().enumerate() {
        let style = relation
            .map(theme::reloc)
            .unwrap_or_else(|| Style::default().fg(Color::White));
        spans.push(Span::styled(format!("{:02x}", byte), style));
        spans.push(Span::raw(if idx == 7 { "  " } else { " " }));
    }

    if row.bytes.len() < 16 {
        for idx in row.bytes.len()..16 {
            let padding = if idx == 7 { "   " } else { "  " };
            spans.push(Span::raw(format!("{padding} ")));
        }
    }

    spans.push(Span::raw(" |"));
    for (byte, relation) in &row.bytes {
        let ch = if byte.is_ascii_graphic() || *byte == b' ' {
            char::from(*byte)
        } else {
            '.'
        };
        spans.push(Span::styled(ch.to_string(), theme::ascii(*relation)));
    }
    spans.push(Span::raw("|"));

    Line::from(spans)
}
