//! CodeForge TUI (terminal user interface).
//!
//! Built on top of ratatui so the CodeForge CLI can present a Codex-style
//! chat surface without copying codex's full chatwidget implementation.
//! The actual session/agent logic stays in `cli::run_chat` and
//! `agent_runner`; this module only owns the rendering and input layer.

pub mod app;
pub mod terminal;
pub mod widgets;

pub use app::{run_tui, App, AppEvent, AppOutcome};
