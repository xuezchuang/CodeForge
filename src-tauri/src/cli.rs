use std::fs;
use std::io::{self, IsTerminal, Write};

use crossterm::cursor;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::queue;
use crossterm::style::ResetColor;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType};
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::json;

use crate::agent_runner::{self, AgentRunInput};
use crate::app_state::{current_settings, AppState};
use crate::project_registry::{ProjectInput, ProjectSession};
use crate::tool_trace::{MockAgentRun, ToolTraceEvent, TraceEventType};
use crate::vs_registry::{AppSettings, ProviderConfig, ProviderCredential};

const CHAT_PROMPT: &str = "› ";

#[derive(Debug)]
pub struct Cli {
    pub project: Option<String>,
    pub provider: Option<String>,
    pub credential: Option<String>,
    pub model: Option<String>,
    pub reasoning: Option<String>,
    pub no_shell: bool,
    pub yes: bool,
    pub json: bool,
    pub verbose: bool,
    pub command: Command,
}

#[derive(Clone, Debug)]
struct CliSession {
    provider_id: Option<String>,
    credential_id: Option<String>,
    model_id: Option<String>,
    reasoning_effort: Option<String>,
}

#[derive(Clone, Debug)]
struct CliModelChoice {
    provider_id: String,
    provider_name: String,
    credential_id: Option<String>,
    credential_name: Option<String>,
    model_id: String,
    model_name: String,
}

#[derive(Clone, Debug)]
struct CliPickerItem {
    label: String,
    description: String,
}

#[derive(Debug)]
pub enum Command {
    Chat,
    Run { task: String },
    Projects { command: ProjectsCommand },
    Models { command: ModelsCommand },
}

#[derive(Debug)]
pub enum ProjectsCommand {
    List,
    Add(ProjectAddArgs),
}

#[derive(Debug, Default)]
pub struct ProjectAddArgs {
    pub name: Option<String>,
    pub path: Option<String>,
    pub solution: Option<String>,
}

#[derive(Debug)]
pub enum ModelsCommand {
    List,
}

pub async fn main_entry() -> Result<(), String> {
    let cli = parse_cli(std::env::args().skip(1).collect())?;
    run(cli).await
}

fn parse_cli(args: Vec<String>) -> Result<Cli, String> {
    let mut cli = Cli {
        project: None,
        provider: None,
        credential: None,
        model: None,
        reasoning: None,
        no_shell: false,
        yes: false,
        json: false,
        verbose: false,
        command: Command::Chat,
    };
    let mut positionals = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--project" => {
                i += 1;
                cli.project = Some(take_arg(&args, i, "--project")?);
            }
            "--provider" => {
                i += 1;
                cli.provider = Some(take_arg(&args, i, "--provider")?);
            }
            "--credential" => {
                i += 1;
                cli.credential = Some(take_arg(&args, i, "--credential")?);
            }
            "--model" => {
                i += 1;
                cli.model = Some(take_arg(&args, i, "--model")?);
            }
            "--reasoning" | "--reason" | "--reasion" => {
                i += 1;
                cli.reasoning = Some(take_arg(&args, i, "--reasoning")?);
            }
            "--no-shell" => cli.no_shell = true,
            "--yes" => cli.yes = true,
            "--json" => cli.json = true,
            "--verbose" => cli.verbose = true,
            "--help" | "-h" => return Err(help_text()),
            value => positionals.push(value.to_string()),
        }
        i += 1;
    }
    cli.command = parse_command(&positionals)?;
    Ok(cli)
}

fn take_arg(args: &[String], index: usize, flag: &str) -> Result<String, String> {
    args.get(index)
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn parse_command(args: &[String]) -> Result<Command, String> {
    match args {
        [] => Ok(Command::Chat),
        [cmd] if cmd == "chat" => Ok(Command::Chat),
        [cmd, task] if cmd == "run" => Ok(Command::Run { task: task.clone() }),
        [cmd, rest @ ..] if cmd == "run" => Ok(Command::Run {
            task: rest.join(" "),
        }),
        [cmd, sub] if cmd == "projects" && sub == "list" => Ok(Command::Projects {
            command: ProjectsCommand::List,
        }),
        [cmd, sub, rest @ ..] if cmd == "projects" && sub == "add" => Ok(Command::Projects {
            command: ProjectsCommand::Add(parse_project_add(rest)?),
        }),
        [cmd, sub] if cmd == "models" && sub == "list" => Ok(Command::Models {
            command: ModelsCommand::List,
        }),
        _ => Err(help_text()),
    }
}

fn parse_project_add(args: &[String]) -> Result<ProjectAddArgs, String> {
    let mut parsed = ProjectAddArgs::default();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--name" => {
                i += 1;
                parsed.name = Some(take_arg(args, i, "--name")?);
            }
            "--path" => {
                i += 1;
                parsed.path = Some(take_arg(args, i, "--path")?);
            }
            "--solution" => {
                i += 1;
                parsed.solution = Some(take_arg(args, i, "--solution")?);
            }
            other if parsed.path.is_none() => {
                parsed.path = Some(other.to_string());
            }
            other => return Err(format!("Unknown projects add argument: {other}")),
        }
        i += 1;
    }
    Ok(parsed)
}

fn help_text() -> String {
    "Usage: codeforge [--project <name-or-path>] [--provider <provider>] [--credential <credential>] [--model <model>] [--reasoning <effort>] [--no-shell] [--yes] [--json] [--verbose] <chat|run|projects|models>\n\nCommands:\n  codeforge\n  codeforge chat\n  codeforge run \"<task>\"\n  codeforge projects list\n  codeforge projects add [path] [--name <name>] [--path <path>] [--solution <solution.sln>]\n  codeforge models list\n\nInteractive commands:\n  /           show commands\n  /model      choose model, then reasoning\n  /reason     choose reasoning effort\n  /status     show current model selection\n  /exit       quit".to_string()
}

