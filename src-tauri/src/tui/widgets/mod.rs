//! Reusable ratatui widgets for the CodeForge TUI.

pub mod composer;
pub mod footer;
pub mod header;
pub mod slash_popup;
pub mod transcript;

pub use composer::{ComposerMode, ComposerOutcome, ComposerWidget};
pub use footer::{FooterStatus, FooterWidget};
pub use header::HeaderWidget;
pub use slash_popup::SlashPopupWidget;
pub use transcript::{TranscriptLine, TranscriptRole, TranscriptWidget};
