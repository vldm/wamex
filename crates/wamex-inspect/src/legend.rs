use ratatui::text::{Line, Span};

use crate::{App, scene::Scene, theme};

pub fn footer(app: &App) -> Line<'static> {
    let scene = app.current_scene();
    let mut spans = vec![
        Span::raw("q quit  "),
        Span::raw("← → scenes  "),
        Span::raw("Enter drill  "),
        Span::raw("Esc back  "),
        Span::raw("? help  "),
        Span::raw("↑↓ move"),
    ];

    if matches!(scene, Scene::SectionDetail(_)) {
        spans.extend([
            Span::raw("  |  Tab mode: "),
            Span::styled(app.section_mode().title(), theme::selection()),
        ]);
    }

    Line::from(spans)
}

pub fn help() -> Vec<Line<'static>> {
    vec![
        Line::from("1 Overall view"),
        Line::from("2 Section detail"),
        Line::from(""),
        Line::from("Left/Right switch top-level scenes."),
        Line::from("Tab cycles section mode: raw, structured."),
        Line::from("Enter from Overall opens selected section."),
        Line::from("Enter from Structured jumps to related section when available."),
        Line::from("Esc or Backspace returns to previous drill-in scene."),
        Line::from("Raw mode shows the current section's raw blocks with preview."),
        Line::from("Structured mode shows the current section's entries with preview."),
    ]
}
