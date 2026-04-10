use ratatui::{
    layout::{Constraint, Direction, Layout},
    prelude::*,
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::{App, theme};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(8)])
        .split(area);

    let summary = app.summary();
    let meta = vec![
        Line::from(format!("file: {}", app.path().display())),
        Line::from(format!("size: {} bytes", summary.file_size)),
        Line::from(format!("features: {}", summary.target_features)),
    ];

    let meta_widget = Paragraph::new(meta)
        .block(
            Block::default()
                .title("Metadata")
                .title_style(theme::title())
                .borders(Borders::ALL)
                .border_style(theme::border(false)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(meta_widget, chunks[0]);

    let section_lines = app
        .summary()
        .section_rows
        .iter()
        .enumerate()
        .map(|(idx, row)| {
            let style = if idx == app.overall_selected() {
                theme::selection()
            } else {
                Style::default().fg(Color::White)
            };

            Line::from(vec![
                Span::styled(
                    format!("[{:>5}] {:<10}", row.kind.canonical_label(), row.title),
                    style,
                ),
                Span::styled(format!(" {:>5}  ", row.count), style),
                Span::styled(row.note.clone(), style),
            ])
        })
        .collect::<Vec<_>>();

    let section_widget = Paragraph::new(section_lines)
        .block(
            Block::default()
                .title("Sections")
                .title_style(theme::title())
                .borders(Borders::ALL)
                .border_style(theme::border(true)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(section_widget, chunks[1]);
}
