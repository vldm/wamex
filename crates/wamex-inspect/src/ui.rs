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

    let tabs = render_tabs(app);
    frame.render_widget(tabs, chunks[1]);

    // Split body area into main pane + optional preview pane.
    // Hide preview for Detail (leaf) and for raw SectionDetail (it IS the hexdump).
    let hide_preview = matches!(app.current_scene(), Scene::Detail(_, _))
        || matches!(
            (app.current_scene(), app.mode()),
            (Scene::SectionDetail(_), crate::ViewMode::Raw)
        );
    let (main_area, preview_area) = if app.show_preview() && !hide_preview {
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
    use crate::scenes::helpers::{content_width, render_semantic_dump, truncate_text};

    match app.current_scene() {
        Scene::OverallView => {
            // Preview shows the SectionDetail content for the highlighted section,
            // preserving the section_detail scroll / selection state so that
            // go_back doesn't lose the user's position.
            match app.mode() {
                crate::ViewMode::Raw => {
                    // Show hexdump of the selected raw section, scrolled to match
                    // where the user was (or left off) in raw SectionDetail.
                    let scroll = app.section_scroll();
                    let mut lines = Vec::new();
                    if let Some(block) = app.overview_raw_preview() {
                        let width = content_width(area);
                        lines.push(Line::from(Span::styled(
                            truncate_text(&block.title, width),
                            theme::title(),
                        )));
                        lines.push(Line::from(""));
                        lines.extend(render_semantic_dump(&block.dump));
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
                        .scroll((scroll as u16, 0))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(p, area);
                }
                crate::ViewMode::Structured => {
                    // Show entry list for the selected section kind, scrolled to match
                    // the section_detail selection state.
                    let kind = app.overview_structural_preview_section();
                    let entries = app.section_entries(kind);
                    let width = content_width(area);
                    use crate::scenes::helpers::content_height;
                    let viewport = content_height(area);
                    app.set_section_viewport(viewport);
                    let scroll = app.section_scroll();
                    let end = (scroll + viewport).min(entries.len());
                    let lines: Vec<Line<'static>> = if entries.is_empty() {
                        vec![Line::from("No entries")]
                    } else {
                        entries
                            .iter()
                            .enumerate()
                            .skip(scroll)
                            .take(end.saturating_sub(scroll))
                            .map(|(idx, entry)| {
                                let style = if idx == app.section_selected() {
                                    theme::selection()
                                } else {
                                    theme::accent(entry.accent)
                                };
                                Line::from(Span::styled(
                                    truncate_text(&format!("  {}", entry.label), width),
                                    style,
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
            // Raw SectionDetail has no preview (the whole pane IS the hexdump).
            // Only structured mode shows a detail preview.
            if app.mode() == crate::ViewMode::Structured {
                let idx = app.section_selected();
                scenes::detail::render_structured_detail(frame, area, app, *kind, idx, 0);
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

fn render_tabs(app: &App) -> Tabs<'static> {
    // Detail tab is greyed out only in Raw mode — no disassembly there yet.
    let detail_style = if app.mode() == crate::ViewMode::Raw {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default()
    };
    let titles = vec![
        Line::from("Overview"),
        Line::from("Section"),
        Line::from(Span::styled("Detail", detail_style)),
    ];

    Tabs::new(titles)
        .select(app.current_scene().tab_index())
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
