use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    prelude::*,
    widgets::{Block, Borders, Clear, Paragraph, Tabs, Wrap},
};

use crate::{App, legend, scene::Scene, scenes, theme};

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

    match app.current_scene() {
        Scene::OverallView => scenes::overall::render(frame, chunks[2], app),
        Scene::SectionDetail(kind) => scenes::section_detail::render(frame, chunks[2], app, *kind),
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

fn render_status(app: &App) -> Paragraph<'static> {
    let summary = app.summary();
    let scene = app.current_scene().header_title();
    let spans = if let Some(error) = &summary.validation_error {
        vec![
            Span::styled("INVALID ", theme::status_error()),
            Span::raw(format!("{}  |  {}", scene, error)),
        ]
    } else {
        vec![
            Span::styled("VALID ", theme::status_ok()),
            Span::raw(format!("{}  |  {}", scene, app.path().display())),
        ]
    };

    Paragraph::new(Line::from(spans)).alignment(Alignment::Left)
}

fn render_tabs(scene: &Scene) -> Tabs<'static> {
    let titles = ["Overall", "Section"]
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
