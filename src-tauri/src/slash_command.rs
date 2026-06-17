//! CodeForge-owned slash command registry.
//!
//! This module defines typed slash commands with names, descriptions, argument
//! parsers, and handlers. It replaces the ad-hoc string-matching approach that
//! was scattered through the CLI input loop.

use std::fmt;
use std::io::Write;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// SlashCommand enum
// ---------------------------------------------------------------------------

/// Built-in slash commands recognized by the CodeForge CLI.
///
/// Enum order is presentation order in the popup, so more frequently used
/// commands should be listed first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SlashCommand {
    /// Show all available commands.
    Help,
    /// Show model, provider, directory, workspace, permissions, account, and usage/limits.
    Status,
    /// Manage the active goal: show, set, clear, pause, resume.
    Goal(GoalSubcommand),
    /// Choose model and reasoning/thinking.
    Model,
    /// Choose reasoning/thinking.
    Reason,
    /// Clear the terminal.
    Clear,
    /// Start a new chat and clear conversation context.
    New,
    /// Quit the CLI.
    Quit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GoalSubcommand {
    /// Show the current goal (also /goal with no arguments).
    Show,
    /// Set or replace the goal objective.
    Set { objective: String },
    /// Clear the current goal.
    Clear,
    /// Pause the current goal.
    Pause,
    /// Resume a paused goal.
    Resume,
}

// ---------------------------------------------------------------------------
// Command metadata
// ---------------------------------------------------------------------------

impl SlashCommand {
    /// The slash-prefixed name the user types, e.g. /help.
    pub fn command_name(&self) -> &'static str {
        match self {
            SlashCommand::Help => "/help",
            SlashCommand::Status => "/status",
            SlashCommand::Goal(_) => "/goal",
            SlashCommand::Model => "/model",
            SlashCommand::Reason => "/reason",
            SlashCommand::Clear => "/clear",
            SlashCommand::New => "/new",
            SlashCommand::Quit => "/quit",
        }
    }

    /// One-line description shown in the command popup.
    pub fn description(&self) -> &'static str {
        match self {
            SlashCommand::Help => "show commands",
            SlashCommand::Status => "show model, provider, directory, workspace, permissions, account, session, agents.md, and usage/limits",
            SlashCommand::Goal(_) => "manage the active goal (show, set, clear, pause, resume)",
            SlashCommand::Model => "choose model and reasoning/thinking",
            SlashCommand::Reason => "choose reasoning/thinking",
            SlashCommand::Clear => "clear the terminal",
            SlashCommand::New => "start a new chat and clear conversation context",
            SlashCommand::Quit => "quit",
        }
    }

    /// Whether this command is available while a task is running.
    pub fn available_during_task(&self) -> bool {
        matches!(self, SlashCommand::Help | SlashCommand::Quit)
    }
}

