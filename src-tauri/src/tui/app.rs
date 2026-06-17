//! TUI app: state + event loop + frame draw.
//!
//! This is the ratatui replacement for the legacy hand-rolled crossterm
//! rendering in `cli::run_chat`. The actual session/agent logic still
//! lives in `cli.rs` — this file only owns the visual layer.

use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::agent_runner::AgentConversationMessage;
use crate::goal_state::GoalState;
use crate::slash_command::{builtin_slash_commands, parse_slash_command, SlashCommand};
use crate::tui::terminal::TuiGuard;
use crate::tui::widgets::composer::{ComposerMode, ComposerState, ComposerWidget};
use crate::tui::widgets::footer::{FooterStatus, FooterWidget};
use crate::tui::widgets::header::{HeaderState, HeaderWidget};
use crate::tui::widgets::slash_popup::{SlashEntry, SlashPopupWidget};
use crate::tui::widgets::transcript::{TranscriptLine, TranscriptWidget};

pub use crate::slash_command::GoalSubcommand;

/// Inputs the host (`cli::run_chat`) passes into the TUI.
#[derive(Clone, Debug)]
pub struct AppEvent {
    /// Append a user message to the visible transcript.
    pub append_user: Option<String>,
    /// Append an assistant message to the visible transcript.
    pub append_assistant: Option<String>,
    /// Start a "task running" indicator with the given label.
    pub task_started: Option<String>,
    /// Stop the "task running" indicator.
    pub task_finished: Option<()>,
    /// Update the goal indicator.
    pub set_goal: Option<Option<GoalState>>,
    /// Update the workspace label.
    pub set_workspace: Option<String>,
    /// Update the model label.
    pub set_model: Option<String>,
    /// Update the reasoning label.
    pub set_reasoning: Option<String>,
    /// Clear the visible transcript.
    pub clear_transcript: bool,
    /// Quit the TUI.
    pub quit: bool,
}

impl Default for AppEvent {
    fn default() -> Self {
        Self {
            append_user: None,
            append_assistant: None,
            task_started: None,
            task_finished: None,
            set_goal: None,
            set_workspace: None,
            set_model: None,
            set_reasoning: None,
            clear_transcript: false,
            quit: false,
        }
    }
}

/// Result of running the TUI.
#[derive(Clone, Debug)]
pub enum AppOutcome {
    /// The user submitted a plain text task. The host should run the agent.
    Submit(String),
    /// The user submitted a slash command. The host should dispatch it.
    Command { name: String, raw: String },
    /// The user quit (Ctrl-C, Esc, /quit, etc.).
    Exit,
}

/// Top-level TUI state. Owns the transcript, the composer, and the visual
/// status fields. The host (`run_tui`) is responsible for providing
/// `AppEvent`s while the TUI is running.
pub struct App {
    header: HeaderState,
    composer: ComposerState,
    transcript: Vec<TranscriptLine>,
    footer: FooterStatus,
    slash_entries: Vec<SlashEntry>,
    slash_selected: usize,
    /// Index into the most recent slash match list. Recomputed every
    /// render from `slash_entries` + current composer text.
    history: Vec<String>,
    history_index: Option<usize>,
    history_draft: String,
    task_running: bool,
    task_started: Option<Instant>,
    pending_outcome: Option<AppOutcome>,
    version: String,
}

impl App {
    pub fn new(version: String) -> Self {
        let slash_entries = builtin_slash_commands()
            .iter()
            .map(|(name, desc)| SlashEntry {
                name: *name,
                description: *desc,
            })
            .collect();
        Self {
            header: HeaderState {
                title: String::new(),
                model_line: String::new(),
                directory_line: String::new(),
                version: version.clone(),
            },
            composer: ComposerState::default(),
            transcript: Vec::new(),
            footer: FooterStatus {
                model: String::new(),
                reasoning: String::new(),
                context_remaining: "100% context left".to_string(),
                workspace: String::new(),
                goal: None,
                task_running: false,
                elapsed_seconds: 0,
            },
            slash_entries,
            slash_selected: 0,
            history: Vec::new(),
            history_index: None,
            history_draft: String::new(),
            task_running: false,
            task_started: None,
            pending_outcome: None,
            version,
        }
    }

