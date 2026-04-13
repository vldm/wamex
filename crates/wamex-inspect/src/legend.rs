use ratatui::text::{Line, Span};

use crate::{App, scene::Scene, theme};

pub fn footer(app: &App) -> Line<'static> {
    let scene = app.current_scene();
    let mode_label = match scene {
        Scene::OverallView => app.overall_mode().title(),
        Scene::SectionDetail(_) | Scene::Detail(_, _) => app.section_mode().title(),
    };
    let preview_label = if app.show_preview() { "on" } else { "off" };
    let spans = vec![
        Span::raw("q quit  "),
        Span::raw("← → scenes  "),
        Span::raw("Enter drill  "),
        Span::raw("Esc back  "),
        Span::raw("? help  "),
        Span::raw("↑↓ move  "),
        Span::raw("s mode: "),
        Span::styled(mode_label, theme::selection()),
        Span::raw("  p Preview: "),
        Span::styled(preview_label, theme::selection()),
    ];

    Line::from(spans)
}

pub fn help() -> Vec<Line<'static>> {
    vec![
        Line::from("1 Overview  2 Section  3 Detail"),
        Line::from(""),
        Line::from("Left/Right switch top-level scenes."),
        Line::from("s toggles mode (Raw/Structured) in Overview and Section views."),
        Line::from("p toggles the preview pane on/off."),
        Line::from("Enter from Overview opens selected section."),
        Line::from("Enter from Section drills into Detail for the selected entry."),
        Line::from("Esc or Backspace returns to previous scene."),
        Line::from("Raw mode shows raw bytes; Structured shows semantic entities."),
        Line::from("Preview pane shows one level deeper (Overview→Section, Section→Detail)."),
    ]
}
