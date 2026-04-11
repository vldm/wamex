use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    prelude::*,
    widgets::{Block, Borders, Paragraph, Tabs, Wrap},
};

use crate::{App, HexdumpRow, SectionKind, scene::SectionDetailMode, theme};
use super::helpers::{content_height, content_width, truncate_text};

pub fn render(frame: &mut Frame, area: Rect, app: &App, kind: SectionKind) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(3)])
        .split(area);

    match app.section_mode() {
        SectionDetailMode::Raw => render_raw(frame, chunks[0], app, kind),
        SectionDetailMode::Structured => render_structured(frame, chunks[0], app, kind),
    }

    let mode_tabs = Tabs::new(
        SectionDetailMode::ALL
            .into_iter()
            .map(|mode| Line::from(mode.title()))
            .collect::<Vec<_>>(),
    )
    .select(app.section_mode().tab_index())
    .highlight_style(theme::selection())
    .block(
        Block::default()
            .title("Mode")
            .title_style(theme::title())
            .borders(Borders::ALL)
            .border_style(theme::border(true)),
    );
    frame.render_widget(mode_tabs, chunks[1]);
}

fn render_raw(frame: &mut Frame, area: Rect, app: &App, kind: SectionKind) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(area);

    let left_width = content_width(chunks[0]);
    let blocks = app.raw_blocks(kind);
    let viewport = content_height(chunks[0]);
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
                Line::from(Span::styled(truncate_text(&block.title, left_width), style))
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
    frame.render_widget(list, chunks[0]);

    let right_width = content_width(chunks[1]);
    let mut preview_lines = Vec::new();
    if let Some(block) = app.raw_preview(kind) {
        preview_lines.push(Line::from(Span::styled(
            truncate_text(&block.title, right_width),
            theme::title(),
        )));
        preview_lines.push(Line::from(""));
        preview_lines.extend(block.rows.into_iter().map(render_hexdump_row));
    } else {
        preview_lines.push(Line::from("No raw preview"));
    }

    let preview = Paragraph::new(preview_lines)
        .block(
            Block::default()
                .title("Preview")
                .title_style(theme::title())
                .borders(Borders::ALL)
                .border_style(theme::border(true)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(preview, chunks[1]);
}

fn render_structured(frame: &mut Frame, area: Rect, app: &App, kind: SectionKind) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(area);

    let left_width = content_width(chunks[0]);
    let entries = app.structured_preview_entries(kind);
    let viewport = content_height(chunks[0]);
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
                    truncate_text(&format!("{prefix}{}", entry.label), left_width),
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
    frame.render_widget(list, chunks[0]);

    let right_width = content_width(chunks[1]);
    let mut preview_lines = Vec::new();
    if let Some(summary) = app.structured_section_summary(kind) {
        preview_lines.push(Line::from(Span::styled(
            truncate_text(&app.section_label(kind), right_width),
            theme::title(),
        )));
        preview_lines.push(Line::from(Span::styled(
            truncate_text(&summary.note, right_width),
            theme::accent(crate::Accent::Muted),
        )));

        if let Some(notice) = app.section_notice(kind) {
            preview_lines.push(Line::from(""));
            preview_lines.push(Line::from(Span::styled(
                truncate_text(notice, right_width),
                theme::accent(crate::Accent::Warning),
            )));
        }

        if let Some(detail) = app.detail_view(kind) {
            preview_lines.push(Line::from(""));
            preview_lines.push(Line::from(Span::styled(
                truncate_text(&detail.title, right_width),
                theme::title(),
            )));
            preview_lines.extend(detail.info_lines.into_iter().map(|line| {
                Line::from(Span::styled(
                    truncate_text(&line, right_width),
                    Style::default().fg(Color::White),
                ))
            }));

            if let Some(note) = detail.dump_note {
                preview_lines.push(Line::from(""));
                preview_lines.push(Line::from(Span::styled(
                    truncate_text(&note, right_width),
                    theme::accent(crate::Accent::Warning),
                )));
            }

            if let Some(dump_title) = detail.dump_title {
                preview_lines.push(Line::from(""));
                preview_lines.push(Line::from(Span::styled(
                    truncate_text(&dump_title, right_width),
                    theme::title(),
                )));
                preview_lines.extend(detail.dump_rows.into_iter().map(render_hexdump_row));
            }

            if !detail.reloc_lines.is_empty() {
                preview_lines.push(Line::from(""));
                preview_lines.push(Line::from(Span::styled("Relocations", theme::title())));
                preview_lines.extend(detail.reloc_lines.into_iter().map(|reloc| {
                    Line::from(Span::styled(
                        truncate_text(&reloc.label, right_width),
                        theme::reloc(reloc.relation),
                    ))
                }));
            }
        } else {
            preview_lines.push(Line::from(""));
            preview_lines.push(Line::from("No structured preview"));
        }
    } else {
        preview_lines.push(Line::from("No structured preview"));
    }

    let preview = Paragraph::new(preview_lines)
        .block(
            Block::default()
                .title("Preview")
                .title_style(theme::title())
                .borders(Borders::ALL)
                .border_style(theme::border(true)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(preview, chunks[1]);
}

fn render_hexdump_row(row: HexdumpRow) -> Line<'static> {
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