    /// Push a message into the visible transcript. Safe to call from outside
    /// the TUI's event loop (e.g. from the agent runner thread).
    pub fn push_message(&mut self, line: TranscriptLine) {
        self.transcript.push(line);
    }

    /// Apply an event coming from the host.
    pub fn apply(&mut self, event: AppEvent) {
        if event.clear_transcript {
            self.transcript.clear();
        }
        if let Some(text) = event.append_user {
            self.transcript.push(TranscriptLine {
                role: crate::tui::widgets::transcript::TranscriptRole::User,
                text,
            });
        }
        if let Some(text) = event.append_assistant {
            self.transcript.push(TranscriptLine {
                role: crate::tui::widgets::transcript::TranscriptRole::Assistant,
                text,
            });
        }
        if let Some(_label) = event.task_started {
            self.task_running = true;
            self.task_started = Some(Instant::now());
        }
        if event.task_finished.is_some() {
            self.task_running = false;
            self.task_started = None;
        }
        if let Some(goal) = event.set_goal {
            self.footer.goal = goal;
        }
        if let Some(workspace) = event.set_workspace {
            self.header.directory_line = workspace.clone();
            self.footer.workspace = workspace;
        }
        if let Some(model) = event.set_model {
            self.header.model_line = format!("model:     {model}   /model to change");
            self.footer.model = model;
        }
        if let Some(reasoning) = event.set_reasoning {
            self.footer.reasoning = reasoning;
        }
        if event.quit {
            self.pending_outcome = Some(AppOutcome::Exit);
        }
    }

    /// Take any pending outcome produced during the last event/tick.
    pub fn take_outcome(&mut self) -> Option<AppOutcome> {
        self.pending_outcome.take()
    }