pub async fn run(cli: Cli) -> Result<(), String> {
    let state = AppState::load()?;
    let session = CliSession::from_cli(&cli);
    match &cli.command {
        Command::Projects { command } => run_projects(&state, command, cli.json),
        Command::Models { command } => run_models(&state, command, cli.json),
        Command::Run { task } => run_task(&state, &cli, &session, task).await.map(|_| ()),
        Command::Chat => run_chat(&state, &cli).await,
    }
}

impl CliSession {
    fn from_cli(cli: &Cli) -> Self {
        Self {
            provider_id: cli.provider.clone(),
            credential_id: cli.credential.clone(),
            model_id: cli.model.clone(),
            reasoning_effort: normalize_cli_reasoning(cli.reasoning.as_deref()),
        }
    }
}

fn run_projects(
    state: &AppState,
    command: &ProjectsCommand,
    json_output: bool,
) -> Result<(), String> {
    match command {
        ProjectsCommand::List => {
            let projects = state
                .projects
                .lock()
                .map_err(|_| "project registry lock failed".to_string())?
                .list();
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&projects).map_err(|e| e.to_string())?
                );
            } else if projects.is_empty() {
                println!("No projects registered.");
            } else {
                for project in projects {
                    println!("{}\t{}\t{}", project.name, project.id, project.repo_root);
                }
            }
            Ok(())
        }
        ProjectsCommand::Add(args) => {
            let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
            let root = args
                .path
                .clone()
                .unwrap_or_else(|| cwd.to_string_lossy().to_string());
            let name = args.name.clone().unwrap_or_else(|| {
                Path::new(&root)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("workspace")
                    .to_string()
            });
            let input = ProjectInput {
                name,
                repo_root: root,
                solution_path: args.solution.clone(),
                uproject_path: None,
                build_command: None,
            };
            let project = state
                .projects
                .lock()
                .map_err(|_| "project registry lock failed".to_string())?
                .add(input)?;
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&project).map_err(|e| e.to_string())?
                );
            } else {
                println!("Added project {} ({})", project.name, project.repo_root);
            }
            Ok(())
        }
    }
}

fn run_models(state: &AppState, command: &ModelsCommand, json_output: bool) -> Result<(), String> {
    match command {
        ModelsCommand::List => {
            let settings_guard = state
                .settings
                .lock()
                .map_err(|_| "settings lock failed".to_string())?;
            let settings = current_settings(&settings_guard);
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&settings.providers).map_err(|e| e.to_string())?
                );
            } else {
                for provider in settings.providers {
                    println!(
                        "{}\t{}\tenabled={}",
                        provider.id, provider.name, provider.enabled
                    );
                    for model in provider.models {
                        println!("  {}\tenabled={}", model.id, model.enabled);
                    }
                }
            }
            Ok(())
        }
    }
}

async fn run_chat(state: &AppState, cli: &Cli) -> Result<(), String> {
    let stdin = io::stdin();
    let mut session = CliSession::from_cli(cli);
    loop {
        let Some(line) = read_chat_input(&stdin, CHAT_PROMPT, &session)? else {
            break;
        };
        let task = line.trim();
        if task.is_empty() {
            continue;
        }
        if matches!(task, "/exit" | "/quit") {
            break;
        }
        if handle_chat_command(state, &stdin, &mut session, task)? {
            continue;
        }
        run_task(state, cli, &session, task).await?;
    }
    Ok(())
}

struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

struct InlineScreenGuard {
    start_row: u16,
}

impl InlineScreenGuard {
    fn enter() -> Result<Self, String> {
        let mut stdout = io::stdout();
        let (_, start_row) = cursor::position().unwrap_or((0, 0));
        execute!(stdout, cursor::Hide).map_err(|e| e.to_string())?;
        Ok(Self { start_row })
    }

    fn start_row(&self) -> u16 {
        self.start_row
    }

    fn clear(&self) -> Result<(), String> {
        let mut stdout = io::stdout();
        queue!(
            stdout,
            ResetColor,
            cursor::MoveTo(0, self.start_row),
            Clear(ClearType::FromCursorDown)
        )
        .map_err(|e| e.to_string())?;
        stdout.flush().map_err(|e| e.to_string())
    }
}

impl Drop for InlineScreenGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        let _ = execute!(stdout, cursor::Show);
    }
}

fn read_chat_input(
    stdin: &io::Stdin,
    prompt: &str,
    session: &CliSession,
) -> Result<Option<String>, String> {
    if !stdin.is_terminal() {
        return read_prompt(stdin, prompt);
    }

    read_interactive_chat_input(prompt, session)
}

