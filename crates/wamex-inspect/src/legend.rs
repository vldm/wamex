use ratatui::text::{Line, Span};

use crate::{App, scene::{Scene, SectionDetailMode}, theme};

pub fn footer(app: &App) -> Line<'static> {
    let scene = app.current_scene();
    let mut spans = vec![
        Span::raw("q quit  "),
        Span::raw("← → scenes  "),
        Span::raw("Enter drill  "),
        Span::raw("Esc back  "),
        Span::raw("? help  "),
        Span::raw("↑↓ move  PgUp/PgDn scroll"),
    ];

    if matches!(scene, Scene::SectionDetail(_)) {
        spans.extend([
            Span::raw("  |  Tab mode: "),
            Span::styled(app.section_mode().title(), theme::selection()),
        ]);
    }

    if matches!(scene, Scene::SectionDetail(_))
        && app.section_mode() == SectionDetailMode::StructuredDetailed
    {
        spans.extend([
            Span::raw("  |  "),
            Span::styled(
                "GOT",
                theme::reloc(wamex_object::linkage::reloc::Relative::Got),
            ),
            Span::raw("  "),
            Span::styled(
                "TLS",
                theme::reloc(wamex_object::linkage::reloc::Relative::Tls),
            ),
            Span::raw("  "),
            Span::styled(
                "LocRel",
                theme::reloc(wamex_object::linkage::reloc::Relative::LocRel),
            ),
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
        Line::from("Tab cycles section mode: raw, structured short, structured detailed."),
        Line::from("Enter from Overall opens selected section."),
        Line::from("Enter from Section jumps to related section or opens detailed view."),
        Line::from("Esc or Backspace returns to previous drill-in scene."),
        Line::from("Structured detailed view embeds byte dump and relocation links."),
    ]
}