    /// Compute the area splits for a given frame size.
    fn layout(&self, area: Rect) -> (Rect, Rect, Rect, Rect, Rect) {
        let header_height = if self.transcript.is_empty() {
            HeaderWidget::HEIGHT
        } else {
            0
        };
        let popup_height = if self.popup_matches().is_empty() {
            0
        } else {
            (self.popup_matches().len() as u16).min(SlashPopupWidget::MAX_ROWS as u16)
        };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(header_height),
                Constraint::Min(3),
                Constraint::Length(
                    ComposerWidget::MIN_ROWS
                        .max(ComposerWidget::height_for(area.width, &self.composer.text)),
                ),
                Constraint::Length(popup_height),
                Constraint::Length(1),
            ])
            .split(area);
        (
            chunks[0], // header
            chunks[1], // transcript
            chunks[2], // composer
            chunks[3], // popup
            chunks[4], // footer
        )
    }

    /// Draw a single frame.
    pub fn draw(&mut self, frame: &mut Frame) {
        let (header, transcript, composer, popup, footer) = self.layout(frame.area());
        if header.height > 0 {
            HeaderWidget::render(&self.header, frame, header);
        }
        TranscriptWidget::render(&self.transcript, frame, transcript);
        let (cx, cy) = ComposerWidget::render(&self.composer, frame, composer, "\u{203a} ");
        if !self.popup_matches().is_empty() {
            SlashPopupWidget::render(&self.popup_matches(), self.slash_selected, frame, popup);
        }
        // Update the task-running elapsed time.
        if self.task_running {
            self.footer.task_running = true;
            if let Some(start) = self.task_started {
                self.footer.elapsed_seconds = start.elapsed().as_secs();
            }
        } else {
            self.footer.task_running = false;
        }
        FooterWidget::render(&self.footer, frame, footer);
        frame.set_cursor_position(ratatui::layout::Position::new(cx, cy));
    }

    /// Handle a key event. Returns `Some(outcome)` if the TUI wants the
    /// host to act (submit text, dispatch a command, exit).
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<AppOutcome> {
        // Slash popup navigation has priority when the popup is open and the
        // user is at the start of a slash input.
        if !self.popup_matches().is_empty() && self.composer.text.starts_with('/') {
            match key.code {
                KeyCode::Up => {
                    if self.slash_selected == 0 {
                        self.slash_selected = self.popup_matches().len().saturating_sub(1);
                    } else {
                        self.slash_selected -= 1;
                    }
                    return None;
                }
                KeyCode::Down => {
                    let count = self.popup_matches().len();
                    if count > 0 {
                        self.slash_selected = (self.slash_selected + 1) % count;
                    }
                    return None;
                }
                KeyCode::Tab => {
                    if let Some(entry) =
                        SlashPopupWidget::selected(&self.popup_matches(), self.slash_selected)
                    {
                        self.composer.text = format!("{} ", entry.name);
                        self.composer.cursor = self.composer.text.chars().count();
                        self.slash_selected = 0;
                    }
                    return None;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Esc => {
                if self.composer.text.is_empty() {
                    return Some(AppOutcome::Exit);
                }
                self.composer.text.clear();
                self.composer.cursor = 0;
                None
            }
            KeyCode::Char('c') | KeyCode::Char('C')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                Some(AppOutcome::Exit)
            }
            KeyCode::Char('d') | KeyCode::Char('D')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.composer.text.is_empty() =>
            {
                Some(AppOutcome::Exit)
            }
            KeyCode::Enter => {
                if key.modifiers.contains(KeyModifiers::SHIFT)
                    || key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.insert_char('\n');
                    return None;
                }
                let submitted = if self.composer.text.trim() == "/" {
                    "/help".to_string()
                } else {
                    self.composer.text.clone()
                };
                let submitted = submitted.trim().to_string();
                if submitted.is_empty() {
                    return None;
                }
                self.push_history(&submitted);
                // Decide whether this is a slash command or a plain task.
                let outcome = if submitted.starts_with('/') {
                    match parse_slash_command(&submitted) {
                        Some(Ok(_cmd)) => AppOutcome::Command {
                            name: submitted.clone(),
                            raw: submitted.clone(),
                        },
                        Some(Err(err)) => {
                            self.transcript.push(TranscriptLine {
                                role: crate::tui::widgets::transcript::TranscriptRole::User,
                                text: submitted.clone(),
                            });
                            self.transcript.push(TranscriptLine {
                                role: crate::tui::widgets::transcript::TranscriptRole::Assistant,
                                text: format!("error: {err}"),
                            });
                            self.composer.text.clear();
                            self.composer.cursor = 0;
                            self.composer.mode = ComposerMode::Editing;
                            return None;
                        }
                        None => AppOutcome::Submit(submitted.clone()),
                    }
                } else {
                    AppOutcome::Submit(submitted.clone())
                };
                self.composer.text.clear();
                self.composer.cursor = 0;
                self.composer.mode = ComposerMode::Submitted;
                self.slash_selected = 0;
                Some(outcome)
            }
            KeyCode::Backspace => {
                if self.composer.cursor > 0 {
                    self.composer.cursor -= 1;
                    let byte = char_byte_index(&self.composer.text, self.composer.cursor);
                    self.composer.text.remove(byte);
                }
                self.slash_selected = 0;
                None
            }
            KeyCode::Delete => {
                if self.composer.cursor < self.composer.text.chars().count() {
                    let byte = char_byte_index(&self.composer.text, self.composer.cursor);
                    self.composer.text.remove(byte);
                }
                self.slash_selected = 0;
                None
            }
            KeyCode::Home => {
                self.composer.cursor = 0;
                None
            }
            KeyCode::End => {
                self.composer.cursor = self.composer.text.chars().count();
                None
            }
            KeyCode::Up => {
                self.history_prev();
                self.slash_selected = 0;
                None
            }
            KeyCode::Down => {
                self.history_next();
                self.slash_selected = 0;
                None
            }
            KeyCode::Left => {
                if self.composer.cursor > 0 {
                    self.composer.cursor -= 1;
                }
                None
            }
            KeyCode::Right => {
                if self.composer.cursor < self.composer.text.chars().count() {
                    self.composer.cursor += 1;
                }
                None
            }
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                if ch == '\n' {
                    self.insert_char('\n');
                } else {
                    self.insert_char(ch);
                }
                self.slash_selected = 0;
                None
            }
            _ => None,
        }
    }

    /// Current slash command match list based on composer text.
    pub fn popup_matches(&self) -> Vec<SlashEntry> {
        if !self.composer.text.starts_with('/') {
            return Vec::new();
        }
        SlashPopupWidget::filter(&self.composer.text, &self.slash_entries)
    }

    fn insert_char(&mut self, ch: char) {
        let byte = char_byte_index(&self.composer.text, self.composer.cursor);
        self.composer.text.insert(byte, ch);
        self.composer.cursor += 1;
    }

    fn push_history(&mut self, submitted: &str) {
        let submitted = submitted.trim();
        if submitted.is_empty() {
            return;
        }
        if self
            .history
            .last()
            .map(|last| last == submitted)
            .unwrap_or(false)
        {
            return;
        }
        self.history.push(submitted.to_string());
        if self.history.len() > 200 {
            self.history.remove(0);
        }
    }

    fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.history_index {
            Some(index) => index.saturating_sub(1),
            None => {
                self.history_draft = self.composer.text.clone();
                self.history.len().saturating_sub(1)
            }
        };
        self.history_index = Some(next);
        self.composer.text = self.history[next].clone();
        self.composer.cursor = self.composer.text.chars().count();
    }

    fn history_next(&mut self) {
        if let Some(index) = self.history_index {
            if index + 1 < self.history.len() {
                let next = index + 1;
                self.history_index = Some(next);
                self.composer.text = self.history[next].clone();
                self.composer.cursor = self.composer.text.chars().count();
            } else {
                self.history_index = None;
                self.composer.text = self.history_draft.clone();
                self.history_draft.clear();
                self.composer.cursor = self.composer.text.chars().count();
            }
        }
    }

    pub fn load_history(&mut self, history: Vec<String>) {
        self.history = history;
    }

    pub fn snapshot_history(&self) -> Vec<String> {
        self.history.clone()
    }

    pub fn clear_transcript(&mut self) {
        self.transcript.clear();
    }
}