fn read_interactive_chat_input(
    prompt: &str,
    session: &CliSession,
) -> Result<Option<String>, String> {
    enable_raw_mode().map_err(|e| e.to_string())?;
    let raw_mode = RawModeGuard;
    let inline_screen = InlineScreenGuard::enter()?;
    let mut line = String::new();
    let mut selected_command_index = 0usize;
    render_chat_input(
        prompt,
        &line,
        selected_command_index,
        session,
        inline_screen.start_row(),
    )?;

    loop {
        match event::read().map_err(|e| e.to_string())? {
            Event::Key(KeyEvent {
                code,
                modifiers,
                kind,
                ..
            }) if matches!(kind, KeyEventKind::Press | KeyEventKind::Repeat) => match code {
                KeyCode::Enter => {
                    let submitted = selected_slash_command(&line, selected_command_index)
                        .map(str::to_string)
                        .unwrap_or_else(|| line.clone());
                    let should_echo = !submitted.trim_start().starts_with('/');
                    inline_screen.clear()?;
                    drop(inline_screen);
                    drop(raw_mode);
                    if should_echo {
                        println!("{prompt}{submitted}");
                    }
                    io::stdout().flush().map_err(|e| e.to_string())?;
                    if submitted == "/" {
                        return Ok(Some(String::new()));
                    }
                    return Ok(Some(submitted));
                }
                KeyCode::Backspace => {
                    line.pop();
                    selected_command_index = 0;
                    render_chat_input(
                        prompt,
                        &line,
                        selected_command_index,
                        session,
                        inline_screen.start_row(),
                    )?;
                }
                KeyCode::Up => {
                    if let Some(count) = slash_command_match_count(&line) {
                        selected_command_index = if selected_command_index == 0 {
                            count.saturating_sub(1)
                        } else {
                            selected_command_index.saturating_sub(1)
                        };
                        render_chat_input(
                            prompt,
                            &line,
                            selected_command_index,
                            session,
                            inline_screen.start_row(),
                        )?;
                    }
                }
                KeyCode::Down => {
                    if let Some(count) = slash_command_match_count(&line) {
                        selected_command_index = (selected_command_index + 1) % count.max(1);
                        render_chat_input(
                            prompt,
                            &line,
                            selected_command_index,
                            session,
                            inline_screen.start_row(),
                        )?;
                    }
                }
                KeyCode::Tab => {
                    if let Some(command) = selected_slash_command(&line, selected_command_index) {
                        line.clear();
                        line.push_str(command);
                        selected_command_index = 0;
                        render_chat_input(
                            prompt,
                            &line,
                            selected_command_index,
                            session,
                            inline_screen.start_row(),
                        )?;
                    }
                }
                KeyCode::Esc => {
                    line.clear();
                    inline_screen.clear()?;
                    drop(inline_screen);
                    drop(raw_mode);
                    io::stdout().flush().map_err(|e| e.to_string())?;
                    return Ok(Some(line));
                }
                KeyCode::Char('c') | KeyCode::Char('C')
                    if modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    inline_screen.clear()?;
                    drop(inline_screen);
                    drop(raw_mode);
                    io::stdout().flush().map_err(|e| e.to_string())?;
                    return Ok(None);
                }
                KeyCode::Char('d') | KeyCode::Char('D')
                    if modifiers.contains(KeyModifiers::CONTROL) && line.is_empty() =>
                {
                    inline_screen.clear()?;
                    drop(inline_screen);
                    drop(raw_mode);
                    io::stdout().flush().map_err(|e| e.to_string())?;
                    return Ok(None);
                }
                KeyCode::Char(ch)
                    if !modifiers.contains(KeyModifiers::CONTROL)
                        && !modifiers.contains(KeyModifiers::ALT) =>
                {
                    if ch == '/' && line == "/" {
                        continue;
                    }
                    line.push(ch);
                    selected_command_index = 0;
                    render_chat_input(
                        prompt,
                        &line,
                        selected_command_index,
                        session,
                        inline_screen.start_row(),
                    )?;
                }
                _ => {}
            },
            Event::Key(_) => {}
            _ => {}
        }
    }
}

