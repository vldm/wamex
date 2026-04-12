use ratatui::{
    layout::{Constraint, Direction, Layout},
    prelude::*,
    widgets::{Block, Borders, Paragraph, Tabs, Wrap},
};

use super::helpers::{content_height, content_width, truncate_text};
use crate::{App, scene::OverallViewMode, theme};

// Print in human-friendly format, and full bytes in parens.
fn format_bytes_len(bytes: usize) -> String {
    let with_units = if bytes >= 1 << 30 {
        format!("{:.2} GB", bytes as f64 / (1 << 30) as f64)
    } else if bytes >= 1 << 20 {
        format!("{:.2} MB", bytes as f64 / (1 << 20) as f64)
    } else if bytes >= 1 << 10 {
        format!("{:.2} KB", bytes as f64 / (1 << 10) as f64)
    } else {
        format!("{} B", bytes)
    };
    format!("{} ({})", with_units, bytes)
}

fn metadata_widget(path: &std::path::Path, summary: &crate::RawSummary) -> Paragraph<'static> {
    let meta = vec![
        Line::from(format!("file: {}", path.display())),
        Line::from(format!("size: {}", format_bytes_len(summary.file_size))),
        Line::from(format!("features: {}", summary.target_features)),
    ];

    Paragraph::new(meta)
        .block(
            Block::default()
                .title("Metadata")
                .title_style(theme::title())
                .borders(Borders::ALL)
                .border_style(theme::border(false)),
        )
        .wrap(Wrap { trim: false })
}
//
// Render raw sections view as in `wasm-objdump -h`
//
fn sections_raw(app: &App, area: Rect) -> Paragraph<'static> {
    let sections = app.raw_sections();
    let len = sections.len();

    let width = content_width(area);
    let viewport = content_height(area);
    app.set_overall_viewport(viewport);
    let scroll = app.overall_scroll();
    let end = (scroll + viewport).min(len);

    let section_lines = sections
        .iter()
        .enumerate()
        .skip(scroll)
        .take(end.saturating_sub(scroll))
        .map(|(idx, block)| {
            let style = if idx == app.overall_selected() {
                theme::selection()
            } else {
                Style::default().fg(Color::White)
            };
            Line::from(Span::styled(
                truncate_text(&app.raw_section_title(block), width),
                style,
            ))
        })
        .collect::<Vec<_>>();

    Paragraph::new(section_lines)
        .block(
            Block::default()
                .title("Sections")
                .title_style(theme::title())
                .borders(Borders::ALL)
                .border_style(theme::border(true)),
        )
        .wrap(Wrap { trim: false })
}

fn sections_structural(app: &App, area: Rect) -> Paragraph<'static> {
    let rows = app.structural_overview();
    let len = rows.len();

    let width = content_width(area);
    let viewport = content_height(area);
    app.set_overall_viewport(viewport);
    let scroll = app.overall_scroll();
    let end = (scroll + viewport).min(len);

    let lines = rows
        .iter()
        .enumerate()
        .skip(scroll)
        .take(end.saturating_sub(scroll))
        .map(|(idx, row)| {
            let style = if idx == app.overall_selected() {
                theme::selection()
            } else {
                Style::default().fg(Color::White)
            };
            Line::from(Span::styled(truncate_text(&row.text, width), style))
        })
        .collect::<Vec<_>>();

    Paragraph::new(lines)
        .block(
            Block::default()
                .title("Sections [structural]")
                .title_style(theme::title())
                .borders(Borders::ALL)
                .border_style(theme::border(true)),
        )
        .wrap(Wrap { trim: false })
}

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(area);

    frame.render_widget(metadata_widget(app.path(), app.summary()), chunks[0]);

    match app.overall_mode() {
        OverallViewMode::Raw => {
            let w = sections_raw(app, chunks[1]);
            frame.render_widget(w, chunks[1]);
        }
        OverallViewMode::Structural => {
            let w = sections_structural(app, chunks[1]);
            frame.render_widget(w, chunks[1]);
        }
    }

    let mode_tabs = Tabs::new(
        OverallViewMode::ALL
            .into_iter()
            .map(|mode| Line::from(mode.title()))
            .collect::<Vec<_>>(),
    )
    .select(app.overall_mode().tab_index())
    .highlight_style(theme::selection())
    .block(
        Block::default()
            .title("Mode")
            .title_style(theme::title())
            .borders(Borders::ALL)
            .border_style(theme::border(true)),
    );
    frame.render_widget(mode_tabs, chunks[2]);
}
