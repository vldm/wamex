use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyEvent};

pub enum AppEvent {
    Key(KeyEvent),
    Resize,
    Tick,
}

pub fn next_event(timeout: Duration) -> Result<AppEvent> {
    if !event::poll(timeout)? {
        return Ok(AppEvent::Tick);
    }

    Ok(match event::read()? {
        Event::Key(key) => AppEvent::Key(key),
        Event::Resize(_, _) => AppEvent::Resize,
        _ => AppEvent::Tick,
    })
}