fn render_chat_input(
    prompt: &str,
    line: &str,
    selected_command_index: usize,
    session: &CliSession,
    start_row: u16,
) -> Result<(), String> {
    let mut stdout = io::stdout();
    let (cols, rows) = crossterm::terminal::size().map_err(|e| e.to_string())?;
    let rows = rows.saturating_sub(start_row).max(1);
    let header_rows = codex_chat_header_height();
    let composer_height = 3u16;
    let top_gap = 1u16;
    let popup_lines = if line.starts_with('/') {
        slash_command_popup_lines(line, selected_command_index)
            .into_iter()
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let max_popup_rows = rows
        .saturating_sub(header_rows + top_gap + composer_height)
        .max(1) as usize;
    let visible_lines = popup_lines
        .into_iter()
        .take(max_popup_rows)
        .collect::<Vec<_>>();
    let composer_start = header_rows
        .saturating_add(top_gap)
        .min(rows.saturating_sub(composer_height + visible_lines.len() as u16));
    let prompt_row = start_row.saturating_add(composer_start.saturating_add(1));
    let popup_start = start_row.saturating_add(composer_start.saturating_add(composer_height));
    queue!(
        stdout,
        cursor::MoveTo(0, start_row),
        Clear(ClearType::FromCursorDown)
    )
    .map_err(|e| e.to_string())?;
    render_codex_chat_header(&mut stdout, cols, session)?;

    render_composer_band(
        &mut stdout,
        cols,
        start_row.saturating_add(composer_start),
        composer_height,
        prompt_row,
        prompt,
        line,
    )?;
    let cursor_col = prompt
        .chars()
        .count()
        .saturating_add(line.chars().count())
        .min(cols.saturating_sub(1) as usize) as u16;
    queue!(stdout, ResetColor, cursor::MoveTo(cursor_col, prompt_row))
        .map_err(|e| e.to_string())?;
    for (index, text) in visible_lines.iter().enumerate() {
        queue!(
            stdout,
            ResetColor,
            cursor::MoveTo(0, popup_start + index as u16),
            Clear(ClearType::CurrentLine)
        )
        .map_err(|e| e.to_string())?;
        write!(stdout, "{text}").map_err(|e| e.to_string())?;
    }
    queue!(stdout, cursor::MoveTo(cursor_col, prompt_row)).map_err(|e| e.to_string())?;
    stdout.flush().map_err(|e| e.to_string())
}

fn codex_chat_header_height() -> u16 {
    9
}

fn render_codex_chat_header(
    stdout: &mut io::Stdout,
    cols: u16,
    session: &CliSession,
) -> Result<(), String> {
    let width = codex_header_width(cols);
    let border = "─".repeat(width.saturating_sub(2));
    writeln!(stdout, "╭{border}╮").map_err(|e| e.to_string())?;
    write_box_line(
        stdout,
        width,
        &format!(">_ CodeForge Codex (v{})", env!("CARGO_PKG_VERSION")),
    )?;
    write_box_line(stdout, width, "")?;
    write_box_line(
        stdout,
        width,
        &format!(
            "model:     {}    /model to change",
            cli_session_model_label(session)
        ),
    )?;
    write_box_line(
        stdout,
        width,
        &format!("directory: {}", cli_current_directory_label()),
    )?;
    writeln!(stdout, "╰{border}╯").map_err(|e| e.to_string())?;
    writeln!(stdout).map_err(|e| e.to_string())?;
    writeln!(stdout, "Tip: Type / to open commands. Type /exit to quit.")
        .map_err(|e| e.to_string())?;
    writeln!(
        stdout,
        "\x1b[33m! Heads up, CodeForge CLI does not track provider quota locally. Run /status for the current selection.\x1b[0m"
    )
    .map_err(|e| e.to_string())
}

fn render_composer_band(
    stdout: &mut io::Stdout,
    cols: u16,
    start_row: u16,
    height: u16,
    prompt_row: u16,
    prompt: &str,
    line: &str,
) -> Result<(), String> {
    let width = cols.max(1) as usize;
    let input = truncate_chars(&format!("{prompt}{line}"), width);
    let blank = " ".repeat(width);
    for row in start_row..start_row.saturating_add(height) {
        queue!(
            stdout,
            cursor::MoveTo(0, row),
            Clear(ClearType::CurrentLine)
        )
        .map_err(|e| e.to_string())?;
        if row == prompt_row {
            write!(stdout, "\x1b[48;5;236m{:<width$}\x1b[0m", input).map_err(|e| e.to_string())?;
        } else {
            write!(stdout, "\x1b[48;5;236m{blank}\x1b[0m").map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn write_box_line(stdout: &mut io::Stdout, width: usize, text: &str) -> Result<(), String> {
    let inner_width = width.saturating_sub(2);
    let clipped = truncate_chars(text, inner_width);
    writeln!(stdout, "│{:<inner_width$}│", clipped).map_err(|e| e.to_string())
}

fn codex_header_width(cols: u16) -> usize {
    let available = cols.saturating_sub(2).max(24) as usize;
    available.min(78)
}

fn cli_session_model_label(session: &CliSession) -> String {
    let model = session.model_id.as_deref().unwrap_or("auto");
    let reasoning = session.reasoning_effort.as_deref().unwrap_or("default");
    if reasoning == "default" {
        model.to_string()
    } else {
        format!("{model} {reasoning}")
    }
}

fn cli_current_directory_label() -> String {
    std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    if max_chars <= 3 {
        return text.chars().take(max_chars).collect();
    }
    let mut truncated = text
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    truncated.push_str("...");
    truncated
}

async fn run_task(
    state: &AppState,
    cli: &Cli,
    session: &CliSession,
    task: &str,
) -> Result<MockAgentRun, String> {
    let project = select_project(state, cli.project.as_deref())?;
    let settings_guard = state
        .settings
        .lock()
        .map_err(|_| "settings lock failed".to_string())?;
    let settings = current_settings(&settings_guard);
    let input = AgentRunInput {
        project_id: project.id.clone(),
        user_prompt: task.to_string(),
        messages: None,
        provider_id: session.provider_id.clone(),
        credential_id: session.credential_id.clone(),
        model_id: session.model_id.clone(),
        reasoning_effort: session.reasoning_effort.clone(),
        allow_shell: !cli.no_shell,
        assume_yes: cli.yes,
        cli_mode: true,
    };

    let mut terminal_events = Vec::new();
    let run = agent_runner::run_agent(&project, &settings, input, |event| {
        terminal_events.push(event.clone());
        if !cli.json {
            print_trace_event(event, cli.verbose);
        }
    })
    .await?;
    save_trace(&project.repo_root, &run)?;

    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&run).map_err(|e| e.to_string())?
        );
    } else if let Some(final_text) = final_response(&run) {
        println!("{final_text}");
    } else if let Some(last) = terminal_events
        .last()
        .and_then(|event| event.output_summary.clone())
    {
        println!("{last}");
    }
    Ok(run)
}

fn handle_chat_command(
    state: &AppState,
    stdin: &io::Stdin,
    session: &mut CliSession,
    task: &str,
) -> Result<bool, String> {
    match task.trim().to_ascii_lowercase().as_str() {
        "/" => {
            print_slash_commands();
            Ok(true)
        }
        "/model" | "/models" => {
            choose_cli_model(state, stdin, session)?;
            Ok(true)
        }
        "/reason" | "/reasoning" | "/reasion" => {
            choose_cli_reasoning(stdin, session)?;
            Ok(true)
        }
        "/status" => {
            print_cli_session(session);
            Ok(true)
        }
        "/fast"
        | "/ide"
        | "/permissions"
        | "/keymap"
        | "/vim"
        | "/sandbox-add-read-dir"
        | "/experimental" => {
            println!("{} is not implemented yet.", task.trim());
            Ok(true)
        }
        "/help" => {
            print_slash_commands();
            Ok(true)
        }
        value if value.starts_with('/') => {
            run_slash_command_prefix(state, stdin, session, value)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn run_slash_command_prefix(
    state: &AppState,
    stdin: &io::Stdin,
    session: &mut CliSession,
    prefix: &str,
) -> Result<(), String> {
    let matches = slash_commands()
        .iter()
        .filter(|(name, _)| name.starts_with(prefix))
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        print_slash_command_matches(prefix);
        return Ok(());
    }

    match matches[0].0 {
        "/model" => choose_cli_model(state, stdin, session),
        "/reason" => choose_cli_reasoning(stdin, session),
        "/status" => {
            print_cli_session(session);
            Ok(())
        }
        command => {
            println!("{command} is not implemented yet.");
            Ok(())
        }
    }
}

fn print_slash_commands() {
    println!();
    for (name, description) in slash_commands() {
        println!("  {name:<22} {description}");
    }
}

fn print_slash_command_matches(prefix: &str) {
    let mut stdout = io::stdout();
    let _ = write_slash_command_matches(&mut stdout, prefix);
    let _ = stdout.flush();
}

fn write_slash_command_matches<W: Write>(stdout: &mut W, prefix: &str) -> io::Result<()> {
    for line in slash_command_match_lines(prefix) {
        writeln!(stdout, "{line}")?;
    }
    Ok(())
}

fn slash_command_match_lines(prefix: &str) -> Vec<String> {
    let matches = slash_command_matches(prefix);
    let mut lines = Vec::new();
    lines.push(String::new());
    if matches.is_empty() {
        lines.push(format!("  No command matches `{prefix}`."));
        lines.push("  Type / to show all commands.".to_string());
        return lines;
    }
    for (name, description) in matches {
        lines.push(format!("  {name:<22} {description}"));
    }
    lines
}

fn slash_command_popup_lines(prefix: &str, selected_index: usize) -> Vec<String> {
    let matches = slash_command_matches(prefix);
    let mut lines = Vec::new();
    lines.push(String::new());
    if matches.is_empty() {
        lines.push(format!("  No command matches `{prefix}`."));
        lines.push("  Type / to show all commands.".to_string());
        return lines;
    }
    for (index, (name, description)) in matches.into_iter().enumerate() {
        if index == selected_index {
            lines.push(format!("\x1b[36;1m› {name:<22} {description}\x1b[0m"));
        } else {
            lines.push(format!("  {name:<22} \x1b[2m{description}\x1b[0m"));
        }
    }
    lines
}

fn slash_command_matches(prefix: &str) -> Vec<(&'static str, &'static str)> {
    slash_commands()
        .iter()
        .copied()
        .filter(|(name, _)| name.starts_with(prefix))
        .collect()
}

fn slash_command_match_count(prefix: &str) -> Option<usize> {
    if !prefix.starts_with('/') {
        return None;
    }
    let count = slash_command_matches(prefix).len();
    (count > 0).then_some(count)
}

fn selected_slash_command(prefix: &str, selected_index: usize) -> Option<&'static str> {
    if !prefix.starts_with('/') {
        return None;
    }
    slash_command_matches(prefix)
        .get(selected_index)
        .map(|(name, _)| *name)
}

fn slash_commands() -> &'static [(&'static str, &'static str)] {
    &[
        ("/model", "choose what model and reasoning effort to use"),
        ("/fast", "1.5x speed, increased usage"),
        (
            "/ide",
            "include current selection, open files, and other context from your IDE",
        ),
        ("/permissions", "choose what CodeForge is allowed to do"),
        ("/keymap", "remap TUI shortcuts"),
        ("/vim", "toggle Vim mode for the composer"),
        (
            "/sandbox-add-read-dir",
            "let sandbox read a directory: /sandbox-add-read-dir <absolute_path>",
        ),
        ("/experimental", "toggle experimental features"),
        ("/reason", "choose reasoning effort"),
        ("/status", "show current model selection"),
    ]
}

fn choose_cli_model(
    state: &AppState,
    stdin: &io::Stdin,
    session: &mut CliSession,
) -> Result<(), String> {
    let settings = {
        let settings_guard = state
            .settings
            .lock()
            .map_err(|_| "settings lock failed".to_string())?;
        current_settings(&settings_guard)
    };
    let choices = cli_model_choices(&settings);
    if choices.is_empty() {
        println!("No enabled models. Configure providers and models in Desktop Settings first.");
        return Ok(());
    }

    if stdin.is_terminal() {
        let items = choices
            .iter()
            .map(|choice| CliPickerItem {
                label: cli_model_display_name(choice),
                description: cli_model_description(choice).to_string(),
            })
            .collect::<Vec<_>>();
        let current_index = choices
            .iter()
            .enumerate()
            .find_map(|(index, choice)| {
                cli_choice_is_current(session, choice, index).then_some(index)
            })
            .unwrap_or(0);
        let Some(index) = run_cli_picker(
            session,
            "Select Model and Effort",
            "Access legacy models by running codeforge --model <model_name> or editing ~/.codeforge/settings.json",
            &items,
            current_index,
        )? else {
            return Ok(());
        };
        let choice = &choices[index];
        session.provider_id = Some(choice.provider_id.clone());
        session.credential_id = choice.credential_id.clone();
        session.model_id = Some(choice.model_id.clone());
        println!("Selected model: {}", cli_model_display_name(choice));
        return choose_cli_reasoning(stdin, session);
    }

    println!();
    println!("  Select Model and Effort");
    println!("  Access legacy models by running codeforge --model <model_name> or editing ~/.codeforge/settings.json");
    println!();
    for (index, choice) in choices.iter().enumerate() {
        let marker = if cli_choice_is_current(session, choice, index) {
            "›"
        } else {
            " "
        };
        let current = if cli_choice_is_current(session, choice, index) {
            " (current)"
        } else {
            ""
        };
        println!(
            "{marker} {}. {}{}    {}",
            index + 1,
            cli_model_display_name(choice),
            current,
            cli_model_description(choice)
        );
    }
    println!();
    println!("  Press enter to keep current or type a number.");

    let Some(index) = read_number(stdin, "› ", choices.len())? else {
        return Ok(());
    };
    let choice = &choices[index];
    session.provider_id = Some(choice.provider_id.clone());
    session.credential_id = choice.credential_id.clone();
    session.model_id = Some(choice.model_id.clone());
    println!("Selected model: {}", cli_model_display_name(choice));
    choose_cli_reasoning(stdin, session)
}

fn choose_cli_reasoning(stdin: &io::Stdin, session: &mut CliSession) -> Result<(), String> {
    let choices = reasoning_choices();
    let current = session.reasoning_effort.as_deref().unwrap_or("default");
    if stdin.is_terminal() {
        let items = choices
            .iter()
            .map(|(value, label)| CliPickerItem {
                label: (*label).to_string(),
                description: reasoning_description(value).to_string(),
            })
            .collect::<Vec<_>>();
        let current_index = choices
            .iter()
            .position(|(value, _)| *value == current)
            .unwrap_or(0);
        let Some(index) =
            run_cli_picker(session, "Select Reasoning Effort", "", &items, current_index)?
        else {
            return Ok(());
        };
        session.reasoning_effort = normalize_cli_reasoning(Some(choices[index].0));
        println!(
            "Selected reasoning: {}",
            session.reasoning_effort.as_deref().unwrap_or("default")
        );
        return Ok(());
    }

    println!();
    println!("  Select Reasoning Effort");
    println!();
    for (index, (value, label)) in choices.iter().enumerate() {
        let marker = if *value == current { "›" } else { " " };
        let current_label = if *value == current { " (current)" } else { "" };
        println!(
            "{marker} {}. {}{}    {}",
            index + 1,
            label,
            current_label,
            reasoning_description(value)
        );
    }
    println!();
    println!("  Press enter to keep current or type a number.");
    let Some(line) = read_prompt(stdin, "› ")? else {
        return Ok(());
    };
    if line.trim().is_empty() {
        return Ok(());
    }
    let selected = parse_reasoning_choice(&line, choices)
        .ok_or_else(|| format!("Unknown reasoning effort: {}", line.trim()))?;
    session.reasoning_effort = normalize_cli_reasoning(Some(selected));
    println!(
        "Selected reasoning: {}",
        session.reasoning_effort.as_deref().unwrap_or("default")
    );
    Ok(())
}

fn run_cli_picker(
    session: &CliSession,
    title: &str,
    subtitle: &str,
    items: &[CliPickerItem],
    current_index: usize,
) -> Result<Option<usize>, String> {
    if items.is_empty() {
        return Ok(None);
    }

    enable_raw_mode().map_err(|e| e.to_string())?;
    let raw_mode = RawModeGuard;
    let inline_screen = InlineScreenGuard::enter()?;
    let mut selected_index = current_index.min(items.len().saturating_sub(1));
    render_cli_picker(
        session,
        title,
        subtitle,
        items,
        selected_index,
        current_index,
        inline_screen.start_row(),
    )?;

    loop {
        match event::read().map_err(|e| e.to_string())? {
            Event::Key(KeyEvent {
                code,
                kind,
                modifiers,
                ..
            }) if matches!(kind, KeyEventKind::Press | KeyEventKind::Repeat) => match code {
                KeyCode::Up => {
                    selected_index = if selected_index == 0 {
                        items.len().saturating_sub(1)
                    } else {
                        selected_index.saturating_sub(1)
                    };
                    render_cli_picker(
                        session,
                        title,
                        subtitle,
                        items,
                        selected_index,
                        current_index,
                        inline_screen.start_row(),
                    )?;
                }
                KeyCode::Down => {
                    selected_index = (selected_index + 1) % items.len();
                    render_cli_picker(
                        session,
                        title,
                        subtitle,
                        items,
                        selected_index,
                        current_index,
                        inline_screen.start_row(),
                    )?;
                }
                KeyCode::Enter => {
                    inline_screen.clear()?;
                    drop(inline_screen);
                    drop(raw_mode);
                    return Ok(Some(selected_index));
                }
                KeyCode::Esc => {
                    inline_screen.clear()?;
                    drop(inline_screen);
                    drop(raw_mode);
                    return Ok(None);
                }
                KeyCode::Char('c') | KeyCode::Char('C')
                    if modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    inline_screen.clear()?;
                    drop(inline_screen);
                    drop(raw_mode);
                    return Ok(None);
                }
                _ => {}
            },
            Event::Key(_) => {}
            _ => {}
        }
    }
}

