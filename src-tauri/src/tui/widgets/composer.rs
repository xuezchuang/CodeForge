//! Composer widget: multi-line input box at the bottom of the screen.
//!
//! Unlike the legacy crossterm-based composer, this widget is fully driven
//! by ratatui `Frame` and stores its state in the parent `App`. The widget
//! itself only renders; key handling lives in `App::handle_key`.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use ratatui::Frame;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ComposerMode {
    #[default]
    Editing,
    Submitted,
    /// A slash command was submitted from the popup.
    Command,
}

#[derive(Clone, Debug, Default)]
pub struct ComposerState {
    pub text: String,
    pub cursor: usize,
    pub mode: ComposerMode,
}

pub enum ComposerOutcome {
    /// Continue editing.
    KeepEditing,
    /// User pressed Enter on plain text; the caller should send `state.text`
    /// to the agent runner.
    Submit(String),
    /// User submitted a slash command; the caller dispatches it.
    SubmitCommand(String),
    /// User pressed Ctrl-C / Esc; caller should exit.
    Cancel,
}

pub struct ComposerWidget;

impl ComposerWidget {
    /// Minimum number of rows the composer takes.
    pub const MIN_ROWS: u16 = 2;
    /// Maximum number of rows the composer can grow to.
    pub const MAX_ROWS: u16 = 8;

    /// Compute the number of rows the composer needs for the current text
    /// and the given width, capped between `MIN_ROWS` and `MAX_ROWS`.
    pub fn height_for(width: u16, text: &str) -> u16 {
        let width = width.max(1) as usize;
        let line_count = textwrap::count_lines(text, width.saturating_sub(2).max(1)) as u16;
        (line_count + 2).max(Self::MIN_ROWS).min(Self::MAX_ROWS)
    }

    pub fn render(
        state: &ComposerState,
        frame: &mut Frame,
        area: Rect,
        prompt: &str,
    ) -> (u16, u16) {
        if area.height == 0 || area.width == 0 {
            return (0, 0);
        }
        frame.render_widget(Clear, area);

        let width = area.width.max(1) as usize;
        let body = build_prompt_lines(&state.text, state.cursor, width, prompt);
        let paragraph = Paragraph::new(body).wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);

        cursor_position(&state.text, state.cursor, width, prompt, area)
    }
}

fn build_prompt_lines(text: &str, cursor: usize, width: usize, prompt: &str) -> Vec<Line<'static>> {
    let _ = cursor; // cursor is handled by cursor_position(); the body just renders text
    let prompt = truncate_to(prompt, width);
    let indent = " ".repeat(display_width(&prompt));
    let mut out: Vec<Line<'static>> = Vec::new();
    if text.is_empty() {
        return vec![Line::from(vec![
            Span::styled(prompt, Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(
                "Ask CodeForge to do anything",
                Style::default().fg(Color::DarkGray),
            ),
        ])];
    }

    let mut current = prompt.to_string();
    let mut current_width = display_width(&prompt);
    for ch in text.chars() {
        let w = char_width(ch);
        if current_width > indent.len() && current_width + w > width {
            out.push(Line::from(Span::raw(current)));
            current = indent.clone();
            current_width = indent.len();
        }
        current.push(ch);
        current_width += w;
    }
    out.push(Line::from(vec![
        Span::styled(
            prompt.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(
            current
                .chars()
                .skip(prompt.chars().count())
                .collect::<String>(),
        ),
    ]));
    out
}

fn cursor_position(
    text: &str,
    cursor: usize,
    width: usize,
    prompt: &str,
    inner: Rect,
) -> (u16, u16) {
    let prompt_width = display_width(prompt);
    let indent = " ".repeat(prompt_width);
    let mut col = prompt_width;
    let mut row: u16 = 0;
    let mut current_width = prompt_width;
    for (i, ch) in text.chars().enumerate() {
        if i == cursor {
            break;
        }
        let w = char_width(ch);
        if current_width > indent.len() && current_width + w > width {
            row = row.saturating_add(1);
            col = indent.len();
            current_width = indent.len();
        }
        col += w;
        current_width += w;
    }
    let clamped_col = (col as u16).min(inner.width.saturating_sub(1));
    let clamped_row = row.min(inner.height.saturating_sub(1));
    (
        inner.x.saturating_add(clamped_col),
        inner.y.saturating_add(clamped_row),
    )
}

fn display_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

fn truncate_to(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = char_width(ch);
        if used + w > max {
            break;
        }
        out.push(ch);
        used += w;
    }
    out
}

fn char_width(ch: char) -> usize {
    if ch.is_control() {
        0
    } else {
        1
    }
}

/// Tiny inline copy of textwrap's line counter so we don't take a
/// dependency on the `textwrap` crate for one function.
mod textwrap {
    pub fn count_lines(text: &str, width: usize) -> usize {
        let width = width.max(1);
        let mut lines = 1usize;
        let mut col = 0usize;
        for ch in text.chars() {
            let w = if ch.is_control() { 0 } else { 1 };
            if ch == '\n' {
                lines += 1;
                col = 0;
                continue;
            }
            if col > 0 && col + w > width {
                lines += 1;
                col = w;
            } else {
                col += w;
            }
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn height_for_short_text_uses_minimum() {
        let h = ComposerWidget::height_for(80, "hi");
        assert!(h >= ComposerWidget::MIN_ROWS);
    }

    #[test]
    fn height_for_long_text_grows() {
        let h = ComposerWidget::height_for(20, "this is a long line that should wrap");
        assert!(h >= ComposerWidget::MIN_ROWS);
    }

    #[test]
    fn height_caps_at_max() {
        let h = ComposerWidget::height_for(
            10,
            "alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\neta\ntheta\niota\nkappa",
        );
        assert!(h <= ComposerWidget::MAX_ROWS);
    }
}
