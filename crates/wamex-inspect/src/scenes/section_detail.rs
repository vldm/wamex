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
    use super::helpers::render_semantic_dump;

    let viewport = content_height(area);
    app.set_section_viewport(viewport);
    let scroll = app.section_scroll();
    let width = content_width(area);

    let mut lines = Vec::new();
    if let Some(block) = app.section_detail_raw_dump() {
        lines.push(Line::from(Span::styled(
            truncate_text(&block.title, width),
            theme::title(),
        )));
        lines.push(Line::from(""));
        lines.extend(render_semantic_dump(&block.dump));
    } else {
        lines.push(Line::from("No raw bytes for this section"));
    }

    let p = Paragraph::new(lines)
        .block(
            Block::default()
                .title(format!("{} raw", app.section_label(kind)))
                .title_style(theme::title())
                .borders(Borders::ALL)
                .border_style(theme::border(true)),
        )
        .scroll((scroll as u16, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(p, area);
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