fn render_cli_picker(
    session: &CliSession,
    title: &str,
    subtitle: &str,
    items: &[CliPickerItem],
    selected_index: usize,
    current_index: usize,
    start_row: u16,
) -> Result<(), String> {
    let mut stdout = io::stdout();
    let (cols, rows) = crossterm::terminal::size().map_err(|e| e.to_string())?;
    let width = cols.max(1) as usize;
    let panel_start = start_row
        .saturating_add(codex_chat_header_height())
        .saturating_add(1)
        .min(rows.saturating_sub(1));
    let available_panel_rows = rows.saturating_sub(panel_start).max(1);
    let max_items = available_panel_rows.saturating_sub(5).max(1) as usize;
    let visible_count = items.len().min(max_items);
    let panel_height = (4 + visible_count as u16).min(available_panel_rows);
    queue!(
        stdout,
        cursor::MoveTo(0, start_row),
        Clear(ClearType::FromCursorDown)
    )
    .map_err(|e| e.to_string())?;
    render_codex_chat_header(&mut stdout, cols, session)?;
    render_panel_background(&mut stdout, cols, panel_start, panel_height)?;

    queue!(stdout, cursor::MoveTo(2, panel_start + 1)).map_err(|e| e.to_string())?;
    write!(
        stdout,
        "\x1b[48;5;236m\x1b[1m{}\x1b[0m",
        truncate_chars(title, width.saturating_sub(4))
    )
    .map_err(|e| e.to_string())?;
    if !subtitle.trim().is_empty() && panel_height > 2 {
        queue!(stdout, cursor::MoveTo(2, panel_start + 2)).map_err(|e| e.to_string())?;
        write!(
            stdout,
            "\x1b[48;5;236m\x1b[2m{}\x1b[0m",
            truncate_chars(subtitle, width.saturating_sub(4))
        )
        .map_err(|e| e.to_string())?;
    }

    for (index, item) in items.iter().take(visible_count).enumerate() {
        let row = panel_start + 4 + index as u16;
        if row >= rows {
            break;
        }
        let marker = if index == selected_index { "›" } else { " " };
        let current = if index == current_index {
            " (current)"
        } else {
            ""
        };
        let label = truncate_chars(
            &format!("{marker} {}. {}{}", index + 1, item.label, current),
            28,
        );
        let description = truncate_chars(&item.description, width.saturating_sub(34));
        queue!(
            stdout,
            cursor::MoveTo(0, row),
            Clear(ClearType::CurrentLine)
        )
        .map_err(|e| e.to_string())?;
        if index == selected_index {
            write!(
                stdout,
                "\x1b[48;5;236m\x1b[36;1m{:<28}  {}\x1b[0m",
                label, description
            )
            .map_err(|e| e.to_string())?;
        } else {
            write!(
                stdout,
                "\x1b[48;5;236m{:<28}  \x1b[2m{}\x1b[0m",
                label, description
            )
            .map_err(|e| e.to_string())?;
        }
    }

    let footer_row = panel_start
        .saturating_add(panel_height)
        .min(rows.saturating_sub(1));
    queue!(
        stdout,
        cursor::MoveTo(0, footer_row),
        Clear(ClearType::CurrentLine)
    )
    .map_err(|e| e.to_string())?;
    write!(
        stdout,
        "  \x1b[2mPress enter to confirm or esc to go back\x1b[0m"
    )
    .map_err(|e| e.to_string())?;
    queue!(stdout, ResetColor, cursor::MoveTo(0, footer_row)).map_err(|e| e.to_string())?;
    stdout.flush().map_err(|e| e.to_string())
}

