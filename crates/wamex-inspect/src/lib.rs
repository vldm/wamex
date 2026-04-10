use std::{
    io::{self, Stdout},
    path::PathBuf,
    time::Duration,
};

use anyhow::Result;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

mod app;
mod event;
mod legend;
mod scene;
mod scenes;
mod theme;
mod ui;

pub use app::{
    Accent, App, DetailView, HexdumpRow, ListEntry, RawBlockView, RawSummary, RelocationLine,
};
pub use scene::{InspectTarget, Scene, SectionDetailMode, SectionKind};

pub fn run_path(path: PathBuf) -> Result<()> {
    let mut app = App::load(path)?;
    run_tui(&mut app)
}

pub fn run_tui(app: &mut App) -> Result<()> {
    let mut terminal = setup_terminal()?;

    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        if app.should_quit() {
            break;
        }

        match event::next_event(Duration::from_millis(250))? {
            event::AppEvent::Key(key) => app.handle_key(key),
            event::AppEvent::Resize | event::AppEvent::Tick => {}
        }
    }

    restore_terminal(terminal)
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;

    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn restore_terminal(mut terminal: Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}
