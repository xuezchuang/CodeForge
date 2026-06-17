//! Slash-command autocomplete popup. Mirrors the codex TUI popup: a vertical
//! list above the composer with the currently selected row highlighted.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState};
use ratatui::Frame;

#[derive(Clone, Debug)]
pub struct SlashEntry {
    pub name: &'static str,
    pub description: &'static str,
}

pub struct SlashPopupWidget;

impl SlashPopupWidget {
    /// Maximum number of popup rows to show at once.
    pub const MAX_ROWS: usize = 8;

    /// Filter the built-in command list by the current composer input.
    pub fn filter(prefix: &str, commands: &[SlashEntry]) -> Vec<SlashEntry> {
        commands
            .iter()
            .filter(|entry| entry.name.starts_with(prefix))
            .cloned()
            .collect()
    }

    /// Pick the entry that the user has highlighted, if any.
    pub fn selected<'a>(
        matches: &'a [SlashEntry],
        selected_index: usize,
    ) -> Option<&'a SlashEntry> {
        matches.get(selected_index)
    }

    pub fn render(matches: &[SlashEntry], selected_index: usize, frame: &mut Frame, area: Rect) {
        if matches.is_empty() || area.height == 0 {
            return;
        }
        frame.render_widget(Clear, area);

        let items: Vec<ListItem> = matches
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let marker = if index == selected_index { "›" } else { " " };
                let name_style = if index == selected_index {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                let desc_style = if index == selected_index {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM)
                };
                let name = format!("{:<8}", entry.name);
                ListItem::new(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(format!("{marker} "), name_style),
                    Span::styled(name, name_style),
                    Span::styled(format!(" {}", entry.description), desc_style),
                ]))
            })
            .collect();

        let mut state = ListState::default();
        if !matches.is_empty() {
            state.select(Some(selected_index.min(matches.len().saturating_sub(1))));
        }
        let list = List::new(items).highlight_symbol("").highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
        frame.render_stateful_widget(list, area, &mut state);
    }
}
