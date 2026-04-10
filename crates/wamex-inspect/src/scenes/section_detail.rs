use ratatui::{
    layout::{Constraint, Direction, Layout},
    prelude::*,
    widgets::{Block, Borders, Paragraph, Tabs, Wrap},
};

use crate::{
    App,
    scene::{SectionDetailMode, SectionKind},
    theme,
};

pub fn render(frame: &mut Frame, area: Rect, app: &App, kind: SectionKind) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(3)])
        .split(area);

    match app.section_mode() {
        SectionDetailMode::Raw => render_raw(frame, chunks[0], app, kind),
        SectionDetailMode::StructuredShort => render_short(frame, chunks[0], app, kind),
        SectionDetailMode::StructuredDetailed => render_detailed(frame, chunks[0], app, kind),
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
    let mut lines = Vec::new();
    for block in app.raw_blocks(kind) {
        lines.push(Line::from(Span::styled(block.title, theme::title())));
        lines.extend(block.rows.into_iter().map(render_hexdump_row));
        lines.push(Line::from(""));
    }

    if lines.is_empty() {
        lines.push(Line::from("No raw section bytes for this view."));
    }

    let paragraph = Paragraph::new(lines.into_iter().skip(app.detail_scroll()).collect::<Vec<_>>())
        .block(
            Block::default()
                .title(format!("[{}] {}", kind.canonical_label(), kind.title()))
                .title_style(theme::title())
                .borders(Borders::ALL)
                .border_style(theme::border(true)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_short(frame: &mut Frame, area: Rect, app: &App, kind: SectionKind) {
    let entries = app.section_entries(kind);
    let lines = if entries.is_empty() {
        vec![Line::from("No entries")]
    } else {
        entries
            .iter()
            .enumerate()
            .map(|(idx, entry)| {
                let base_style = if idx == app.section_selected() {
                    theme::selection()
                } else {
                    theme::accent(entry.accent)
                };

                Line::from(Span::styled(entry.label.clone(), base_style))
            })
            .collect::<Vec<_>>()
    };

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .title(format!("[{}] {}", kind.canonical_label(), kind.title()))
                .title_style(theme::title())
                .borders(Borders::ALL)
                .border_style(theme::border(true)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_detailed(frame: &mut Frame, area: Rect, app: &App, kind: SectionKind) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(area);

    render_short(frame, chunks[0], app, kind);

    let mut lines = Vec::new();
    if let Some(detail) = app.detail_view(kind) {
        lines.push(Line::from(Span::styled(detail.title, theme::title())));
        lines.extend(detail.info_lines.into_iter().map(Line::from));

        if let Some(note) = detail.dump_note {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                note,
                Style::default().fg(Color::LightYellow),
            )));
        }
        if let Some(dump_title) = detail.dump_title {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(dump_title, theme::title())));
            lines.extend(detail.dump_rows.into_iter().map(render_hexdump_row));
        }
        if !detail.reloc_lines.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Relocations", theme::title())));
            lines.extend(detail.reloc_lines.into_iter().map(|reloc| {
                Line::from(Span::styled(reloc.label, theme::reloc(reloc.relation)))
            }));
        }
    } else {
        lines.push(Line::from("No detail for current selection."));
    }

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .title("Detail")
                .title_style(theme::title())
                .borders(Borders::ALL)
                .border_style(theme::border(true)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, chunks[1]);
}

fn render_hexdump_row(row: crate::HexdumpRow) -> Line<'static> {
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
