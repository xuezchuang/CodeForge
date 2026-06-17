//! Header box widget shown for an empty/fresh chat, matching Codex's
//! startup header.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

/// Inputs needed to render the header.
#[derive(Clone, Debug, Default)]
pub struct HeaderState {
    pub title: String,
    pub model_line: String,
    pub directory_line: String,
    pub version: String,
}

pub struct HeaderWidget;

impl HeaderWidget {
    /// Height in terminal rows that the header occupies when visible.
    pub const HEIGHT: u16 = 6;

    pub fn render(state: &HeaderState, frame: &mut Frame, area: Rect) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let title = if state.title.is_empty() {
            format!(">_ CodeForge Codex (v{})", state.version)
        } else {
            state.title.clone()
        };
        let model_line = if state.model_line.is_empty() {
            "model:     (auto)   /model to change".to_string()
        } else {
            state.model_line.clone()
        };
        let directory_line = if state.directory_line.is_empty() {
            "directory: (unknown)".to_string()
        } else {
            state.directory_line.clone()
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .style(Style::default().bg(Color::Reset));

        let inner = block.inner(area);
        frame.render_widget(Clear, area);
        frame.render_widget(block, area);

        let lines = vec![
            Line::from(Span::styled(
                title,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(model_line, Style::default().fg(Color::Gray))),
            Line::from(Span::styled(
                directory_line,
                Style::default().fg(Color::Gray),
            )),
        ];

        let content_area = Rect {
            x: inner.x.saturating_add(1),
            y: inner.y,
            width: inner.width.saturating_sub(2),
            height: inner.height,
        };
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }),
            content_area,
        );
    }
}