fn char_byte_index(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len())
}

/// Run the TUI to completion. The `on_event` callback fires for every
/// internal state change (slash command submitted, task submitted, etc.) so
/// the host can dispatch agent work, run the tool-calling loop, and push
/// `AppEvent`s back into the app.
pub fn run_tui<F>(version: String, mut on_event: F) -> io::Result<AppOutcome>
where
    F: FnMut(&AppEvent),
{
    let mut app = App::new(version);
    let mut guard = TuiGuard::enter()?;

    let mut last_outcome: Option<AppOutcome> = None;
    let result = run_loop(&mut app, &mut guard, &mut on_event, &mut last_outcome);
    drop(guard);
    result.map(|_| last_outcome.unwrap_or(AppOutcome::Exit))
}

fn run_loop<F>(
    app: &mut App,
    guard: &mut TuiGuard,
    on_event: &mut F,
    last_outcome: &mut Option<AppOutcome>,
) -> io::Result<()>
where
    F: FnMut(&AppEvent),
{
    loop {
        guard.terminal.draw(|frame| app.draw(frame))?;
        if let Some(outcome) = app.take_outcome() {
            *last_outcome = Some(outcome.clone());
            match outcome {
                AppOutcome::Exit => return Ok(()),
                AppOutcome::Submit(text) => {
                    on_event(&AppEvent {
                        append_user: Some(text.clone()),
                        ..AppEvent::default()
                    });
                    app.transcript.push(TranscriptLine {
                        role: crate::tui::widgets::transcript::TranscriptRole::User,
                        text,
                    });
                }
                AppOutcome::Command { name, raw } => {
                    let event = match parse_slash_command(&raw) {
                        Some(Ok(SlashCommand::Quit)) => AppEvent {
                            quit: true,
                            ..AppEvent::default()
                        },
                        Some(Ok(SlashCommand::Goal(_sub))) => {
                            // Host handles the goal command after the loop
                            // returns; we mark task_started to suppress the
                            // spinner if needed. Keep transcript line for it.
                            app.transcript.push(TranscriptLine {
                                role: crate::tui::widgets::transcript::TranscriptRole::User,
                                text: name.clone(),
                            });
                            AppEvent::default()
                        }
                        Some(Ok(SlashCommand::New)) => {
                            app.transcript.clear();
                            AppEvent::default()
                        }
                        Some(Ok(SlashCommand::Clear)) => {
                            // Clear is purely visual; nothing to dispatch.
                            AppEvent::default()
                        }
                        Some(Ok(SlashCommand::Help)) => {
                            let text = builtin_slash_commands()
                                .iter()
                                .map(|(name, desc)| format!("{name:<8} {desc}"))
                                .collect::<Vec<_>>()
                                .join("\n");
                            app.transcript.push(TranscriptLine {
                                role: crate::tui::widgets::transcript::TranscriptRole::User,
                                text: name.clone(),
                            });
                            app.transcript.push(TranscriptLine {
                                role: crate::tui::widgets::transcript::TranscriptRole::Assistant,
                                text,
                            });
                            AppEvent::default()
                        }
                        Some(Ok(other)) => {
                            // Other commands (status, model, reason) are
                            // host-driven; pass them through and let the
                            // host dispatch.
                            let _ = other;
                            AppEvent::default()
                        }
                        Some(Err(_)) | None => AppEvent::default(),
                    };
                    on_event(&event);
                }
            }
        }
        if crossterm::event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = crossterm::event::read()? {
                if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                    if let Some(outcome) = app.handle_key(key) {
                        *last_outcome = Some(outcome);
                    }
                }
            }
        }
        if matches!(last_outcome, Some(AppOutcome::Exit)) {
            return Ok(());
        }
    }
}