fn render_panel_background(
    stdout: &mut io::Stdout,
    cols: u16,
    start_row: u16,
    height: u16,
) -> Result<(), String> {
    let blank = " ".repeat(cols.max(1) as usize);
    for row in start_row..start_row.saturating_add(height) {
        queue!(
            stdout,
            cursor::MoveTo(0, row),
            Clear(ClearType::CurrentLine)
        )
        .map_err(|e| e.to_string())?;
        write!(stdout, "\x1b[48;5;236m{blank}\x1b[0m").map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn cli_choice_is_current(session: &CliSession, choice: &CliModelChoice, index: usize) -> bool {
    if session.provider_id.is_none() && session.model_id.is_none() {
        return index == 0;
    }
    session.provider_id.as_deref() == Some(choice.provider_id.as_str())
        && session.credential_id.as_deref() == choice.credential_id.as_deref()
        && session.model_id.as_deref() == Some(choice.model_id.as_str())
}

fn cli_model_display_name(choice: &CliModelChoice) -> String {
    if choice.provider_id == "codex-cli" {
        return choice.model_name.clone();
    }
    match choice.credential_name.as_deref() {
        Some(credential) if !credential.trim().is_empty() => {
            format!(
                "{} / {} / {}",
                choice.provider_name, credential, choice.model_name
            )
        }
        _ => format!("{} / {}", choice.provider_name, choice.model_name),
    }
}

fn cli_model_description(choice: &CliModelChoice) -> &'static str {
    match choice.model_id.as_str() {
        "gpt-5.5" => "Frontier model for complex coding, research, and real-world work.",
        "gpt-5.4" => "Strong model for everyday coding.",
        "gpt-5.4-mini" => "Small, fast, and cost-efficient model for simpler coding tasks.",
        "gpt-5.3-codex-spark" => "Ultra-fast coding model.",
        "default" if choice.provider_id == "codex-cli" => "Use the model configured by Codex CLI.",
        _ if choice.provider_id == "codex-cli" => "Codex CLI model.",
        _ => "Configured provider model from ~/.codeforge/settings.json.",
    }
}

fn reasoning_description(value: &str) -> &'static str {
    match value {
        "default" => "Use the model/provider default.",
        "minimal" => "Fastest responses with minimal reasoning.",
        "low" => "Light reasoning for simple edits.",
        "medium" => "Balanced reasoning for normal coding work.",
        "high" => "More reasoning for harder bugs and design work.",
        "xhigh" => "Maximum reasoning for complex debugging and architecture.",
        _ => "",
    }
}

