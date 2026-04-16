use ratatui::style::{Color, Modifier, Style};
use wamex_object::linkage::reloc::Relative;

use crate::scenes::section_detail_state::Accent;

pub fn title() -> Style {
    Style::default()
        .fg(Color::LightYellow)
        .add_modifier(Modifier::BOLD)
}

pub fn border(active: bool) -> Style {
    if active {
        Style::default().fg(Color::LightCyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

pub fn selection() -> Style {
    Style::default()
        .bg(Color::Rgb(28, 52, 80))
        .fg(Color::White)
        .add_modifier(Modifier::BOLD)
}

pub fn accent(accent: Accent) -> Style {
    match accent {
        Accent::Normal => Style::default().fg(Color::White),
        Accent::Import => Style::default().fg(Color::Cyan),
        Accent::Export => Style::default().fg(Color::Green),
        Accent::Error => Style::default()
            .fg(Color::LightRed)
            .add_modifier(Modifier::BOLD),
        Accent::Muted => Style::default().fg(Color::Gray),
        Accent::Warning => Style::default().fg(Color::Yellow),
    }
}

pub fn reloc(relative: Relative) -> Style {
    match relative {
        Relative::None => Style::default().fg(Color::White),
        Relative::Got => Style::default().fg(Color::Yellow),
        Relative::Tls => Style::default().fg(Color::Magenta),
        Relative::LocRel => Style::default().fg(Color::LightBlue),
    }
}

pub fn ascii(rel: Option<Relative>) -> Style {
    match rel {
        Some(relative) => reloc(relative),
        None => Style::default().fg(Color::Gray),
    }
}

pub fn status_error() -> Style {
    Style::default()
        .fg(Color::LightRed)
        .add_modifier(Modifier::BOLD)
}

pub fn status_ok() -> Style {
    Style::default().fg(Color::Green)
}