/// Convert a list of agent conversation messages into transcript lines.
pub fn transcript_from_messages(messages: &[AgentConversationMessage]) -> Vec<TranscriptLine> {
    messages
        .iter()
        .filter_map(TranscriptLine::from_message)
        .collect()
}

/// Apply a goal subcommand locally (the most common path). The host can
/// still mutate `app.footer.goal` directly for `goal/set` from the tool.
pub fn apply_local_goal_subcommand(app: &mut App, sub: GoalSubcommand) -> Option<GoalState> {
    match sub {
        GoalSubcommand::Set { objective } => {
            let goal = GoalState::new(objective);
            app.footer.goal = Some(goal.clone());
            Some(goal)
        }
        GoalSubcommand::Clear => {
            app.footer.goal = None;
            None
        }
        GoalSubcommand::Pause => {
            if let Some(goal) = app.footer.goal.as_mut() {
                goal.pause();
                Some(goal.clone())
            } else {
                None
            }
        }
        GoalSubcommand::Resume => {
            if let Some(goal) = app.footer.goal.as_mut() {
                goal.resume();
                Some(goal.clone())
            } else {
                None
            }
        }
        GoalSubcommand::Show => app.footer.goal.clone(),
    }
}

// Silence dead_code warnings on the helper `Line`/`Span`/`Paragraph` imports
// when the widget set grows over time; they keep the import surface stable.
#[allow(dead_code)]
fn _imports(_: Line<'_>, _: Span<'_>, _: Paragraph<'_>) {}
