use ratatui::{
    layout::Rect,
    prelude::*,
    widgets::{Block, Borders, Paragraph, Wrap},
};

use super::helpers::{content_height, content_width, truncate_text};
use crate::{App, SectionKind, scene::ViewMode, theme};

pub fn render(frame: &mut Frame, area: Rect, app: &App, kind: SectionKind) {
    match app.mode() {
        ViewMode::Raw => render_raw(frame, area, app, kind),
        ViewMode::Structured => render_structured(frame, area, app, kind),
    }
}

fn render_raw(frame: &mut Frame, area: Rect, app: &App, kind: SectionKind) {
    let width = content_width(area);
    let blocks = app.raw_blocks(kind);
    let viewport = content_height(area);
    app.set_section_viewport(viewport);
    let len = blocks.len();
    let scroll = app.section_scroll();
    let end = (scroll + viewport).min(len);
    let lines = if blocks.is_empty() {
        vec![Line::from("No raw section bytes for this section")]
    } else {
        blocks
            .iter()
            .enumerate()
            .skip(scroll)
            .take(end.saturating_sub(scroll))
            .map(|(idx, block)| {
                let style = if idx == app.section_selected() {
                    theme::selection()
                } else {
                    Style::default().fg(Color::White)
                };
                Line::from(Span::styled(truncate_text(&block.title, width), style))
            })
            .collect::<Vec<_>>()
    };

    let list = Paragraph::new(lines)
        .block(
            Block::default()
                .title(format!("{} raw", app.section_label(kind)))
                .title_style(theme::title())
                .borders(Borders::ALL)
                .border_style(theme::border(true)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(list, area);
}

fn render_structured(frame: &mut Frame, area: Rect, app: &App, kind: SectionKind) {
    let width = content_width(area);
    let entries = app.structured_preview_entries(kind);
    let viewport = content_height(area);
    app.set_section_viewport(viewport);
    let len = entries.len();
    let scroll = app.section_scroll();
    let end = (scroll + viewport).min(len);
    let lines = if entries.is_empty() {
        vec![Line::from("No structured entries")]
    } else {
        entries
            .iter()
            .enumerate()
            .skip(scroll)
            .take(end.saturating_sub(scroll))
            .map(|(idx, entry)| {
                let prefix = if app.is_partial_section(kind) {
                    "! "
                } else {
                    "  "
                };
                let style = if idx == app.section_selected() {
                    theme::selection()
                } else if app.is_partial_section(kind) {
                    theme::accent(crate::Accent::Muted)
                } else {
                    theme::accent(entry.accent)
                };
                Line::from(Span::styled(
                    truncate_text(&format!("{prefix}{}", entry.label), width),
                    style,
                ))
            })
            .collect::<Vec<_>>()
    };

    let list = Paragraph::new(lines)
        .block(
            Block::default()
                .title(format!("{} structured", app.section_label(kind)))
                .title_style(theme::title())
                .borders(Borders::ALL)
                .border_style(theme::border(true)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(list, area);
}
