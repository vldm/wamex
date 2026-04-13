use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    prelude::*,
    widgets::{Block, Borders, Clear, Paragraph, Tabs, Wrap},
};

use crate::{App, legend, scene::Scene, scenes, source::format_size_len, theme};

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(1),
        ])
        .split(area);

    let status = render_status(app);
    frame.render_widget(status, chunks[0]);

    let tabs = render_tabs(app.current_scene());
    frame.render_widget(tabs, chunks[1]);

    // Split body area into main pane + optional preview pane.
    let is_detail = matches!(app.current_scene(), Scene::Detail(_, _));
    let (main_area, preview_area) = if app.show_preview() && !is_detail {
        let split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(chunks[2]);
        (split[0], Some(split[1]))
    } else {
        (chunks[2], None)
    };

    match app.current_scene() {
        Scene::OverallView => scenes::overall::render(frame, main_area, app),
        Scene::SectionDetail(kind) => scenes::section_detail::render(frame, main_area, app, *kind),
        Scene::Detail(kind, idx) => scenes::detail::render(frame, main_area, app, *kind, *idx),
    }

    if let Some(preview_area) = preview_area {
        render_preview(frame, preview_area, app);
    }

    let footer = Paragraph::new(legend::footer(app)).style(Style::default().fg(Color::Gray));
    frame.render_widget(footer, chunks[3]);

    if app.show_help() {
        let popup = centered_rect(area, 68, 52);
        frame.render_widget(Clear, popup);
        let help = Paragraph::new(legend::help())
            .block(
                Block::default()
                    .title("Help")
                    .title_style(theme::title())
                    .borders(Borders::ALL)
                    .border_style(theme::border(true)),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(help, popup);
    }
}

/// Render the preview pane — shows content one level deeper than the current scene.
fn render_preview(frame: &mut Frame, area: Rect, app: &App) {
    use crate::scenes::helpers::{content_width, render_hexdump_row, truncate_text};

    match app.current_scene() {
        Scene::OverallView => {
            // Preview shows Section content for the currently highlighted section.
            match app.mode() {
                crate::ViewMode::Raw => {
                    // Show hexdump of the selected raw section.
                    let mut lines = Vec::new();
                    if let Some(block) = app.overview_raw_preview() {
                        let width = content_width(area);
                        lines.push(Line::from(Span::styled(
                            truncate_text(&block.title, width),
                            theme::title(),
                        )));
                        lines.push(Line::from(""));
                        lines.extend(block.rows.into_iter().map(render_hexdump_row));
                    } else {
                        lines.push(Line::from("No section selected"));
                    }
                    let p = Paragraph::new(lines)
                        .block(
                            Block::default()
                                .title("Preview")
                                .title_style(theme::title())
                                .borders(Borders::ALL)
                                .border_style(theme::border(false)),
                        )
                        .wrap(Wrap { trim: false });
                    frame.render_widget(p, area);
                }
                crate::ViewMode::Structured => {
                    // Show entity list for the selected section kind.
                    let kind = app.overview_structural_preview_section();
                    let entries = app.section_entries(kind);
                    let width = content_width(area);
                    let lines: Vec<Line<'static>> = if entries.is_empty() {
                        vec![Line::from("No entries")]
                    } else {
                        entries
                            .iter()
                            .map(|entry| {
                                Line::from(Span::styled(
                                    truncate_text(&format!("  {}", entry.label), width),
                                    theme::accent(entry.accent),
                                ))
                            })
                            .collect()
                    };
                    let p = Paragraph::new(lines)
                        .block(
                            Block::default()
                                .title(format!("Preview — {}", kind.title()))
                                .title_style(theme::title())
                                .borders(Borders::ALL)
                                .border_style(theme::border(false)),
                        )
                        .wrap(Wrap { trim: false });
                    frame.render_widget(p, area);
                }
            }
        }
        Scene::SectionDetail(kind) => {
            // Preview shows Detail content for the currently selected entry.
            let idx = app.section_selected();
            match app.mode() {
                crate::ViewMode::Raw => {
                    scenes::detail::render_raw_block(frame, area, app, *kind, idx);
                }
                crate::ViewMode::Structured => {
                    scenes::detail::render_structured_detail(frame, area, app, *kind, idx);
                }
            }
        }
        Scene::Detail(_, _) => {} // no preview for Detail (leaf)
    }
}

fn render_status(app: &App) -> Paragraph<'static> {
    let summary = app.summary();
    let size_str = format_size_len(summary.file_size);
    let mut spans = if let Some(error) = &summary.validation_error {
        vec![
            Span::styled("INVALID ", theme::status_error()),
            Span::raw(format!(
                "{}  size: {}  |  error: {}",
                app.path().display(),
                size_str,
                error
            )),
        ]
    } else {
        vec![
            Span::styled("VALID ", theme::status_ok()),
            Span::raw(format!("{}  size: {}", app.path().display(), size_str)),
        ]
    };

    if let Some(notice) = app.status_notice() {
        spans.push(Span::raw("  |  "));
        spans.push(Span::styled(notice, theme::accent(crate::Accent::Warning)));
    }

    Paragraph::new(Line::from(spans)).alignment(Alignment::Left)
}

fn render_tabs(scene: &Scene) -> Tabs<'static> {
    let titles = ["Overview", "Section", "Detail"]
        .into_iter()
        .map(Line::from)
        .collect::<Vec<_>>();

    Tabs::new(titles)
        .select(scene.tab_index())
        .highlight_style(theme::selection())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border(true)),
        )
}

fn centered_rect(area: Rect, width_percent: u16, height_percent: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height_percent) / 2),
            Constraint::Percentage(height_percent),
            Constraint::Percentage((100 - height_percent) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_percent) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Percentage((100 - width_percent) / 2),
        ])
        .split(vertical[1])[1]
}
