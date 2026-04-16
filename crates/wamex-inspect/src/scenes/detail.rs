use ratatui::{
    layout::Rect,
    prelude::*,
    widgets::{Block, Borders, Paragraph, Wrap},
};

use super::helpers::{content_width, render_hexdump_row, truncate_text};
use crate::{App, SectionKind, scene::ViewMode, theme};

/// Renders the Detail scene (full screen — one specific section entry).
pub fn render(frame: &mut Frame, area: Rect, app: &App, kind: SectionKind, idx: usize) {
    match app.mode() {
        ViewMode::Raw => render_raw(frame, area, app, kind, idx),
        ViewMode::Structured => render_structured(frame, area, app, kind, idx),
    }
}

/// Renders a raw hexdump of the `idx`-th raw block for `kind`. Used by Detail[raw]
/// and by the preview pane when in Section[raw] mode.
pub fn render_raw_block(frame: &mut Frame, area: Rect, app: &App, kind: SectionKind, idx: usize) {
    let width = content_width(area);
    let mut lines = Vec::new();
    if let Some(block) = app.raw_block_at(kind, idx) {
        lines.push(Line::from(Span::styled(
            truncate_text(&block.title, width),
            theme::title(),
        )));
        lines.push(Line::from(""));
        lines.extend(block.rows.into_iter().map(render_hexdump_row));
    } else {
        lines.push(Line::from("No raw bytes for this entry"));
    }

    let p = Paragraph::new(lines)
        .block(
            Block::default()
                .title(format!("{} raw", app.section_label(kind)))
                .title_style(theme::title())
                .borders(Borders::ALL)
                .border_style(theme::border(true)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(p, area);
}

/// Renders a structured detail view for entry `idx` in `kind`. Used by Detail[structured]
/// and by the preview pane when in Section[structured] mode.
pub fn render_structured_detail(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    kind: SectionKind,
    idx: usize,
) {
    let width = content_width(area);
    let mut lines = Vec::new();

    if let Some(summary) = app.structured_section_summary(kind) {
        lines.push(Line::from(Span::styled(
            truncate_text(&summary.note, width),
            theme::accent(crate::Accent::Muted),
        )));

        if let Some(notice) = app.section_notice(kind) {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                truncate_text(notice, width),
                theme::accent(crate::Accent::Warning),
            )));
        }

        if let Some(detail) = app.detail_view_for(kind, idx) {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                truncate_text(&detail.title, width),
                theme::title(),
            )));
            lines.extend(detail.info_lines.into_iter().map(|line| {
                Line::from(Span::styled(
                    truncate_text(&line, width),
                    Style::default().fg(Color::White),
                ))
            }));

            if let Some(note) = detail.dump_note {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    truncate_text(&note, width),
                    theme::accent(crate::Accent::Warning),
                )));
            }

            if let Some(dump_title) = detail.dump_title {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    truncate_text(&dump_title, width),
                    theme::title(),
                )));
                lines.extend(detail.dump_rows.into_iter().map(render_hexdump_row));
            }

            if !detail.reloc_lines.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled("Relocations", theme::title())));
                lines.extend(detail.reloc_lines.into_iter().map(|reloc| {
                    Line::from(Span::styled(
                        truncate_text(&reloc.label, width),
                        theme::reloc(reloc.relation),
                    ))
                }));
            }
        } else {
            lines.push(Line::from(""));
            lines.push(Line::from("No structured detail for this entry"));
        }
    } else {
        lines.push(Line::from("No structured detail for this section"));
    }

    let p = Paragraph::new(lines)
        .block(
            Block::default()
                .title(format!("{} detail", app.section_label(kind)))
                .title_style(theme::title())
                .borders(Borders::ALL)
                .border_style(theme::border(true)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(p, area);
}

fn render_raw(frame: &mut Frame, area: Rect, app: &App, kind: SectionKind, idx: usize) {
    render_raw_block(frame, area, app, kind, idx);
}

fn render_structured(frame: &mut Frame, area: Rect, app: &App, kind: SectionKind, idx: usize) {
    render_structured_detail(frame, area, app, kind, idx);
}
