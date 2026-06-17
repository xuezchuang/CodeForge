//! Transcript widget: scrollable area that shows the last N user/assistant
//! messages. The widget itself does not own the data; the App passes in the
//! full transcript list on every render and the widget slices the tail that
//! fits the visible area.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use crate::agent_runner::AgentConversationMessage;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscriptRole {
    User,
    Assistant,
}

#[derive(Clone, Debug)]
pub struct TranscriptLine {
    pub role: TranscriptRole,
    pub text: String,
}

impl TranscriptLine {
    pub fn from_message(message: &AgentConversationMessage) -> Option<Self> {
        let text = message.content.trim();
        if text.is_empty() {
            return None;
        }
        let role = match message.role.as_str() {
            "user" => TranscriptRole::User,
            "assistant" => TranscriptRole::Assistant,
            _ => TranscriptRole::User,
        };
        Some(Self {
            role,
            text: text.to_string(),
        })
    }
}

pub struct TranscriptWidget;

impl TranscriptWidget {
    pub fn render(lines: &[TranscriptLine], frame: &mut Frame, area: Rect) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let width = area.width.max(1) as usize;
        let mut rendered: Vec<Line<'static>> = Vec::new();
        for line in lines {
            rendered.extend(wrap_line(line, width));
        }
        let total = rendered.len();
        let visible = area.height as usize;
        let start = total.saturating_sub(visible);
        let paragraph = Paragraph::new(rendered[start..].to_vec()).wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);
    }
}

fn wrap_line(line: &TranscriptLine, width: usize) -> Vec<Line<'static>> {
    let prefix = match line.role {
        TranscriptRole::User => "\u{203a}",
        TranscriptRole::Assistant => "\u{2022}",
    };
    let prefix_color = match line.role {
        TranscriptRole::User => Color::Cyan,
        TranscriptRole::Assistant => Color::DarkGray,
    };
    let prefix_width = prefix.chars().count();
    let body = line.text.as_str();
    let wrapped = wrap_text(body, width.saturating_sub(prefix_width + 1).max(1));
    let mut out = Vec::new();
    for (index, segment) in wrapped.into_iter().enumerate() {
        let mut spans = Vec::new();
        if index == 0 {
            spans.push(Span::styled(
                format!("{prefix} "),
                Style::default()
                    .fg(prefix_color)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::raw(" ".repeat(prefix_width + 1)));
        }
        spans.push(Span::styled(segment, Style::default().fg(Color::White)));
        out.push(Line::from(spans));
    }
    if out.is_empty() {
        out.push(Line::from(Span::raw(prefix)));
    }
    out
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for ch in text.chars() {
        let w = if ch.is_control() { 0 } else { 1 };
        if ch == '\n' {
            out.push(std::mem::take(&mut current));
            current_width = 0;
            continue;
        }
        if current_width > 0 && current_width + w > width && !current.is_empty() {
            out.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push(ch);
        current_width += w;
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}