fn print_cli_session(session: &CliSession) {
    println!(
        "Model: provider={}, credential={}, model={}, reasoning={}",
        session.provider_id.as_deref().unwrap_or("auto"),
        session.credential_id.as_deref().unwrap_or("auto"),
        session.model_id.as_deref().unwrap_or("auto"),
        session.reasoning_effort.as_deref().unwrap_or("default")
    );
}

fn cli_model_choices(settings: &AppSettings) -> Vec<CliModelChoice> {
    settings
        .providers
        .iter()
        .filter(|provider| provider.enabled || provider.models.iter().any(|model| model.enabled))
        .flat_map(provider_model_choices)
        .collect()
}

fn provider_model_choices(provider: &ProviderConfig) -> Vec<CliModelChoice> {
    if !provider_uses_credentials(provider) {
        let enabled_models = provider
            .models
            .iter()
            .filter(|model| model.enabled)
            .map(|model| (model.id.clone(), model.name.clone()))
            .collect::<Vec<_>>();
        if enabled_models.is_empty() {
            return (!provider.default_model.trim().is_empty())
                .then(|| CliModelChoice {
                    provider_id: provider.id.clone(),
                    provider_name: provider.name.clone(),
                    credential_id: None,
                    credential_name: None,
                    model_id: provider.default_model.clone(),
                    model_name: provider.default_model.clone(),
                })
                .into_iter()
                .collect();
        }
        return enabled_models
            .into_iter()
            .map(|(model_id, model_name)| CliModelChoice {
                provider_id: provider.id.clone(),
                provider_name: provider.name.clone(),
                credential_id: None,
                credential_name: None,
                model_id,
                model_name,
            })
            .collect();
    }

    provider
        .credentials
        .iter()
        .filter(|credential| credential.enabled)
        .flat_map(|credential| credential_model_choices(provider, credential))
        .collect()
}

