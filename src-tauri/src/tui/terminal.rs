//! ratatui Terminal setup, including alternate screen + raw mode lifecycle.
//!
//! CodeForge intentionally reuses the existing crossterm 0.28 stack to
//! keep raw-mode handling identical to the legacy codex-style CLI. ratatui
//! 0.29 ships with a `CrosstermBackend` that drives the same underlying
//! primitives.

use std::io::{self, Stdout};

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::Terminal;

/// Owns the ratatui `Terminal` and restores the terminal on drop.
pub struct TuiGuard {
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TuiGuard {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;
        Ok(Self { terminal })
    }
}

impl Drop for TuiGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}