impl fmt::Display for SlashCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.command_name())
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse a raw input line into a SlashCommand, if it starts with /.
///
/// Returns None if the input does not start with /.
/// Returns Some(Err(..)) if the command is recognized but arguments are invalid.
/// Returns Some(Ok(..)) for valid commands.
pub fn parse_slash_command(input: &str) -> Option<Result<SlashCommand, String>> {
    let input = input.trim();
    if !input.starts_with('/') {
        return None;
    }

    // Split into command word and the rest.
    let (command_word, rest) = match input.find(' ') {
        Some(index) => (&input[..index], input[index + 1..].trim()),
        None => (input, ""),
    };

    match command_word {
        "/" => {
            // Bare "/" is an alias for /help
            Some(Ok(SlashCommand::Help))
        }
        "/help" | "/h" => Some(Ok(SlashCommand::Help)),
        "/status" => Some(Ok(SlashCommand::Status)),
        "/model" | "/models" => Some(Ok(SlashCommand::Model)),
        "/reason" | "/reasoning" | "/reasion" => Some(Ok(SlashCommand::Reason)),
        "/clear" => Some(Ok(SlashCommand::Clear)),
        "/new" => Some(Ok(SlashCommand::New)),
        "/exit" | "/quit" => Some(Ok(SlashCommand::Quit)),
        "/goal" => {
            let sub = if rest.is_empty() {
                GoalSubcommand::Show
            } else {
                let (sub_word, sub_rest) = match rest.find(' ') {
                    Some(index) => (&rest[..index], rest[index + 1..].trim()),
                    None => (rest, ""),
                };
                match sub_word {
                    "set" if !sub_rest.is_empty() => GoalSubcommand::Set {
                        objective: sub_rest.to_string(),
                    },
                    "set" => {
                        return Some(Err("Usage: /goal set <objective>".to_string()));
                    }
                    "clear" => GoalSubcommand::Clear,
                    "pause" => GoalSubcommand::Pause,
                    "resume" => GoalSubcommand::Resume,
                    // If the user types /goal some text, treat it as /goal set some text.
                    other => GoalSubcommand::Set {
                        objective: if sub_rest.is_empty() {
                            other.to_string()
                        } else {
                            format!("{other} {sub_rest}")
                        },
                    },
                }
            };
            Some(Ok(SlashCommand::Goal(sub)))
        }
        other => {
            // Check for prefix matches
            let matches = BUILTIN_COMMANDS
                .iter()
                .filter(|(name, _)| name.starts_with(other))
                .collect::<Vec<_>>();
            if matches.len() == 1 {
                // Re-parse with the full match
                parse_slash_command(matches[0].0)
            } else if matches.is_empty() {
                None
            } else {
                Some(Err(format!(
                    "Ambiguous command '{}'. Matching: {}",
                    other,
                    matches
                        .iter()
                        .map(|(n, _)| *n)
                        .collect::<Vec<_>>()
                        .join(", ")
                )))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Command list for popups
// ---------------------------------------------------------------------------

/// Built-in commands in display order.
const BUILTIN_COMMANDS: &[(&str, &str)] = &[
    ("/new", "start a new chat and clear conversation context"),
    ("/model", "choose model and reasoning/thinking"),
    ("/reason", "choose reasoning/thinking"),
    ("/goal", "manage the active goal (show, set, clear, pause, resume)"),
    ("/status", "show model, provider, directory, workspace, permissions, account, session, agents.md, and usage/limits"),
    ("/clear", "clear the terminal"),
    ("/help", "show commands"),
    ("/quit", "quit"),
];

/// Return all built-in slash commands as (name, description) pairs.
pub fn builtin_slash_commands() -> &'static [(&'static str, &'static str)] {
    BUILTIN_COMMANDS
}

/// Return the subset of built-in commands that match prefix.
pub fn slash_command_matches(prefix: &str) -> Vec<(&'static str, &'static str)> {
    BUILTIN_COMMANDS
        .iter()
        .filter(|(name, _)| name.starts_with(prefix))
        .copied()
        .collect()
}

/// Number of built-in commands matching prefix, or None if prefix is
/// not a slash prefix at all.
pub fn slash_command_match_count(prefix: &str) -> Option<usize> {
    if !prefix.starts_with('/') {
        return None;
    }
    let count = slash_command_matches(prefix).len();
    (count > 0).then_some(count)
}

/// Return the command name at selected_index within the matches for prefix.
pub fn selected_slash_command(prefix: &str, selected_index: usize) -> Option<&'static str> {
    if !prefix.starts_with('/') {
        return None;
    }
    slash_command_matches(prefix)
        .get(selected_index)
        .map(|(name, _)| *name)
}

/// Format one line for the command list display.
pub fn slash_command_list_line(name: &str, description: &str, content_width: usize) -> String {
    let prefix = format!("  {name:<7} ");
    let prefix_width = prefix.chars().count();
    let desc_width = content_width.saturating_sub(prefix_width);
    let description = truncate_display_width(description, desc_width);
    format!("{prefix}{description}")
}

/// Format all matching commands as text lines (for non-interactive output).
pub fn slash_command_match_lines(prefix: &str, content_width: usize) -> Vec<String> {
    let matches = slash_command_matches(prefix);
    if matches.is_empty() {
        return vec![
            truncate_display_width(&format!("  No command matches {prefix}."), content_width),
            truncate_display_width("  Type / to show all commands.", content_width),
        ];
    }
    matches
        .iter()
        .map(|(name, description)| slash_command_list_line(name, description, content_width))
        .collect()
}

// ---------------------------------------------------------------------------
// Display helpers (duplicated from cli.rs to keep this module standalone)
// ---------------------------------------------------------------------------

fn truncate_display_width(text: &str, max_width: usize) -> String {
    let mut width = 0usize;
    let mut output = String::new();
    for ch in text.chars() {
        let char_width = terminal_char_width(ch);
        if width.saturating_add(char_width) > max_width {
            break;
        }
        output.push(ch);
        width += char_width;
    }
    output
}

fn terminal_char_width(ch: char) -> usize {
    if ch.is_control() {
        0
    } else if is_wide_terminal_char(ch) {
        2
    } else {
        1
    }
}

fn is_wide_terminal_char(ch: char) -> bool {
    matches!(
        ch as u32,
        0x1100..=0x115F
            | 0x2329..=0x232A
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE10..=0xFE19
            | 0xFE30..=0xFE6F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x1F300..=0x1FAFF
            | 0x20000..=0x3FFFD
    )
}

// ---------------------------------------------------------------------------
// Popup rendering
// ---------------------------------------------------------------------------

/// Build styled popup lines for the slash command autocomplete.
pub fn slash_command_popup_lines(
    prefix: &str,
    selected_index: usize,
    content_width: usize,
) -> Vec<String> {
    let matches = slash_command_matches(prefix);
    let mut lines = Vec::new();
    if matches.is_empty() {
        lines.push(truncate_display_width(
            &format!("  No command matches {prefix}."),
            content_width,
        ));
        lines.push(truncate_display_width(
            "  Type / to show all commands.",
            content_width,
        ));
        return lines;
    }
    for (index, (name, description)) in matches.iter().enumerate() {
        let prefix_text = if index == selected_index {
            format!("› {name:<7} ")
        } else {
            format!("  {name:<7} ")
        };
        if index == selected_index {
            let visible_line =
                truncate_display_width(&format!("{prefix_text}{description}"), content_width);
            lines.push(format!("\x1b[36;1m{visible_line}\x1b[0m"));
        } else {
            let prefix_width = terminal_display_width(&prefix_text);
            if prefix_width >= content_width {
                lines.push(truncate_display_width(&prefix_text, content_width));
            } else {
                let description =
                    truncate_display_width(description, content_width.saturating_sub(prefix_width));
                lines.push(format!("{prefix_text}\x1b[2m{description}\x1b[0m"));
            }
        }
    }
    lines
}

fn terminal_display_width(text: &str) -> usize {
    text.chars().map(terminal_char_width).sum()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bare_slash_is_help() {
        let cmd = parse_slash_command("/").unwrap().unwrap();
        assert_eq!(cmd, SlashCommand::Help);
    }

    #[test]
    fn parse_help() {
        assert_eq!(
            parse_slash_command("/help").unwrap().unwrap(),
            SlashCommand::Help
        );
    }

    #[test]
    fn parse_goal_show() {
        assert_eq!(
            parse_slash_command("/goal").unwrap().unwrap(),
            SlashCommand::Goal(GoalSubcommand::Show)
        );
    }

    #[test]
    fn parse_goal_set() {
        assert_eq!(
            parse_slash_command("/goal set fix the bug")
                .unwrap()
                .unwrap(),
            SlashCommand::Goal(GoalSubcommand::Set {
                objective: "fix the bug".to_string()
            })
        );
    }

    #[test]
    fn parse_goal_set_without_set_keyword() {
        assert_eq!(
            parse_slash_command("/goal fix the bug").unwrap().unwrap(),
            SlashCommand::Goal(GoalSubcommand::Set {
                objective: "fix the bug".to_string()
            })
        );
    }

    #[test]
    fn parse_goal_clear() {
        assert_eq!(
            parse_slash_command("/goal clear").unwrap().unwrap(),
            SlashCommand::Goal(GoalSubcommand::Clear)
        );
    }

    #[test]
    fn parse_goal_pause() {
        assert_eq!(
            parse_slash_command("/goal pause").unwrap().unwrap(),
            SlashCommand::Goal(GoalSubcommand::Pause)
        );
    }

    #[test]
    fn parse_goal_resume() {
        assert_eq!(
            parse_slash_command("/goal resume").unwrap().unwrap(),
            SlashCommand::Goal(GoalSubcommand::Resume)
        );
    }

    #[test]
    fn parse_goal_set_empty_returns_error() {
        let result = parse_slash_command("/goal set").unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn parse_quit_aliases() {
        assert_eq!(
            parse_slash_command("/quit").unwrap().unwrap(),
            SlashCommand::Quit
        );
        assert_eq!(
            parse_slash_command("/exit").unwrap().unwrap(),
            SlashCommand::Quit
        );
    }

    #[test]
    fn parse_non_slash_returns_none() {
        assert!(parse_slash_command("hello").is_none());
    }

    #[test]
    fn builtin_commands_includes_goal() {
        let names: Vec<&str> = builtin_slash_commands()
            .iter()
            .map(|(name, _)| *name)
            .collect();
        assert!(names.contains(&"/goal"));
        assert!(names.contains(&"/status"));
        assert!(names.contains(&"/help"));
    }

    #[test]
    fn slash_command_match_count_for_partial() {
        assert_eq!(slash_command_match_count("/go"), Some(1));
        assert_eq!(slash_command_match_count("/st"), Some(1)); // /status
        assert_eq!(slash_command_match_count("/unknown"), None);
    }
}
