//! Footer / status line: model + context + goal indicator + workspace.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::goal_state::GoalState;

#[derive(Clone, Debug, Default)]
pub struct FooterStatus {
    pub model: String,
    pub reasoning: String,
    pub context_remaining: String,
    pub workspace: String,
    pub goal: Option<GoalState>,
    pub task_running: bool,
    pub elapsed_seconds: u64,
}

pub struct FooterWidget;

impl FooterWidget {
    pub fn render(status: &FooterStatus, frame: &mut Frame, area: Rect) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let context = if status.context_remaining.is_empty() {
            "100% context left".to_string()
        } else {
            status.context_remaining.clone()
        };

        let goal_segment = match &status.goal {
            Some(goal) => {
                let elapsed = GoalState::format_elapsed(goal.time_used_seconds);
                format!("goal [{}] {}", goal.status.label(), elapsed)
            }
            None => String::new(),
        };

        let left = if status.task_running {
            format!("Esc to interrupt · working {}s", status.elapsed_seconds)
        } else if !goal_segment.is_empty() {
            goal_segment
        } else {
            "? for shortcuts".to_string()
        };

        let line_text = fit_footer(&left, &context, area.width as usize);
        let left_span = Span::styled(
            line_text,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        );
        let line = Line::from(vec![left_span]);
        frame.render_widget(Clear, area);
        frame.render_widget(Paragraph::new(line), area);
    }
}

fn fit_footer(left: &str, right: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let left = left.trim();
    let right = right.trim();
    if right.is_empty() {
        return truncate_to_width(&format!(" {left}"), width);
    }
    let gap = 2usize;
    let left_width = display_width(left);
    let right_width = display_width(right);
    if left_width + right_width + gap + 2 <= width {
        let space_count = width.saturating_sub(left_width + right_width + 1);
        return format!(" {left}{}{}", " ".repeat(space_count), right);
    }
    let right_budget = width.saturating_div(2).min(right_width).max(8);
    let right_text = truncate_to_width(right, right_budget);
    let left_budget = width.saturating_sub(display_width(&right_text) + gap + 1);
    let left_text = truncate_to_width(left, left_budget);
    format!(" {left_text}{}{}", " ".repeat(gap), right_text)
}

fn truncate_to_width(text: &str, max_width: usize) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let width = char_width(ch);
        if used + width > max_width {
            break;
        }
        out.push(ch);
        used += width;
    }
    out
}

fn display_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

fn char_width(ch: char) -> usize {
    if ch.is_control() {
        0
    } else {
        1
    }
}