fn credential_model_choices(
    provider: &ProviderConfig,
    credential: &ProviderCredential,
) -> Vec<CliModelChoice> {
    provider
        .models
        .iter()
        .filter(|model| {
            model.enabled
                && (model.credential_id.trim().is_empty() || model.credential_id == credential.id)
        })
        .map(|model| CliModelChoice {
            provider_id: provider.id.clone(),
            provider_name: provider.name.clone(),
            credential_id: Some(credential.id.clone()),
            credential_name: Some(credential.name.clone()),
            model_id: model.id.clone(),
            model_name: model.name.clone(),
        })
        .collect()
}

fn provider_uses_credentials(provider: &ProviderConfig) -> bool {
    provider.provider_type != "codex-cli" && provider.provider_type != "ollama"
}

fn reasoning_choices() -> &'static [(&'static str, &'static str)] {
    &[
        ("default", "Default"),
        ("minimal", "Minimal"),
        ("low", "Low"),
        ("medium", "Medium"),
        ("high", "High"),
        ("xhigh", "XHigh"),
    ]
}

fn parse_reasoning_choice<'a>(
    line: &str,
    choices: &'a [(&'static str, &'static str)],
) -> Option<&'a str> {
    let trimmed = line.trim();
    if let Ok(index) = trimmed.parse::<usize>() {
        return choices.get(index.checked_sub(1)?).map(|choice| choice.0);
    }
    choices
        .iter()
        .find(|(value, label)| {
            value.eq_ignore_ascii_case(trimmed) || label.eq_ignore_ascii_case(trimmed)
        })
        .map(|choice| choice.0)
}

fn normalize_cli_reasoning(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("default") {
        return None;
    }
    Some(value.to_ascii_lowercase())
}

fn read_number(stdin: &io::Stdin, prompt: &str, max: usize) -> Result<Option<usize>, String> {
    let Some(line) = read_prompt(stdin, prompt)? else {
        return Ok(None);
    };
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let number = trimmed
        .parse::<usize>()
        .map_err(|_| format!("Expected a number from 1 to {max}"))?;
    if number == 0 || number > max {
        return Err(format!("Expected a number from 1 to {max}"));
    }
    Ok(Some(number - 1))
}

fn read_prompt(stdin: &io::Stdin, prompt: &str) -> Result<Option<String>, String> {
    print!("{prompt}");
    io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    let read = stdin.read_line(&mut line).map_err(|e| e.to_string())?;
    if read == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim().to_string()))
}

fn print_trace_event(event: &ToolTraceEvent, verbose: bool) {
    match event.event_type {
        TraceEventType::ToolCall => {
            let name = event.tool_name.as_deref().unwrap_or("tool");
            let args = event.output_summary.as_deref().unwrap_or("");
            println!("[tool:start] {name} {args}");
        }
        TraceEventType::ToolResult | TraceEventType::Error => {
            let Some(name) = event.tool_name.as_deref() else {
                return;
            };
            let status = event
                .output
                .as_ref()
                .and_then(|value| value.get("status"))
                .and_then(|value| value.as_str())
                .unwrap_or("ok");
            let elapsed = event.duration_ms.unwrap_or(0);
            match status {
                "ok" => println!("[tool:ok] {name} {elapsed}"),
                "timeout" => println!("[tool:timeout] {name} {elapsed}"),
                _ => {
                    let message = event
                        .output
                        .as_ref()
                        .and_then(|value| value.get("error"))
                        .and_then(|value| value.as_str())
                        .or(event.output_summary.as_deref())
                        .unwrap_or("tool failed");
                    println!("[tool:error] {name} {message}");
                }
            }
        }
        _ if verbose => println!("[trace] {}", event.title),
        _ => {}
    }
}

fn select_project(state: &AppState, requested: Option<&str>) -> Result<ProjectSession, String> {
    let projects = state
        .projects
        .lock()
        .map_err(|_| "project registry lock failed".to_string())?
        .list();
    if let Some(value) = requested.filter(|value| !value.trim().is_empty()) {
        let normalized = PathBuf::from(value);
        let normalized = normalized.canonicalize().ok();
        return projects
            .into_iter()
            .find(|project| {
                project.id == value
                    || project.name == value
                    || normalized
                        .as_ref()
                        .map(|path| {
                            Path::new(&project.repo_root).canonicalize().ok().as_ref() == Some(path)
                        })
                        .unwrap_or(false)
            })
            .ok_or_else(|| format!("Project not found: {value}"));
    }
    let cwd = std::env::current_dir()
        .map_err(|e| e.to_string())?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    projects
        .iter()
        .find(|project| Path::new(&project.repo_root).canonicalize().ok().as_ref() == Some(&cwd))
        .cloned()
        .or_else(|| projects.first().cloned())
        .ok_or_else(|| {
            "No projects registered. Run `codeforge projects add <workspace>` first.".to_string()
        })
}

fn final_response(run: &MockAgentRun) -> Option<String> {
    run.traces.iter().rev().find_map(|event| {
        if matches!(
            event.event_type,
            TraceEventType::FinalResponse | TraceEventType::ModelMessage
        ) {
            event.output_summary.clone().or_else(|| {
                event
                    .output
                    .as_ref()
                    .and_then(|value| value.get("message"))
                    .and_then(|value| value.as_str())
                    .map(ToString::to_string)
            })
        } else {
            None
        }
    })
}

fn save_trace(repo_root: &str, run: &MockAgentRun) -> Result<(), String> {
    let dir = Path::new(repo_root).join(".codeforge").join("traces");
    fs::create_dir_all(&dir)
        .map_err(|e| format!("trace directory create failed {}: {e}", dir.display()))?;
    let file = dir.join(format!("{}.json", Utc::now().format("%Y%m%dT%H%M%S%.3fZ")));
    let payload = json!({ "savedAt": Utc::now(), "run": run });
    fs::write(
        &file,
        serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("trace write failed {}: {e}", file.display()))
}
