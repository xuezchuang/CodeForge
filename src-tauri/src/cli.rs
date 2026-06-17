use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossterm::cursor;
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::queue;
use crossterm::style::ResetColor;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType, ScrollUp};
use std::path::{Path, PathBuf};

use chrono::Utc;
use reqwest::Client as HttpClient;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::agent_runner::{self, AgentConversationMessage, AgentRunInput};
use crate::app_state::{current_settings, AppState};
use crate::goal_state::GoalState;
use crate::project_registry::{ProjectInput, ProjectSession};
use crate::tool_trace::{MockAgentRun, ToolTraceEvent, TraceEventType};
use crate::tui::AppEvent;
use crate::vs_registry::{AppSettings, ProviderConfig, ProviderCredential};

const CHAT_PROMPT: &str = "› ";
const CLI_TRANSCRIPT_MAX_MESSAGES: usize = 20;
const CODEX_COMPOSER_MIN_ROWS: u16 = 3;
const CODEX_COMPOSER_MAX_ROWS: u16 = 8;
const CODEX_COMPOSER_FOOTER_GAP: u16 = 1;
const CODEX_FOOTER_HEIGHT: u16 = 1;
const CODEX_STARTUP_TIP_ROWS: u16 = 3;
const CODEX_POPUP_MAX_ROWS: u16 = 8;
const CODEX_COMPOSER_BG: &str = "\x1b[48;5;236m";

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
enum RenderedBlock {
    UserInput(String),
    AssistantReply(String),
}

impl RenderedBlock {
    fn lines_for(&self, width: usize) -> Vec<String> {
        let width = width.max(1);
        match self {
            RenderedBlock::UserInput(text) => wrap_block("▌ user", text, width),
            RenderedBlock::AssistantReply(text) => wrap_block("◂ assistant", text, width),
        }
    }
}

fn wrap_block(prefix: &str, body: &str, width: usize) -> Vec<String> {
    let prefix = truncate_display_width(prefix, width);
    let prefix_width = terminal_display_width(&prefix).min(width);
    let body_indent = " ".repeat(prefix_width.saturating_add(1));
    let body_width = width.saturating_sub(prefix_width.saturating_add(1)).max(1);
    let mut out: Vec<String> = Vec::new();
    let mut first = true;
    for raw_line in body.split('\n') {
        for wrapped in wrap_text(raw_line, body_width) {
            if first {
                out.push(format!("{prefix} {wrapped}"));
                first = false;
            } else {
                out.push(format!("{body_indent}{wrapped}"));
            }
        }
        if raw_line.is_empty() && !first {
            out.push(String::new());
        }
    }
    if out.is_empty() {
        out.push(format!("{prefix} "));
    }
    out
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for ch in text.chars() {
        let ch_width = terminal_char_width(ch).max(1);
        if current_width.saturating_add(ch_width) > width && !current.is_empty() {
            out.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push(ch);
        current_width = current_width.saturating_add(ch_width);
    }
    if !current.is_empty() || out.is_empty() {
        out.push(current);
    }
    out
}

#[derive(Clone, Debug)]
struct CliSession {
    provider_id: Option<String>,
    provider_name: Option<String>,
    credential_id: Option<String>,
    credential_name: Option<String>,
    model_id: Option<String>,
    reasoning_effort: Option<String>,
    workspace_label: Option<String>,
    shell_allowed: bool,
    goal: Option<GoalState>,
    goal_traces: Option<Vec<ToolTraceEvent>>,
}

#[derive(Clone, Debug)]
struct CliModelChoice {
    provider_id: String,
    provider_name: String,
    credential_id: Option<String>,
    credential_name: Option<String>,
    model_id: String,
    model_name: String,
    reasoning_mode: String,
    default_reasoning: String,
}

#[derive(Clone, Debug)]
struct CliPickerItem {
    label: String,
    description: String,
}

#[derive(Clone, Debug, Default)]
struct CodingPlanLimits {
    interval_percent_remaining: Option<f64>,
    weekly_percent_remaining: Option<f64>,
    interval_reset_seconds: Option<i64>,
    weekly_reset_seconds: Option<i64>,
    source_provider: Option<String>,
    source_model: Option<String>,
    error: Option<String>,
}

#[derive(Debug)]
pub enum Command {
    Help,
    Version,
    Chat,
    Status,
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
    let first_positional = first_positional_arg(&args);
    let help_requested = has_global_flag_before_terminator(&args, &["--help", "-h"])
        || first_positional == Some("help");
    let version_requested = has_global_flag_before_terminator(&args, &["--version", "-V"])
        || first_positional == Some("version");
    let mut cli = Cli {
        project: None,
        provider: None,
        credential: None,
        model: None,
        reasoning: None,
        no_shell: true,
        yes: false,
        json: false,
        verbose: false,
        command: Command::Chat,
    };
    if help_requested || version_requested {
        cli.json = args.iter().any(|arg| arg == "--json");
        cli.verbose = args.iter().any(|arg| arg == "--verbose");
        cli.command = if help_requested {
            Command::Help
        } else {
            Command::Version
        };
        return Ok(cli);
    }
    let mut positionals = Vec::new();
    let mut force_task = false;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--project" | "-C" => {
                let option = args[i].clone();
                i += 1;
                cli.project = Some(take_arg(&args, i, &option)?);
            }
            "--provider" => {
                i += 1;
                cli.provider = Some(take_arg(&args, i, "--provider")?);
            }
            "--credential" => {
                i += 1;
                cli.credential = Some(take_arg(&args, i, "--credential")?);
            }
            "--model" | "-m" => {
                let option = args[i].clone();
                i += 1;
                cli.model = Some(take_arg(&args, i, &option)?);
            }
            "--reasoning" | "--reason" | "--reasion" => {
                let option = args[i].clone();
                i += 1;
                cli.reasoning = Some(take_arg(&args, i, &option)?);
            }
            "--allow-shell" | "--shell" => {
                if !help_requested && !version_requested {
                    return Err(
                        "Generic shell execution is disabled by CodeForge safety policy."
                            .to_string(),
                    );
                }
            }
            "--no-shell" => cli.no_shell = true,
            "--yes" => cli.yes = true,
            "--json" => cli.json = true,
            "--verbose" => cli.verbose = true,
            "--help" | "-h" => {
                cli.command = Command::Help;
            }
            "--version" | "-V" => {
                cli.command = Command::Version;
            }
            "--" => {
                force_task = true;
                positionals.extend(args.iter().skip(i + 1).cloned());
                break;
            }
            "-" => positionals.push("-".to_string()),
            value if value.starts_with('-') && positionals.is_empty() => {
                return Err(format!("Unknown option: {value}"));
            }
            value => positionals.push(value.to_string()),
        }
        i += 1;
    }
    if matches!(cli.command, Command::Help | Command::Version) {
        return Ok(cli);
    }
    if force_task && !positionals.is_empty() {
        cli.command = Command::Run {
            task: positionals.join(" "),
        };
        return Ok(cli);
    }
    cli.command = parse_command(&positionals)?;
    Ok(cli)
}

fn has_global_flag_before_terminator(args: &[String], flags: &[&str]) -> bool {
    args.iter()
        .take_while(|arg| arg.as_str() != "--")
        .any(|arg| flags.contains(&arg.as_str()))
}

fn first_positional_arg(args: &[String]) -> Option<&str> {
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--project" | "-C" | "--provider" | "--credential" | "--model" | "-m"
            | "--reasoning" | "--reason" | "--reasion" => {
                index += 2;
            }
            "--no-shell" | "--allow-shell" | "--shell" | "--yes" | "--json" | "--verbose"
            | "--help" | "-h" | "--version" | "-V" => {
                index += 1;
            }
            value => return Some(value),
        }
    }
    None
}

fn take_arg(args: &[String], index: usize, flag: &str) -> Result<String, String> {
    let value = args
        .get(index)
        .ok_or_else(|| format!("{flag} requires a value"))?;
    if value.starts_with('-') {
        return Err(format!("{flag} requires a value"));
    }
    Ok(value.clone())
}

fn parse_command(args: &[String]) -> Result<Command, String> {
    match args {
        [] => Ok(Command::Chat),
        [cmd] if cmd == "help" => Ok(Command::Help),
        [cmd] if cmd == "version" => Ok(Command::Version),
        [cmd] if cmd == "chat" => Ok(Command::Chat),
        [cmd] if cmd == "status" => Ok(Command::Status),
        [dash] if dash == "-" => Ok(Command::Run {
            task: String::new(),
        }),
        [cmd] if cmd == "projects" => Ok(Command::Projects {
            command: ProjectsCommand::List,
        }),
        [cmd] if cmd == "models" => Ok(Command::Models {
            command: ModelsCommand::List,
        }),
        [cmd, task] if cmd == "run" || cmd == "exec" => Ok(Command::Run { task: task.clone() }),
        [cmd, rest @ ..] if cmd == "run" || cmd == "exec" => Ok(Command::Run {
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
        [cmd, sub, ..] if cmd == "projects" => Err(format!(
            "Unknown projects command: {sub}. Usage: codeforge projects, codeforge projects list, or codeforge projects add."
        )),
        [cmd, sub, ..] if cmd == "models" => Err(format!(
            "Unknown models command: {sub}. Usage: codeforge models or codeforge models list."
        )),
        [cmd, sub, ..] if cmd == "status" => Err(format!(
            "Unknown status argument: {sub}. Usage: codeforge status."
        )),
        [cmd, sub, ..] if cmd == "chat" => Err(format!(
            "Unknown chat argument: {sub}. Usage: codeforge chat."
        )),
        rest if !rest.is_empty() => Ok(Command::Run {
            task: rest.join(" "),
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
    "Usage: codeforge [-C, --project <name-or-path>] [--provider <provider>] [--credential <credential>] [-m, --model <model>] [--reasoning <effort|off|on>] [--yes] [--json] [--verbose] [command]\n\nOptions:\n  -C, --project <name-or-path>  use a registered project or workspace path\n  --provider <provider>         select provider id\n  --credential <credential>     select credential id\n  -m, --model <model>           select model id\n  --reasoning <effort>          select reasoning/thinking mode\n  --no-shell                    shell execution remains disabled by policy\n  --yes                         assume yes for supported confirmations\n  --json                        print machine-readable JSON where supported\n  --verbose                     print extra trace events\n  --                            stop parsing options; treat following tokens as task text\n  --help, -h                    show help\n  --version, -V                 show version\n\nCommands:\n  codeforge\n  codeforge chat\n  codeforge \"<task>\"\n  codeforge -\n  codeforge status\n  codeforge run \"<task>\"\n  codeforge exec \"<task>\"\n  echo \"<task>\" | codeforge run\n  echo \"<task>\" | codeforge exec\n  codeforge projects\n  codeforge projects list\n  codeforge projects add [path] [--name <name>] [--path <path>] [--solution <solution.sln>]\n  codeforge models\n  codeforge models list\n  codeforge help\n  codeforge version\n\nInteractive commands:\n  /           show commands\n  /new        start a new chat and clear conversation context\n  /model      choose model and reasoning/thinking\n  /reason     choose reasoning/thinking\n  /status     show model, provider, directory, workspace, permissions, account, session, agents.md, and usage/limits\n  /clear      clear the terminal\n  /help       show commands\n  /exit       quit\n  /quit       quit".to_string()
}

pub async fn run(cli: Cli) -> Result<(), String> {
    if matches!(cli.command, Command::Help) {
        print_cli_help(cli.json)?;
        return Ok(());
    }
    if matches!(cli.command, Command::Version) {
        print_cli_version(cli.json)?;
        return Ok(());
    }
    if cli.json && matches!(cli.command, Command::Chat) {
        return Err(
            "--json is not supported for interactive chat. Use status, projects, models, run, or exec."
                .to_string(),
        );
    }

    let state = AppState::load()?;
    let mut session = CliSession::from_cli(&cli);
    match &cli.command {
        Command::Help => Ok(()),
        Command::Version => Ok(()),
        Command::Status => {
            hydrate_cli_session_defaults(&state, &mut session)?;
            let project = select_project(&state, cli.project.as_deref())?;
            let limits = fetch_coding_plan_limits(&state, &session).await;
            if cli.json {
                print_cli_session_json(&session, Some(&project), Some(&limits))?;
            } else {
                print_cli_session(&session, Some(&project), Some(&limits), false);
            }
            Ok(())
        }
        Command::Projects { command } => run_projects(&state, command, cli.json),
        Command::Models { command } => run_models(&state, command, cli.json),
        Command::Run { task } => {
            let task = cli_run_task_text(task)?;
            hydrate_cli_session_defaults(&state, &mut session)?;
            run_task(&state, &cli, &session, &task, None, false, true)
                .await
                .map(|_| ())
        }
        Command::Chat => {
            hydrate_cli_session_defaults(&state, &mut session)?;
            run_chat(&state, &cli, session).await
        }
    }
}

fn print_cli_help(json_output: bool) -> Result<(), String> {
    if json_output {
        let payload = json!({
            "usage": "codeforge [-C, --project <name-or-path>] [--provider <provider>] [--credential <credential>] [-m, --model <model>] [--reasoning <effort|off|on>] [--yes] [--json] [--verbose] [command]",
            "options": [
                { "name": "-C, --project <name-or-path>", "description": "use a registered project or workspace path" },
                { "name": "--provider <provider>", "description": "select provider id" },
                { "name": "--credential <credential>", "description": "select credential id" },
                { "name": "-m, --model <model>", "description": "select model id" },
                { "name": "--reasoning <effort>", "description": "select reasoning/thinking mode" },
                { "name": "--no-shell", "description": "shell execution remains disabled by policy" },
                { "name": "--yes", "description": "assume yes for supported confirmations" },
                { "name": "--json", "description": "print machine-readable JSON where supported" },
                { "name": "--verbose", "description": "print extra trace events" },
                { "name": "--", "description": "stop parsing options; treat following tokens as task text" },
                { "name": "--help, -h", "description": "show help" },
                { "name": "--version, -V", "description": "show version" },
            ],
            "commands": [
                { "name": "codeforge", "description": "start interactive chat" },
                { "name": "codeforge chat", "description": "start interactive chat" },
                { "name": "codeforge \"<task>\"", "description": "run one task and exit" },
                { "name": "codeforge -", "description": "read one task from stdin and exit" },
                { "name": "codeforge status", "description": "show model, provider, directory, workspace, permissions, account, session, agents.md, and usage/limits" },
                { "name": "codeforge run \"<task>\"", "description": "run one task and exit" },
                { "name": "codeforge exec \"<task>\"", "description": "alias for codeforge run" },
                { "name": "echo \"<task>\" | codeforge run", "description": "read one task from stdin and exit" },
                { "name": "echo \"<task>\" | codeforge exec", "description": "read one task from stdin and exit" },
                { "name": "codeforge projects", "description": "list registered projects" },
                { "name": "codeforge projects list", "description": "list registered projects" },
                { "name": "codeforge projects add", "description": "register a project" },
                { "name": "codeforge models", "description": "list configured providers and models" },
                { "name": "codeforge models list", "description": "list configured providers and models" },
                { "name": "codeforge help", "description": "show help" },
                { "name": "codeforge version", "description": "show version" },
            ],
            "interactiveCommands": slash_commands()
                .iter()
                .map(|(name, description)| {
                    json!({
                        "name": name,
                        "description": description,
                    })
                })
                .collect::<Vec<_>>(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|error| error.to_string())?
        );
    } else {
        println!("{}", help_text());
    }
    Ok(())
}

fn cli_run_task_text(task: &str) -> Result<String, String> {
    let trimmed = task.trim();
    if !trimmed.is_empty() && trimmed != "-" {
        return Ok(trimmed.to_string());
    }

    let stdin = io::stdin();
    if stdin.is_terminal() {
        return Err(
            "codeforge run/exec requires a task. Usage: codeforge run \"<task>\", codeforge exec \"<task>\", or pipe a task into codeforge run/exec."
                .to_string(),
        );
    }

    let mut input = String::new();
    stdin
        .lock()
        .read_to_string(&mut input)
        .map_err(|error| format!("Failed to read task from stdin: {error}"))?;
    let input = input.trim();
    if input.is_empty() {
        return Err(
            "codeforge run/exec received an empty task from stdin. Usage: codeforge run \"<task>\", codeforge exec \"<task>\", or pipe a task into codeforge run/exec."
                .to_string(),
        );
    }
    Ok(input.to_string())
}

fn print_cli_version(json_output: bool) -> Result<(), String> {
    if json_output {
        let payload = json!({
            "app": "codeforge",
            "name": "codeforge",
            "surface": "cli",
            "version": env!("CARGO_PKG_VERSION"),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|error| error.to_string())?
        );
    } else {
        println!("codeforge {}", env!("CARGO_PKG_VERSION"));
    }
    Ok(())
}

impl CliSession {
    fn from_cli(cli: &Cli) -> Self {
        Self {
            provider_id: cli.provider.clone(),
            provider_name: None,
            credential_id: cli.credential.clone(),
            credential_name: None,
            model_id: cli.model.clone(),
            reasoning_effort: normalize_cli_reasoning(cli.reasoning.as_deref()),
            workspace_label: None,
            shell_allowed: !cli.no_shell,
            goal: None,
            goal_traces: None,
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
                    serde_json::to_string_pretty(&cli_projects_json(&projects))
                        .map_err(|e| e.to_string())?
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

fn cli_projects_json(projects: &[ProjectSession]) -> serde_json::Value {
    json!(projects
        .iter()
        .map(|project| {
            json!({
                "id": project.id,
                "name": project.name,
                "root": project.repo_root,
                "repoRoot": project.repo_root,
                "solutionPath": project.solution_path,
                "uprojectPath": project.uproject_path,
                "buildCommand": project.build_command,
                "vsBridgeEndpoint": project.vs_bridge_endpoint,
                "vsProcessId": project.vs_process_id,
                "createdAt": project.created_at,
                "updatedAt": project.updated_at,
            })
        })
        .collect::<Vec<_>>())
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
                    serde_json::to_string_pretty(&cli_models_json(&settings))
                        .map_err(|e| e.to_string())?
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

fn cli_models_json(settings: &AppSettings) -> serde_json::Value {
    json!(settings
        .providers
        .iter()
        .map(|provider| {
            json!({
                "id": provider.id,
                "type": provider.provider_type,
                "name": provider.name,
                "enabled": provider.enabled,
                "baseUrl": provider.base_url,
                "baseUrlLocked": provider.base_url_locked,
                "supportsToolCall": provider.supports_tool_call,
                "defaultCredentialId": provider.default_credential_id,
                "defaultModel": provider.default_model,
                "temperature": provider.temperature,
                "credentials": provider
                    .credentials
                    .iter()
                    .map(|credential| {
                        json!({
                            "id": credential.id,
                            "name": credential.name,
                            "enabled": credential.enabled,
                        })
                    })
                    .collect::<Vec<_>>(),
                "models": provider
                    .models
                    .iter()
                    .map(|model| {
                        json!({
                            "id": model.id,
                            "name": model.name,
                            "enabled": model.enabled,
                            "credentialId": model.credential_id,
                            "reasoningMode": model.reasoning_mode,
                            "defaultReasoning": model.default_reasoning,
                            "ownedBy": model.owned_by,
                            "created": model.created,
                        })
                    })
                    .collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>())
}

/// CodeForge chat loop.
async fn run_chat(state: &AppState, cli: &Cli, mut session: CliSession) -> Result<(), String> {
    let stdin = io::stdin();
    let stdin_is_terminal = stdin.is_terminal();
    let mut handled_input = false;
    let mut history = Vec::new();
    let mut transcript: Vec<AgentConversationMessage> = Vec::new();
    let mut blocks: Vec<RenderedBlock> = Vec::new();
    let mut show_header = true;
    let project = select_project(state, cli.project.as_deref())?;
    session.workspace_label = Some(cli_project_label(&project));
    let screen_guard = if stdin_is_terminal {
        Some(CliScreenGuard::enter()?)
    } else {
        None
    };
    let screen_origin_row = screen_guard
        .as_ref()
        .map(CliScreenGuard::origin_row)
        .unwrap_or(0);
    let mut input_start_row = screen_origin_row;
    loop {
        let current_input_start_row = if show_header {
            screen_origin_row
        } else {
            input_start_row
        };
        let Some((line, input_header_visible, submitted_start_row)) = read_chat_input(
            &stdin,
            CHAT_PROMPT,
            &session,
            &mut history,
            show_header,
            current_input_start_row,
            screen_origin_row,
            &blocks,
        )?
        else {
            break;
        };
        show_header = input_header_visible;
        let task = line.trim();
        if task.is_empty() {
            continue;
        }
        handled_input = true;
        let command_result = if task.starts_with('/') {
            if stdin_is_terminal {
                input_start_row = if is_picker_chat_command(task) {
                    clear_submitted_input_area(submitted_start_row, input_header_visible)?
                } else {
                    finalize_submitted_input(
                        CHAT_PROMPT,
                        task,
                        submitted_start_row,
                        input_header_visible,
                    )?
                };
            }
            show_header = false;
            handle_chat_command(state, &stdin, &mut session, cli.project.as_deref(), task).await?
        } else {
            if stdin_is_terminal {
                input_start_row = finalize_submitted_input(
                    CHAT_PROMPT,
                    task,
                    submitted_start_row,
                    input_header_visible,
                )?;
            }
            show_header = false;
            ChatCommandResult::NotCommand
        };
        match command_result {
            ChatCommandResult::Exit => break,
            ChatCommandResult::NewSession => {
                transcript.clear();
                blocks.clear();
                if !stdin_is_terminal {
                    println!("Started a new chat.");
                }
                show_header = false;
                continue;
            }
            ChatCommandResult::Handled => {
                if stdin_is_terminal {
                    input_start_row = next_chat_input_start_row()?;
                }
                show_header = false;
                continue;
            }
            ChatCommandResult::NotCommand => {}
        }
        blocks.push(RenderedBlock::UserInput(task.to_string()));
        let mut run_messages = transcript.clone();
        run_messages.push(cli_conversation_message("user", task));
        transcript.push(cli_conversation_message("user", task));
        let run = run_task(
            state,
            cli,
            &session,
            task,
            Some(run_messages),
            stdin_is_terminal,
            true,
        )
        .await?;
        if let Some(parts) = final_response(&run) {
            let hide_thinking = cli_should_hide_thinking(&session);
            let visible_text = if hide_thinking {
                strip_think_blocks(&parts.content)
            } else {
                parts.content.clone()
            };
            if !visible_text.trim().is_empty() {
                let assistant_text = visible_text.trim().to_string();
                blocks.push(RenderedBlock::AssistantReply(assistant_text.clone()));
                transcript.push(cli_conversation_message("assistant", &assistant_text));
            }
        }
        if transcript.len() > CLI_TRANSCRIPT_MAX_MESSAGES {
            let remove_count = transcript.len().saturating_sub(CLI_TRANSCRIPT_MAX_MESSAGES);
            transcript.drain(0..remove_count);
        }
        if blocks.len() > CLI_TRANSCRIPT_MAX_MESSAGES {
            let remove_count = blocks.len().saturating_sub(CLI_TRANSCRIPT_MAX_MESSAGES);
            blocks.drain(0..remove_count);
        }
        if stdin_is_terminal {
            input_start_row = next_chat_input_start_row()?;
        }
    }
    if !stdin_is_terminal && !handled_input {
        return Err(
            "codeforge received an empty task from stdin. Use codeforge \"<task>\", codeforge run \"<task>\", or pipe a non-empty task."
                .to_string(),
        );
    }
    Ok(())
}

#[derive(Debug)]
enum SubmitRequest {
    Submit(String),
    Exit,
}

async fn handle_tui_submit(
    event_tx: &std::sync::mpsc::Sender<AppEvent>,
    state: &AppState,
    cli: &Cli,
    project: &ProjectSession,
    session: &mut CliSession,
    transcript: std::sync::Arc<std::sync::Mutex<Vec<AgentConversationMessage>>>,
    text: String,
) -> Result<(), String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    // Slash commands are dispatched locally.
    if trimmed.starts_with('/') {
        if let Some(parsed) = crate::slash_command::parse_slash_command(trimmed) {
            match parsed {
                Ok(crate::slash_command::SlashCommand::Quit) => {
                    let _ = event_tx.send(AppEvent {
                        append_user: Some(trimmed.to_string()),
                        ..AppEvent::default()
                    });
                    let _ = event_tx.send(AppEvent {
                        quit: true,
                        ..AppEvent::default()
                    });
                    return Ok(());
                }
                Ok(crate::slash_command::SlashCommand::New) => {
                    transcript.lock().expect("transcript lock").clear();
                    let _ = event_tx.send(AppEvent {
                        clear_transcript: true,
                        append_assistant: Some("Started a new chat.".to_string()),
                        ..AppEvent::default()
                    });
                    return Ok(());
                }
                Ok(crate::slash_command::SlashCommand::Clear) => {
                    let _ = event_tx.send(AppEvent {
                        clear_transcript: true,
                        ..AppEvent::default()
                    });
                    return Ok(());
                }
                Ok(crate::slash_command::SlashCommand::Help) => {
                    let _ = event_tx.send(AppEvent {
                        append_user: Some(trimmed.to_string()),
                        ..AppEvent::default()
                    });
                    let text = crate::slash_command::builtin_slash_commands()
                        .iter()
                        .map(|(name, desc)| format!("{name:<8} {desc}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let _ = event_tx.send(AppEvent {
                        append_assistant: Some(text),
                        ..AppEvent::default()
                    });
                    return Ok(());
                }
                Ok(crate::slash_command::SlashCommand::Status) => {
                    let _ = event_tx.send(AppEvent {
                        append_user: Some(trimmed.to_string()),
                        append_assistant: Some(tui_status_summary(session, project)),
                        ..AppEvent::default()
                    });
                    return Ok(());
                }
                Ok(crate::slash_command::SlashCommand::Model)
                | Ok(crate::slash_command::SlashCommand::Reason) => {
                    let _ = event_tx.send(AppEvent {
                        append_user: Some(trimmed.to_string()),
                        ..AppEvent::default()
                    });
                    let _ = event_tx.send(AppEvent {
                        append_assistant: Some(format!(
                            "Run `codeforge {trimmed}` to change model/reasoning."
                        )),
                        ..AppEvent::default()
                    });
                    return Ok(());
                }
                Ok(crate::slash_command::SlashCommand::Goal(sub)) => {
                    let _ = event_tx.send(AppEvent {
                        append_user: Some(trimmed.to_string()),
                        ..AppEvent::default()
                    });
                    let (new_goal, message) = match sub {
                        crate::slash_command::GoalSubcommand::Set { objective } => {
                            let goal = crate::goal_state::GoalState::new(objective);
                            let message = tui_goal_summary(Some(&goal));
                            (Some(goal), message)
                        }
                        crate::slash_command::GoalSubcommand::Clear => {
                            (None, "Goal cleared.".to_string())
                        }
                        crate::slash_command::GoalSubcommand::Show => (
                            session.goal.clone(),
                            tui_goal_summary(session.goal.as_ref()),
                        ),
                        crate::slash_command::GoalSubcommand::Pause => {
                            if let Some(mut goal) = session.goal.clone() {
                                goal.pause();
                                let message = tui_goal_summary(Some(&goal));
                                (Some(goal), message)
                            } else {
                                (None, "No active goal.".to_string())
                            }
                        }
                        crate::slash_command::GoalSubcommand::Resume => {
                            if let Some(mut goal) = session.goal.clone() {
                                goal.resume();
                                let message = tui_goal_summary(Some(&goal));
                                (Some(goal), message)
                            } else {
                                (None, "No active goal.".to_string())
                            }
                        }
                    };
                    session.goal = new_goal.clone();
                    let _ = event_tx.send(AppEvent {
                        set_goal: Some(new_goal),
                        append_assistant: Some(message),
                        ..AppEvent::default()
                    });
                    return Ok(());
                }
                Err(err) => {
                    let _ = event_tx.send(AppEvent {
                        append_user: Some(trimmed.to_string()),
                        ..AppEvent::default()
                    });
                    let _ = event_tx.send(AppEvent {
                        append_assistant: Some(format!("error: {err}")),
                        ..AppEvent::default()
                    });
                    return Ok(());
                }
            }
        } else {
            let _ = event_tx.send(AppEvent {
                append_user: Some(trimmed.to_string()),
                append_assistant: Some(format!(
                    "Unknown command: {trimmed}\nType /help to show available commands."
                )),
                ..AppEvent::default()
            });
            return Ok(());
        }
    }
    // Regular task: run the agent.
    let _ = event_tx.send(AppEvent {
        append_user: Some(trimmed.to_string()),
        ..AppEvent::default()
    });
    transcript
        .lock()
        .expect("transcript lock")
        .push(AgentConversationMessage {
            role: "user".to_string(),
            content: trimmed.to_string(),
            attachments: Vec::new(),
        });
    let _ = event_tx.send(AppEvent {
        task_started: Some(trimmed.to_string()),
        ..AppEvent::default()
    });
    let prior_messages = {
        let messages = transcript.lock().expect("transcript lock");
        messages
            .iter()
            .take(messages.len().saturating_sub(1))
            .cloned()
            .collect::<Vec<_>>()
    };
    let run = run_task(
        state,
        cli,
        session,
        trimmed,
        Some(prior_messages),
        false,
        false,
    )
    .await?;
    if let Some(parts) = final_response(&run) {
        let content = parts.content.trim().to_string();
        let _ = event_tx.send(AppEvent {
            append_assistant: Some(content.clone()),
            task_finished: Some(()),
            ..AppEvent::default()
        });
        transcript
            .lock()
            .expect("transcript lock")
            .push(AgentConversationMessage {
                role: "assistant".to_string(),
                content,
                attachments: Vec::new(),
            });
    } else if let Some(summary) = run
        .traces
        .last()
        .and_then(|event| event.output_summary.clone())
    {
        let _ = event_tx.send(AppEvent {
            append_assistant: Some(summary.clone()),
            task_finished: Some(()),
            ..AppEvent::default()
        });
        transcript
            .lock()
            .expect("transcript lock")
            .push(AgentConversationMessage {
                role: "assistant".to_string(),
                content: summary,
                attachments: Vec::new(),
            });
    } else {
        let _ = event_tx.send(AppEvent {
            task_finished: Some(()),
            ..AppEvent::default()
        });
    }
    let _ = project;
    Ok(())
}

fn tui_status_summary(session: &CliSession, project: &ProjectSession) -> String {
    let mut lines = vec![
        format!("model: {}", cli_session_model_label(session)),
        format!("provider: {}", cli_session_provider_label(session)),
        format!("reasoning: {}", cli_session_reasoning_label(session)),
        format!("workspace: {}", cli_project_label(project)),
        format!(
            "shell: {}",
            if session.shell_allowed {
                "allowed by policy"
            } else {
                "disabled by CodeForge safety policy"
            }
        ),
    ];
    if let Some(goal) = session.goal.as_ref() {
        lines.push(format!(
            "goal: [{}] {}",
            goal.status.label(),
            goal.objective
        ));
    } else {
        lines.push("goal: none".to_string());
    }
    lines.join("\n")
}

fn tui_goal_summary(goal: Option<&crate::goal_state::GoalState>) -> String {
    match goal {
        Some(goal) if !goal.objective.trim().is_empty() => {
            format!("Goal [{}]: {}", goal.status.label(), goal.objective)
        }
        Some(goal) => format!("Goal [{}]", goal.status.label()),
        None => "No active goal.".to_string(),
    }
}

/// Non-interactive fallback: read a single task from stdin and run it.
/// Used when the user pipes input instead of running in a real TTY.
async fn run_chat_non_interactive(
    state: &AppState,
    cli: &Cli,
    mut session: CliSession,
) -> Result<(), String> {
    let project = select_project(state, cli.project.as_deref())?;
    session.workspace_label = Some(cli_project_label(&project));
    let mut buffer = String::new();
    if io::stdin().read_to_string(&mut buffer).is_err() {
        return Err("failed to read task from stdin".to_string());
    }
    let task = buffer.trim();
    if task.is_empty() {
        return Err(
            "codeforge received an empty task from stdin. Use codeforge \"<task>\", codeforge run \"<task>\", or pipe a non-empty task."
                .to_string(),
        );
    }
    run_task(state, cli, &session, task, None, false, true).await?;
    Ok(())
}
/// Run the ratatui TUI on a dedicated thread. Owns the terminal until the
/// user quits or the event channel closes.
fn run_tui_thread(
    version: String,
    submit_tx: std::sync::mpsc::Sender<SubmitRequest>,
    event_rx: std::sync::mpsc::Receiver<AppEvent>,
) -> Result<(), String> {
    use std::sync::mpsc::TryRecvError;
    use std::time::Duration;
    let mut guard =
        crate::tui::terminal::TuiGuard::enter().map_err(|e| format!("tui init failed: {e}"))?;
    let mut app = crate::tui::App::new(version);
    let mut exited = false;
    loop {
        // Drain any pending host events so the TUI reflects them before
        // we draw the next frame.
        loop {
            match event_rx.try_recv() {
                Ok(event) => {
                    let is_quit = event.quit;
                    app.apply(event);
                    if is_quit {
                        exited = true;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    exited = true;
                    break;
                }
            }
        }
        if exited {
            break;
        }
        guard
            .terminal
            .draw(|frame| app.draw(frame))
            .map_err(|e| format!("tui draw failed: {e}"))?;
        if let Some(outcome) = app.take_outcome() {
            let request = match outcome {
                crate::tui::AppOutcome::Submit(text) => SubmitRequest::Submit(text),
                crate::tui::AppOutcome::Command { name: _, raw } => {
                    // For now treat command submissions as plain text.
                    SubmitRequest::Submit(raw)
                }
                crate::tui::AppOutcome::Exit => SubmitRequest::Exit,
            };
            let should_exit = matches!(request, SubmitRequest::Exit);
            if submit_tx.send(request).is_err() {
                break;
            }
            if should_exit {
                break;
            }
        }
        if crossterm::event::poll(Duration::from_millis(50))
            .map_err(|e| format!("event poll failed: {e}"))?
        {
            if let crossterm::event::Event::Key(key) =
                crossterm::event::read().map_err(|e| format!("event read failed: {e}"))?
            {
                if matches!(
                    key.kind,
                    crossterm::event::KeyEventKind::Press | crossterm::event::KeyEventKind::Repeat
                ) {
                    if let Some(outcome) = app.handle_key(key) {
                        let request = match outcome {
                            crate::tui::AppOutcome::Submit(text) => SubmitRequest::Submit(text),
                            crate::tui::AppOutcome::Command { name: _, raw } => {
                                SubmitRequest::Submit(raw)
                            }
                            crate::tui::AppOutcome::Exit => SubmitRequest::Exit,
                        };
                        let should_exit = matches!(request, SubmitRequest::Exit);
                        if submit_tx.send(request).is_err() {
                            break;
                        }
                        if should_exit {
                            exited = true;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn cli_conversation_message(role: &str, content: &str) -> AgentConversationMessage {
    AgentConversationMessage {
        role: role.to_string(),
        content: content.to_string(),
        attachments: Vec::new(),
    }
}

fn write_user_band_line<W: Write>(stdout: &mut W, text: &str, width: usize) -> Result<(), String> {
    let fill = " ".repeat(width);
    write!(stdout, "{CODEX_COMPOSER_BG}{fill}\x1b[0m").map_err(|e| e.to_string())?;
    write!(stdout, "\r{CODEX_COMPOSER_BG}").map_err(|e| e.to_string())?;
    write_styled_prompt_line(stdout, text, width)?;
    write!(stdout, "\x1b[0m").map_err(|e| e.to_string())
}

struct CliScreenGuard {
    origin_row: u16,
}

impl CliScreenGuard {
    fn enter() -> Result<Self, String> {
        let mut stdout = io::stdout();
        let (col, mut origin_row) = cursor::position().map_err(|e| e.to_string())?;
        if col > 0 {
            writeln!(stdout).map_err(|e| e.to_string())?;
            stdout.flush().map_err(|e| e.to_string())?;
            origin_row = cursor::position().map_err(|e| e.to_string())?.1;
        }
        execute!(
            stdout,
            ResetColor,
            cursor::MoveTo(0, origin_row),
            cursor::SetCursorStyle::BlinkingBar,
            cursor::Show
        )
        .map_err(|e| e.to_string())?;
        Ok(Self { origin_row })
    }

    fn origin_row(&self) -> u16 {
        self.origin_row
    }
}

impl Drop for CliScreenGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        let _ = execute!(
            stdout,
            ResetColor,
            cursor::SetCursorStyle::DefaultUserShape,
            cursor::Show
        );
    }
}

struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        let _ = execute!(stdout, DisableBracketedPaste);
        let _ = disable_raw_mode();
    }
}

struct InlineScreenGuard {
    start_row: u16,
}

impl InlineScreenGuard {
    fn enter(header_visible: bool, preferred_start_row: u16) -> Result<Self, String> {
        let max_start_row = codex_inline_start_row(header_visible);
        let start_row = preferred_start_row.min(max_start_row);
        Ok(Self { start_row })
    }

    fn start_row(&self) -> u16 {
        self.start_row
    }

    fn set_start_row(&mut self, start_row: u16) {
        self.start_row = start_row;
    }

    fn clear(&self) -> Result<(), String> {
        self.clear_interaction(false)
    }

    fn clear_interaction(&self, header_visible: bool) -> Result<(), String> {
        let mut stdout = io::stdout();
        let header_rows = if header_visible {
            codex_chat_header_height()
        } else {
            0
        };
        let start_row = self.start_row.saturating_add(header_rows);
        queue!(
            stdout,
            ResetColor,
            cursor::MoveTo(0, start_row),
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
    history: &mut Vec<String>,
    header_visible: bool,
    start_row: u16,
    origin_row: u16,
    blocks: &[RenderedBlock],
) -> Result<Option<(String, bool, u16)>, String> {
    if !stdin.is_terminal() {
        return read_stdin_line(stdin)
            .map(|line| line.map(|line| (line, header_visible, start_row)));
    }

    read_interactive_chat_input(
        prompt,
        session,
        history,
        header_visible,
        start_row,
        origin_row,
        blocks,
    )
}

fn read_interactive_chat_input(
    prompt: &str,
    session: &CliSession,
    history: &mut Vec<String>,
    mut active_header_visible: bool,
    start_row: u16,
    origin_row: u16,
    blocks: &[RenderedBlock],
) -> Result<Option<(String, bool, u16)>, String> {
    enable_raw_mode().map_err(|e| e.to_string())?;
    let raw_mode = RawModeGuard;
    execute!(io::stdout(), EnableBracketedPaste).map_err(|e| e.to_string())?;
    let mut inline_screen = InlineScreenGuard::enter(active_header_visible, start_row)?;
    let mut line = String::new();
    let mut cursor_index = 0usize;
    let mut selected_command_index = 0usize;
    let mut history_index: Option<usize> = None;
    let mut history_draft = String::new();
    inline_screen.set_start_row(render_chat_input(
        prompt,
        &line,
        cursor_index,
        selected_command_index,
        session,
        inline_screen.start_row(),
        active_header_visible,
        active_header_visible,
        blocks,
    )?);

    loop {
        match event::read().map_err(|e| e.to_string())? {
            Event::Key(KeyEvent {
                code,
                modifiers,
                kind,
                ..
            }) if matches!(kind, KeyEventKind::Press | KeyEventKind::Repeat) => match code {
                KeyCode::Enter if modifiers.contains(KeyModifiers::SHIFT) => {
                    history_index = None;
                    insert_char_at(&mut line, cursor_index, '\n');
                    cursor_index += 1;
                    selected_command_index = 0;
                    inline_screen.set_start_row(render_chat_input(
                        prompt,
                        &line,
                        cursor_index,
                        selected_command_index,
                        session,
                        inline_screen.start_row(),
                        active_header_visible,
                        false,
                        blocks,
                    )?);
                }
                KeyCode::Enter => {
                    if line.trim().is_empty() {
                        // Empty (or whitespace-only) submissions are ignored.
                        // Do not re-render or submit; just stay at the prompt.
                        continue;
                    }
                    let submitted = if line.trim() == "/" {
                        line.clone()
                    } else {
                        selected_slash_command(&line, selected_command_index)
                            .map(str::to_string)
                            .unwrap_or_else(|| line.clone())
                    };
                    let submitted_start_row = inline_screen.start_row();
                    drop(inline_screen);
                    drop(raw_mode);
                    io::stdout().flush().map_err(|e| e.to_string())?;
                    record_chat_history(history, &submitted);
                    return Ok(Some((
                        submitted,
                        active_header_visible,
                        submitted_start_row,
                    )));
                }
                KeyCode::Backspace => {
                    history_index = None;
                    if modifiers.contains(KeyModifiers::CONTROL)
                        || modifiers.contains(KeyModifiers::ALT)
                    {
                        let word_start = previous_word_start(&line, cursor_index);
                        remove_char_range(&mut line, word_start, cursor_index);
                        cursor_index = word_start;
                    } else if cursor_index > 0 {
                        remove_char_at(&mut line, cursor_index - 1);
                        cursor_index = cursor_index.saturating_sub(1);
                    }
                    selected_command_index = 0;
                    inline_screen.set_start_row(render_chat_input(
                        prompt,
                        &line,
                        cursor_index,
                        selected_command_index,
                        session,
                        inline_screen.start_row(),
                        active_header_visible,
                        false,
                        blocks,
                    )?);
                }
                KeyCode::Up | KeyCode::Char('p') | KeyCode::Char('P')
                    if matches!(code, KeyCode::Up) || modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    if let Some(count) = slash_command_match_count(&line) {
                        selected_command_index = if selected_command_index == 0 {
                            count.saturating_sub(1)
                        } else {
                            selected_command_index.saturating_sub(1)
                        };
                        inline_screen.set_start_row(render_chat_input(
                            prompt,
                            &line,
                            cursor_index,
                            selected_command_index,
                            session,
                            inline_screen.start_row(),
                            active_header_visible,
                            false,
                            blocks,
                        )?);
                    } else if !history.is_empty() {
                        let next_index = match history_index {
                            Some(index) => index.saturating_sub(1),
                            None => {
                                history_draft = line.clone();
                                history.len().saturating_sub(1)
                            }
                        };
                        history_index = Some(next_index);
                        line = history[next_index].clone();
                        cursor_index = line.chars().count();
                        selected_command_index = 0;
                        inline_screen.set_start_row(render_chat_input(
                            prompt,
                            &line,
                            cursor_index,
                            selected_command_index,
                            session,
                            inline_screen.start_row(),
                            active_header_visible,
                            false,
                            blocks,
                        )?);
                    }
                }
                KeyCode::Down | KeyCode::Char('n') | KeyCode::Char('N')
                    if matches!(code, KeyCode::Down)
                        || modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    if let Some(count) = slash_command_match_count(&line) {
                        selected_command_index = (selected_command_index + 1) % count.max(1);
                        inline_screen.set_start_row(render_chat_input(
                            prompt,
                            &line,
                            cursor_index,
                            selected_command_index,
                            session,
                            inline_screen.start_row(),
                            active_header_visible,
                            false,
                            blocks,
                        )?);
                    } else if let Some(index) = history_index {
                        if index + 1 < history.len() {
                            let next_index = index + 1;
                            history_index = Some(next_index);
                            line = history[next_index].clone();
                        } else {
                            history_index = None;
                            line = history_draft.clone();
                            history_draft.clear();
                        }
                        cursor_index = line.chars().count();
                        selected_command_index = 0;
                        inline_screen.set_start_row(render_chat_input(
                            prompt,
                            &line,
                            cursor_index,
                            selected_command_index,
                            session,
                            inline_screen.start_row(),
                            active_header_visible,
                            false,
                            blocks,
                        )?);
                    }
                }
                KeyCode::PageUp => {
                    if let Some(count) = slash_command_match_count(&line) {
                        let page_size =
                            chat_popup_page_size(inline_screen.start_row(), active_header_visible);
                        selected_command_index = selected_command_index.saturating_sub(page_size);
                        selected_command_index =
                            selected_command_index.min(count.saturating_sub(1));
                        inline_screen.set_start_row(render_chat_input(
                            prompt,
                            &line,
                            cursor_index,
                            selected_command_index,
                            session,
                            inline_screen.start_row(),
                            active_header_visible,
                            false,
                            blocks,
                        )?);
                    }
                }
                KeyCode::PageDown => {
                    if let Some(count) = slash_command_match_count(&line) {
                        let page_size =
                            chat_popup_page_size(inline_screen.start_row(), active_header_visible);
                        selected_command_index = selected_command_index
                            .saturating_add(page_size)
                            .min(count.saturating_sub(1));
                        inline_screen.set_start_row(render_chat_input(
                            prompt,
                            &line,
                            cursor_index,
                            selected_command_index,
                            session,
                            inline_screen.start_row(),
                            active_header_visible,
                            false,
                            blocks,
                        )?);
                    }
                }
                KeyCode::Tab => {
                    if let Some(command) = selected_slash_command(&line, selected_command_index) {
                        history_index = None;
                        line.clear();
                        line.push_str(command);
                        cursor_index = line.chars().count();
                        selected_command_index = 0;
                        inline_screen.set_start_row(render_chat_input(
                            prompt,
                            &line,
                            cursor_index,
                            selected_command_index,
                            session,
                            inline_screen.start_row(),
                            active_header_visible,
                            false,
                            blocks,
                        )?);
                    }
                }
                KeyCode::Esc => {
                    if line.starts_with('/') {
                        line.clear();
                        cursor_index = 0;
                        selected_command_index = 0;
                        inline_screen.set_start_row(render_chat_input(
                            prompt,
                            &line,
                            cursor_index,
                            selected_command_index,
                            session,
                            inline_screen.start_row(),
                            active_header_visible,
                            false,
                            blocks,
                        )?);
                        continue;
                    }
                    line.clear();
                    cursor_index = 0;
                    selected_command_index = 0;
                    history_index = None;
                    history_draft.clear();
                    inline_screen.set_start_row(render_chat_input(
                        prompt,
                        &line,
                        cursor_index,
                        selected_command_index,
                        session,
                        inline_screen.start_row(),
                        active_header_visible,
                        false,
                        blocks,
                    )?);
                }
                KeyCode::Char('c') | KeyCode::Char('C')
                    if modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    clear_chat_runtime_area(
                        inline_screen.start_row(),
                        active_header_visible,
                        blocks.is_empty(),
                    )?;
                    drop(inline_screen);
                    drop(raw_mode);
                    println!();
                    io::stdout().flush().map_err(|e| e.to_string())?;
                    return Ok(None);
                }
                KeyCode::Char('d') | KeyCode::Char('D')
                    if modifiers.contains(KeyModifiers::ALT) =>
                {
                    history_index = None;
                    let delete_end = next_word_delete_end(&line, cursor_index);
                    remove_char_range(&mut line, cursor_index, delete_end);
                    selected_command_index = 0;
                    inline_screen.set_start_row(render_chat_input(
                        prompt,
                        &line,
                        cursor_index,
                        selected_command_index,
                        session,
                        inline_screen.start_row(),
                        active_header_visible,
                        false,
                        blocks,
                    )?);
                }
                KeyCode::Char('d') | KeyCode::Char('D')
                    if modifiers.contains(KeyModifiers::CONTROL) && line.is_empty() =>
                {
                    clear_chat_runtime_area(
                        inline_screen.start_row(),
                        active_header_visible,
                        blocks.is_empty(),
                    )?;
                    drop(inline_screen);
                    drop(raw_mode);
                    println!();
                    io::stdout().flush().map_err(|e| e.to_string())?;
                    return Ok(None);
                }
                KeyCode::Char('d') | KeyCode::Char('D')
                    if modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    history_index = None;
                    if cursor_index < line.chars().count() {
                        remove_char_at(&mut line, cursor_index);
                        selected_command_index = 0;
                    }
                    inline_screen.set_start_row(render_chat_input(
                        prompt,
                        &line,
                        cursor_index,
                        selected_command_index,
                        session,
                        inline_screen.start_row(),
                        active_header_visible,
                        false,
                        blocks,
                    )?);
                }
                KeyCode::Char('a') | KeyCode::Char('A')
                    if modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    cursor_index = 0;
                    inline_screen.set_start_row(render_chat_input(
                        prompt,
                        &line,
                        cursor_index,
                        selected_command_index,
                        session,
                        inline_screen.start_row(),
                        active_header_visible,
                        false,
                        blocks,
                    )?);
                }
                KeyCode::Char('e') | KeyCode::Char('E')
                    if modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    cursor_index = line.chars().count();
                    inline_screen.set_start_row(render_chat_input(
                        prompt,
                        &line,
                        cursor_index,
                        selected_command_index,
                        session,
                        inline_screen.start_row(),
                        active_header_visible,
                        false,
                        blocks,
                    )?);
                }
                KeyCode::Char('u') | KeyCode::Char('U')
                    if modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    history_index = None;
                    remove_char_range(&mut line, 0, cursor_index);
                    cursor_index = 0;
                    selected_command_index = 0;
                    inline_screen.set_start_row(render_chat_input(
                        prompt,
                        &line,
                        cursor_index,
                        selected_command_index,
                        session,
                        inline_screen.start_row(),
                        active_header_visible,
                        false,
                        blocks,
                    )?);
                }
                KeyCode::Char('k') | KeyCode::Char('K')
                    if modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    history_index = None;
                    let end_index = line.chars().count();
                    remove_char_range(&mut line, cursor_index, end_index);
                    selected_command_index = 0;
                    inline_screen.set_start_row(render_chat_input(
                        prompt,
                        &line,
                        cursor_index,
                        selected_command_index,
                        session,
                        inline_screen.start_row(),
                        active_header_visible,
                        false,
                        blocks,
                    )?);
                }
                KeyCode::Char('l') | KeyCode::Char('L')
                    if modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    history_index = None;
                    history_draft.clear();
                    active_header_visible = true;
                    inline_screen.set_start_row(0);
                    let mut stdout = io::stdout();
                    queue!(
                        stdout,
                        ResetColor,
                        cursor::MoveTo(0, 0),
                        Clear(ClearType::All)
                    )
                    .map_err(|e| e.to_string())?;
                    inline_screen.set_start_row(render_chat_input(
                        prompt,
                        &line,
                        cursor_index,
                        selected_command_index,
                        session,
                        inline_screen.start_row(),
                        true,
                        true,
                        blocks,
                    )?);
                }
                KeyCode::Char('w') | KeyCode::Char('W')
                    if modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    history_index = None;
                    let word_start = previous_word_start(&line, cursor_index);
                    remove_char_range(&mut line, word_start, cursor_index);
                    cursor_index = word_start;
                    selected_command_index = 0;
                    inline_screen.set_start_row(render_chat_input(
                        prompt,
                        &line,
                        cursor_index,
                        selected_command_index,
                        session,
                        inline_screen.start_row(),
                        active_header_visible,
                        false,
                        blocks,
                    )?);
                }
                KeyCode::Left => {
                    cursor_index = if modifiers.contains(KeyModifiers::CONTROL)
                        || modifiers.contains(KeyModifiers::ALT)
                    {
                        previous_word_start(&line, cursor_index)
                    } else {
                        cursor_index.saturating_sub(1)
                    };
                    inline_screen.set_start_row(render_chat_input(
                        prompt,
                        &line,
                        cursor_index,
                        selected_command_index,
                        session,
                        inline_screen.start_row(),
                        active_header_visible,
                        false,
                        blocks,
                    )?);
                }
                KeyCode::Right => {
                    cursor_index = if modifiers.contains(KeyModifiers::CONTROL)
                        || modifiers.contains(KeyModifiers::ALT)
                    {
                        next_word_start(&line, cursor_index)
                    } else {
                        (cursor_index + 1).min(line.chars().count())
                    };
                    inline_screen.set_start_row(render_chat_input(
                        prompt,
                        &line,
                        cursor_index,
                        selected_command_index,
                        session,
                        inline_screen.start_row(),
                        active_header_visible,
                        false,
                        blocks,
                    )?);
                }
                KeyCode::Home => {
                    if slash_command_match_count(&line).is_some() {
                        selected_command_index = 0;
                    } else {
                        cursor_index = 0;
                    }
                    inline_screen.set_start_row(render_chat_input(
                        prompt,
                        &line,
                        cursor_index,
                        selected_command_index,
                        session,
                        inline_screen.start_row(),
                        active_header_visible,
                        false,
                        blocks,
                    )?);
                }
                KeyCode::End => {
                    if let Some(count) = slash_command_match_count(&line) {
                        selected_command_index = count.saturating_sub(1);
                    } else {
                        cursor_index = line.chars().count();
                    }
                    inline_screen.set_start_row(render_chat_input(
                        prompt,
                        &line,
                        cursor_index,
                        selected_command_index,
                        session,
                        inline_screen.start_row(),
                        active_header_visible,
                        false,
                        blocks,
                    )?);
                }
                KeyCode::Delete => {
                    history_index = None;
                    if modifiers.contains(KeyModifiers::CONTROL) {
                        let delete_end = next_word_delete_end(&line, cursor_index);
                        remove_char_range(&mut line, cursor_index, delete_end);
                        selected_command_index = 0;
                    } else if cursor_index < line.chars().count() {
                        remove_char_at(&mut line, cursor_index);
                        selected_command_index = 0;
                    }
                    inline_screen.set_start_row(render_chat_input(
                        prompt,
                        &line,
                        cursor_index,
                        selected_command_index,
                        session,
                        inline_screen.start_row(),
                        active_header_visible,
                        false,
                        blocks,
                    )?);
                }
                KeyCode::Char(ch)
                    if !modifiers.contains(KeyModifiers::CONTROL)
                        && !modifiers.contains(KeyModifiers::ALT) =>
                {
                    if ch == '/' && line == "/" {
                        continue;
                    }
                    history_index = None;
                    let Some(ch) = normalize_typed_char(ch) else {
                        continue;
                    };
                    insert_char_at(&mut line, cursor_index, ch);
                    cursor_index += 1;
                    selected_command_index = 0;
                    inline_screen.set_start_row(render_chat_input(
                        prompt,
                        &line,
                        cursor_index,
                        selected_command_index,
                        session,
                        inline_screen.start_row(),
                        active_header_visible,
                        false,
                        blocks,
                    )?);
                }
                _ => {}
            },
            Event::Paste(pasted) => {
                history_index = None;
                let pasted = normalize_pasted_input(&pasted);
                let pasted_len = pasted.chars().count();
                insert_str_at(&mut line, cursor_index, &pasted);
                cursor_index += pasted_len;
                selected_command_index = 0;
                inline_screen.set_start_row(render_chat_input(
                    prompt,
                    &line,
                    cursor_index,
                    selected_command_index,
                    session,
                    inline_screen.start_row(),
                    active_header_visible,
                    false,
                    blocks,
                )?);
            }
            Event::Resize(_, _) => {
                // Resize: re-anchor the composer at the session's origin
                // row and let `render_chat_input` repaint the layout into
                // the alternate screen buffer. This mirrors how codex-rs/tui
                // handles resize: it maps the event to a redraw trigger and
                // re-emits the whole frame. The alt-screen (entered in
                // `CliScreenGuard::enter`) is the only reason this is
                // sufficient — the host terminal's scrollback never sees any
                // of the chat output, so resize can't surface stale cells.
                inline_screen.set_start_row(origin_row);
                inline_screen.set_start_row(render_chat_input(
                    prompt,
                    &line,
                    cursor_index,
                    selected_command_index,
                    session,
                    inline_screen.start_row(),
                    active_header_visible,
                    true,
                    blocks,
                )?);
            }
            Event::Key(_) => {}
            _ => {}
        }
    }
}

fn render_codex_startup_tip(stdout: &mut io::Stdout, cols: u16, row: u16) -> Result<(), String> {
    let width = (cols as usize).max(1);
    let tip = "Tip: Type / for commands. Use /model to change model.";
    let line = truncate_display_width(tip, width);
    queue!(
        stdout,
        ResetColor,
        cursor::MoveTo(0, row),
        Clear(ClearType::CurrentLine)
    )
    .map_err(|e| e.to_string())?;
    write!(stdout, "\x1b[1mTip:\x1b[22m {}", &line[5.min(line.len())..]).map_err(|e| e.to_string())
}

fn render_chat_input(
    prompt: &str,
    line: &str,
    cursor_index: usize,
    selected_command_index: usize,
    session: &CliSession,
    start_row: u16,
    header_visible: bool,
    redraw_header: bool,
    blocks: &[RenderedBlock],
) -> Result<u16, String> {
    let mut stdout = io::stdout();
    let popup_lines = if line.starts_with('/') {
        slash_command_popup_lines(line, selected_command_index)
    } else {
        Vec::new()
    };
    let (cols, terminal_rows) = crossterm::terminal::size().map_err(|e| e.to_string())?;
    let header_rows = if header_visible {
        codex_chat_header_height()
    } else {
        0
    };

    let max_composer_height = terminal_rows
        .saturating_sub(header_rows + CODEX_COMPOSER_FOOTER_GAP + CODEX_FOOTER_HEIGHT)
        .max(CODEX_COMPOSER_MIN_ROWS);
    let composer_height = codex_composer_height_for_input(cols, prompt, line, cursor_index)
        .min(max_composer_height)
        .max(CODEX_COMPOSER_MIN_ROWS);
    let bottom_reserved = composer_height
        .saturating_add(CODEX_COMPOSER_FOOTER_GAP)
        .saturating_add(CODEX_FOOTER_HEIGHT);
    let max_popup_rows = if popup_lines.is_empty() {
        0usize
    } else {
        terminal_rows
            .saturating_sub(header_rows.saturating_add(bottom_reserved))
            .min(CODEX_POPUP_MAX_ROWS) as usize
    };
    let visible_popup_rows = popup_lines.len().min(max_popup_rows);
    let scroll_start = slash_popup_scroll_start(
        selected_command_index,
        popup_lines.len(),
        visible_popup_rows,
    );
    let visible_lines = popup_lines
        .iter()
        .skip(scroll_start)
        .take(visible_popup_rows)
        .cloned()
        .collect::<Vec<_>>();

    // Pre-compute the per-block line breakdown for the rendered transcript
    // history. We compute the lines for every block first, then later decide
    // which tail to display inside the visible viewport.
    let width = (cols as usize).max(1);
    let block_lines: Vec<Vec<String>> = blocks.iter().map(|block| block.lines_for(width)).collect();
    let block_line_counts: Vec<usize> = block_lines.iter().map(|lines| lines.len()).collect();
    let total_block_lines: usize = block_line_counts.iter().sum();

    let blocks_are_empty = total_block_lines == 0;
    let compact_startup = blocks_are_empty && header_visible && visible_popup_rows == 0;
    let tip_rows = if compact_startup {
        CODEX_STARTUP_TIP_ROWS
    } else {
        0
    };
    let top_rows = header_rows.saturating_add(tip_rows);
    let layout_height = top_rows
        .saturating_add(visible_popup_rows as u16)
        .saturating_add(composer_height)
        .saturating_add(CODEX_COMPOSER_FOOTER_GAP)
        .saturating_add(CODEX_FOOTER_HEIGHT)
        .max(1);
    let layout_start = reserve_terminal_rows(start_row, layout_height)?;
    let safe_rows = terminal_rows.saturating_sub(1).max(1);
    let bottom_start = safe_rows.saturating_sub(
        composer_height
            .saturating_add(visible_popup_rows as u16)
            .saturating_add(CODEX_COMPOSER_FOOTER_GAP)
            .saturating_add(CODEX_FOOTER_HEIGHT),
    );
    let composer_start = if compact_startup {
        layout_start.saturating_add(top_rows)
    } else {
        bottom_start
    };
    let popup_start = composer_start.saturating_add(composer_height);
    let footer_row = composer_start
        .saturating_add(composer_height)
        .saturating_add(visible_popup_rows as u16)
        .saturating_add(CODEX_COMPOSER_FOOTER_GAP)
        .min(safe_rows.saturating_sub(1));
    let clear_from = layout_start;
    let clear_to: u16 = if compact_startup {
        footer_row.saturating_add(1).min(terminal_rows)
    } else {
        terminal_rows
    };
    for row in clear_from..clear_to {
        queue!(
            stdout,
            ResetColor,
            cursor::MoveTo(0, row),
            Clear(ClearType::CurrentLine)
        )
        .map_err(|e| e.to_string())?;
    }

    // Paint blocks above the composer. If the transcript history would push
    // the composer past the bottom of the viewport, we drop the oldest
    // blocks (from the front) so only the most recent ones render. The
    // dropped blocks remain in `block_line_counts` for the caller to use
    // when computing the next `input_start_row`.
    let blocks_start = layout_start.saturating_add(top_rows);
    let blocks_band_height: u16 = composer_start.saturating_sub(blocks_start).min(safe_rows);
    let mut consumed: usize = 0;
    let mut blocks_to_render: Vec<&[String]> = Vec::new();
    for lines in block_lines.iter().rev() {
        if consumed.saturating_add(lines.len()) > blocks_band_height as usize {
            break;
        }
        consumed = consumed.saturating_add(lines.len());
        blocks_to_render.push(lines.as_slice());
    }
    blocks_to_render.reverse();
    let mut row_cursor: u16 = blocks_start;
    for lines in blocks_to_render {
        for text in lines {
            let row = row_cursor;
            if row >= blocks_start.saturating_add(blocks_band_height) {
                break;
            }
            queue!(
                stdout,
                ResetColor,
                cursor::MoveTo(0, row),
                Clear(ClearType::CurrentLine)
            )
            .map_err(|e| e.to_string())?;
            write!(stdout, "{text}").map_err(|e| e.to_string())?;
            row_cursor = row_cursor.saturating_add(1);
        }
    }

    if header_visible && (redraw_header || compact_startup) {
        render_codex_chat_header(&mut stdout, cols, layout_start, session)?;
    }
    if tip_rows > 0 {
        render_codex_startup_tip(
            &mut stdout,
            cols,
            layout_start.saturating_add(header_rows).saturating_add(1),
        )?;
    }

    let scrollbar_col = cols.saturating_sub(2);
    let scrollbar_thumb =
        popup_scrollbar_thumb(popup_lines.len(), visible_lines.len(), scroll_start);
    for (index, text) in visible_lines.iter().enumerate() {
        let row = popup_start.saturating_add(index as u16);
        queue!(
            stdout,
            ResetColor,
            cursor::MoveTo(0, row),
            Clear(ClearType::CurrentLine)
        )
        .map_err(|e| e.to_string())?;
        write!(stdout, "{text}").map_err(|e| e.to_string())?;
        if popup_lines.len() > visible_lines.len() {
            let marker = if Some(index) == scrollbar_thumb {
                "█"
            } else {
                "│"
            };
            queue!(stdout, cursor::MoveTo(scrollbar_col, row)).map_err(|e| e.to_string())?;
            write!(stdout, "\x1b[2m{marker}\x1b[0m").map_err(|e| e.to_string())?;
        }
    }

    let (cursor_col, cursor_row) = render_composer_band(
        &mut stdout,
        cols,
        composer_start,
        composer_height,
        prompt,
        line,
        cursor_index,
    )?;
    render_codex_footer(&mut stdout, cols, footer_row, session)?;
    queue!(stdout, ResetColor, cursor::MoveTo(cursor_col, cursor_row))
        .map_err(|e| e.to_string())?;
    stdout.flush().map_err(|e| e.to_string())?;
    Ok(layout_start)
}

fn finalize_submitted_input(
    prompt: &str,
    submitted: &str,
    layout_start: u16,
    header_visible: bool,
) -> Result<u16, String> {
    let mut stdout = io::stdout();
    let (cols, terminal_rows) = crossterm::terminal::size().map_err(|e| e.to_string())?;
    let width = cols.max(1) as usize;
    let composer_height =
        codex_composer_height_for_input(cols, prompt, submitted, submitted.chars().count());
    let visible_popup_rows = if submitted.starts_with('/') {
        slash_command_popup_lines(submitted, 0)
            .len()
            .min(CODEX_POPUP_MAX_ROWS as usize) as u16
    } else {
        0
    };
    let cleanup_rows = composer_height
        .saturating_add(visible_popup_rows)
        .saturating_add(CODEX_COMPOSER_FOOTER_GAP)
        .saturating_add(CODEX_FOOTER_HEIGHT);
    let band_lines = user_band_lines(prompt, submitted, width);
    let redraw_rows = cleanup_rows.max(band_lines.len() as u16);
    let safe_rows = terminal_rows.saturating_sub(1).max(1);
    let composer_start = if header_visible {
        layout_start
            .saturating_add(codex_chat_header_height())
            .saturating_add(CODEX_STARTUP_TIP_ROWS)
    } else {
        safe_rows.saturating_sub(cleanup_rows)
    };
    for offset in 0..redraw_rows {
        let row = composer_start.saturating_add(offset);
        if row >= safe_rows {
            break;
        }
        queue!(
            stdout,
            ResetColor,
            cursor::MoveTo(0, row),
            Clear(ClearType::CurrentLine)
        )
        .map_err(|e| e.to_string())?;
    }
    let fill = " ".repeat(width);
    for offset in 0..composer_height {
        let row = composer_start.saturating_add(offset);
        if row >= safe_rows {
            break;
        }
        queue!(stdout, cursor::MoveTo(0, row)).map_err(|e| e.to_string())?;
        write!(stdout, "{CODEX_COMPOSER_BG}{fill}\x1b[0m").map_err(|e| e.to_string())?;
    }
    for (index, text) in band_lines.iter().enumerate() {
        let row = composer_start
            .saturating_add(1)
            .saturating_add(index as u16);
        if row >= safe_rows {
            break;
        }
        queue!(stdout, cursor::MoveTo(0, row)).map_err(|e| e.to_string())?;
        write_user_band_line(&mut stdout, text, width)?;
    }
    let next_row = composer_start.saturating_add(composer_height);
    let _ = layout_start;
    move_to_append_row(&mut stdout, next_row.min(safe_rows))
}

fn clear_submitted_input_area(layout_start: u16, _header_visible: bool) -> Result<u16, String> {
    let mut stdout = io::stdout();
    let (_, terminal_rows) = crossterm::terminal::size().map_err(|e| e.to_string())?;
    let safe_rows = terminal_rows.saturating_sub(1).max(1);
    let start_row = safe_rows.saturating_sub(
        CODEX_COMPOSER_MIN_ROWS
            .saturating_add(CODEX_COMPOSER_FOOTER_GAP)
            .saturating_add(CODEX_FOOTER_HEIGHT),
    );
    for row in start_row..safe_rows {
        queue!(
            stdout,
            ResetColor,
            cursor::MoveTo(0, row),
            Clear(ClearType::CurrentLine)
        )
        .map_err(|e| e.to_string())?;
    }
    let _ = layout_start;
    move_to_append_row(&mut stdout, start_row.min(safe_rows))
}

fn clear_chat_runtime_area(
    layout_start: u16,
    header_visible: bool,
    blocks_are_empty: bool,
) -> Result<u16, String> {
    let mut stdout = io::stdout();
    let (_, terminal_rows) = crossterm::terminal::size().map_err(|e| e.to_string())?;
    let safe_rows = terminal_rows.saturating_sub(1).max(1);
    let start_row = if header_visible && blocks_are_empty {
        layout_start
            .saturating_add(codex_chat_header_height())
            .saturating_add(CODEX_STARTUP_TIP_ROWS)
    } else {
        layout_start
    }
    .min(safe_rows);
    for row in start_row..safe_rows {
        queue!(
            stdout,
            ResetColor,
            cursor::MoveTo(0, row),
            Clear(ClearType::CurrentLine)
        )
        .map_err(|e| e.to_string())?;
    }
    move_to_append_row(&mut stdout, start_row)
}

fn user_band_lines(prompt: &str, content: &str, width: usize) -> Vec<String> {
    let display = composer_display(prompt, content, content.chars().count(), width);
    display.lines
}

fn current_append_row() -> Result<u16, String> {
    let mut stdout = io::stdout();
    let (col, row) = cursor::position().map_err(|e| e.to_string())?;
    let row = if col > 0 {
        writeln!(stdout).map_err(|e| e.to_string())?;
        stdout.flush().map_err(|e| e.to_string())?;
        cursor::position().map_err(|e| e.to_string())?.1
    } else {
        row
    };
    Ok(row)
}

fn next_chat_input_start_row() -> Result<u16, String> {
    Ok(compute_next_chat_input_start_row(current_append_row()?))
}

fn compute_next_chat_input_start_row(current_row: u16) -> u16 {
    current_row.saturating_add(1)
}

fn reserve_terminal_rows(start_row: u16, required_rows: u16) -> Result<u16, String> {
    let mut stdout = io::stdout();
    let (_, terminal_rows) = crossterm::terminal::size().map_err(|e| e.to_string())?;
    let terminal_rows = terminal_rows.max(1);
    let required_rows = required_rows.max(1).min(terminal_rows);
    // Use terminal_rows - 1 as effective height to keep the bottom row blank,
    // so content never renders flush against the terminal bottom edge.
    let effective_rows = terminal_rows.saturating_sub(1).max(1);
    // Clamp start_row to the largest value that still fits `required_rows`
    // inside the new viewport. Without this, a start_row computed against the
    // old (larger) height would push the composer below the new viewport and
    // trigger a ScrollUp that consumes the chat history sitting in the
    // terminal's scrollback buffer.
    let max_start = effective_rows.saturating_sub(required_rows);
    let start_row = start_row.min(max_start);
    let overflow_rows = start_row
        .saturating_add(required_rows)
        .saturating_sub(effective_rows);

    if overflow_rows == 0 {
        return Ok(start_row);
    }

    queue!(stdout, ResetColor, ScrollUp(overflow_rows)).map_err(|e| e.to_string())?;
    stdout.flush().map_err(|e| e.to_string())?;
    Ok(start_row.saturating_sub(overflow_rows))
}

fn move_to_append_row(stdout: &mut io::Stdout, row: u16) -> Result<u16, String> {
    let (_, terminal_rows) = crossterm::terminal::size().map_err(|e| e.to_string())?;
    let terminal_rows = terminal_rows.max(1);
    // Keep the bottom row blank; cursor should not land on the last row.
    let safe_row = terminal_rows.saturating_sub(1);
    if row < safe_row {
        queue!(stdout, ResetColor, cursor::MoveTo(0, row)).map_err(|e| e.to_string())?;
        stdout.flush().map_err(|e| e.to_string())?;
        return Ok(row);
    }
    let scroll_rows = row.saturating_sub(safe_row).saturating_add(1);
    queue!(stdout, ResetColor, ScrollUp(scroll_rows)).map_err(|e| e.to_string())?;
    stdout.flush().map_err(|e| e.to_string())?;
    Ok(safe_row.saturating_sub(1))
}

struct CliWorkingStatus {
    row: u16,
    clear_end_row: u16,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl CliWorkingStatus {
    fn start() -> Result<Self, String> {
        let mut stdout = io::stdout();
        let row = current_append_row()?.saturating_add(1);
        let row = reserve_terminal_rows(row, working_status_layout_height())?;
        queue!(stdout, cursor::Hide).map_err(|e| e.to_string())?;
        stdout.flush().map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let start = Instant::now();
        render_working_status(row, start)?;
        let thread_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(32));
                if thread_stop.load(Ordering::Relaxed) {
                    break;
                }
                let _ = render_working_status(row, start);
            }
        });
        Ok(Self {
            row,
            clear_end_row: row,
            stop,
            handle: Some(handle),
        })
    }

    fn finish(mut self) -> Result<u16, String> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let mut stdout = io::stdout();
        let (_, terminal_rows) = crossterm::terminal::size().map_err(|e| e.to_string())?;
        let clear_end_row = self.clear_end_row.min(terminal_rows.saturating_sub(1));
        for row in self.row..=clear_end_row {
            queue!(
                stdout,
                ResetColor,
                cursor::MoveTo(0, row),
                Clear(ClearType::CurrentLine)
            )
            .map_err(|e| e.to_string())?;
        }
        queue!(
            stdout,
            ResetColor,
            cursor::Show,
            cursor::MoveTo(0, self.row)
        )
        .map_err(|e| e.to_string())?;
        stdout.flush().map_err(|e| e.to_string())?;
        Ok(self.row)
    }
}

fn working_status_layout_height() -> u16 {
    1
}

fn render_working_status(row: u16, start: Instant) -> Result<(), String> {
    let mut stdout = io::stdout();
    let elapsed = start.elapsed().as_secs();
    let marker = working_activity_marker(start);
    let label = shimmer_text_ansi("Working", start);
    queue!(
        stdout,
        ResetColor,
        cursor::MoveTo(0, row),
        Clear(ClearType::CurrentLine)
    )
    .map_err(|e| e.to_string())?;
    write!(
        stdout,
        "{marker} {label} \x1b[2m({elapsed}s • esc to interrupt)\x1b[0m"
    )
    .map_err(|e| e.to_string())?;
    stdout.flush().map_err(|e| e.to_string())
}

fn working_activity_marker(start: Instant) -> &'static str {
    let blink_on = (start.elapsed().as_millis() / 600).is_multiple_of(2);
    if blink_on {
        "•"
    } else {
        "\x1b[2m◦\x1b[22m"
    }
}

fn shimmer_text_ansi(text: &str, start: Instant) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return String::new();
    }
    let padding = 10usize;
    let period = chars.len().saturating_add(padding.saturating_mul(2));
    let sweep_seconds = 2.0f32;
    let pos_f = (start.elapsed().as_secs_f32() % sweep_seconds) / sweep_seconds * period as f32;
    let pos = pos_f as isize;
    let band_half_width = 5.0f32;
    let mut output = String::new();
    for (index, ch) in chars.into_iter().enumerate() {
        let char_pos = index as isize + padding as isize;
        let dist = (char_pos - pos).abs() as f32;
        let intensity = if dist <= band_half_width {
            let x = std::f32::consts::PI * (dist / band_half_width);
            0.5 * (1.0 + x.cos())
        } else {
            0.0
        };
        if intensity < 0.2 {
            output.push_str("\x1b[2m");
            output.push(ch);
            output.push_str("\x1b[22m");
        } else if intensity < 0.6 {
            output.push(ch);
        } else {
            output.push_str("\x1b[1m");
            output.push(ch);
            output.push_str("\x1b[22m");
        }
    }
    output
}

fn insert_char_at(text: &mut String, char_index: usize, ch: char) {
    let byte_index = byte_index_for_char(text, char_index);
    text.insert(byte_index, ch);
}

fn insert_str_at(text: &mut String, char_index: usize, value: &str) {
    let byte_index = byte_index_for_char(text, char_index);
    text.insert_str(byte_index, value);
}

fn normalize_pasted_input(value: &str) -> String {
    value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .map(|ch| match ch {
            '\n' => '\n',
            '\t' => ' ',
            _ if ch.is_control() => ' ',
            _ => ch,
        })
        .collect()
}

fn normalize_typed_char(ch: char) -> Option<char> {
    match ch {
        '\r' | '\n' | '\t' => Some(' '),
        _ if ch.is_control() => None,
        _ => Some(ch),
    }
}

fn remove_char_at(text: &mut String, char_index: usize) {
    let start = byte_index_for_char(text, char_index);
    let end = byte_index_for_char(text, char_index.saturating_add(1));
    if start < end && end <= text.len() {
        text.replace_range(start..end, "");
    }
}

fn remove_char_range(text: &mut String, start_char: usize, end_char: usize) {
    let start = byte_index_for_char(text, start_char);
    let end = byte_index_for_char(text, end_char);
    if start < end && end <= text.len() {
        text.replace_range(start..end, "");
    }
}

fn byte_index_for_char(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(index, _)| index)
        .unwrap_or(text.len())
}

fn previous_word_start(text: &str, cursor_index: usize) -> usize {
    let chars = text.chars().collect::<Vec<_>>();
    let mut index = cursor_index.min(chars.len());
    while index > 0 && chars[index - 1].is_whitespace() {
        index -= 1;
    }
    while index > 0 && !chars[index - 1].is_whitespace() {
        index -= 1;
    }
    index
}

fn next_word_start(text: &str, cursor_index: usize) -> usize {
    let chars = text.chars().collect::<Vec<_>>();
    let mut index = cursor_index.min(chars.len());
    while index < chars.len() && !chars[index].is_whitespace() {
        index += 1;
    }
    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    index
}

fn next_word_delete_end(text: &str, cursor_index: usize) -> usize {
    let chars = text.chars().collect::<Vec<_>>();
    let mut index = cursor_index.min(chars.len());
    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    while index < chars.len() && !chars[index].is_whitespace() {
        index += 1;
    }
    index
}

fn record_chat_history(history: &mut Vec<String>, submitted: &str) {
    let submitted = submitted.trim();
    if submitted.is_empty() {
        return;
    }
    if history
        .last()
        .map(|last| last.trim() == submitted)
        .unwrap_or(false)
    {
        return;
    }
    history.push(submitted.to_string());
    if history.len() > 200 {
        history.remove(0);
    }
}

fn codex_chat_header_height() -> u16 {
    6
}

fn render_codex_chat_header(
    stdout: &mut io::Stdout,
    cols: u16,
    start_row: u16,
    session: &CliSession,
) -> Result<(), String> {
    let width = cols.max(4) as usize;
    let title = format!(">_ CodeForge Codex (v{})", env!("CARGO_PKG_VERSION"));
    let model = codex_header_model_label(session);
    let directory = codex_footer_workspace_label(session);
    let model_line = format!("model:     {model}   /model to change");
    let directory_line = format!("directory: {directory}");
    let natural_inner_width = [
        terminal_display_width(&title),
        terminal_display_width(&model_line),
        terminal_display_width(&directory_line),
    ]
    .into_iter()
    .max()
    .unwrap_or(0)
    .saturating_add(2)
    .max(43);
    let inner_width = natural_inner_width.min(width.saturating_sub(2).max(1));
    let horizontal = "─".repeat(inner_width);
    write_header_box_line(stdout, start_row, &format!("╭{horizontal}╮"))?;
    write_header_content_line(stdout, start_row.saturating_add(1), &title, inner_width)?;
    write_header_content_line(stdout, start_row.saturating_add(2), "", inner_width)?;
    write_header_content_line(
        stdout,
        start_row.saturating_add(3),
        &model_line,
        inner_width,
    )?;
    write_header_content_line(
        stdout,
        start_row.saturating_add(4),
        &directory_line,
        inner_width,
    )?;
    write_header_box_line(
        stdout,
        start_row.saturating_add(5),
        &format!("╰{horizontal}╯"),
    )
}

fn write_header_box_line<W: Write>(stdout: &mut W, row: u16, text: &str) -> Result<(), String> {
    queue!(
        stdout,
        ResetColor,
        cursor::MoveTo(0, row),
        Clear(ClearType::CurrentLine)
    )
    .map_err(|e| e.to_string())?;
    write!(stdout, "{text}").map_err(|e| e.to_string())
}

fn write_header_content_line<W: Write>(
    stdout: &mut W,
    row: u16,
    text: &str,
    inner_width: usize,
) -> Result<(), String> {
    let content = pad_display_width(&format!(" {text}"), inner_width);
    write_header_box_line(stdout, row, &format!("│{content}│"))
}

fn codex_header_model_label(session: &CliSession) -> String {
    let model = cli_session_model_label(session);
    let reasoning = cli_session_reasoning_label(session);
    if reasoning == "default" {
        return model;
    }
    // Surface the picker-friendly label (Low/Medium/High/Extra High) so the
    // header matches what the user selected, even when the underlying value
    // is `off` for thinking-only models.
    let display = cli_reasoning_display_label(&reasoning);
    if display.is_empty() {
        model
    } else {
        format!("{model} {display}")
    }
}

fn cli_reasoning_display_label(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "low" => "Low".to_string(),
        "medium" => "Medium".to_string(),
        "high" => "High".to_string(),
        "xhigh" | "extra high" | "extra_high" => "Extra High".to_string(),
        "minimal" => "Minimal".to_string(),
        "on" => "On".to_string(),
        "off" | "none" => String::new(),
        _ => String::new(),
    }
}

fn codex_composer_height() -> u16 {
    CODEX_COMPOSER_MAX_ROWS
}

fn codex_composer_height_for_input(
    cols: u16,
    prompt: &str,
    line: &str,
    cursor_index: usize,
) -> u16 {
    let width = (cols as usize).max(1);
    let display = composer_display(prompt, line, cursor_index, width);
    (display.lines.len() as u16)
        .saturating_add(2)
        .max(CODEX_COMPOSER_MIN_ROWS)
        .min(codex_composer_height())
}

/// Pure model of "where does the next composer start, after an agent reply".
///
/// `submitted_start_row` is the row the previous composer started on (0-indexed).
/// `agent_response_lines` is how many lines the agent reply printed.
/// `old_composer_height` is how many rows the previous composer occupied.
/// `println_trailing_newline` is whether `print_cli_visible_response` ends
/// with a `println!()` (true) or not (false).
///
/// The cursor is moved by `finalize_submitted_input` to
/// `submitted_start_row + old_composer_height` (1 line past old composer).
/// Then agent reply prints `agent_response_lines` lines from that cursor.
/// If `println_trailing_newline`, cursor moves down 1 more line.
/// `next_chat_input_start_row` adds 1 more line.
///
/// Returns the row the new composer should start on.
fn predict_next_composer_row(
    submitted_start_row: u16,
    old_composer_height: u16,
    agent_response_lines: u16,
    println_trailing_newline: bool,
) -> u16 {
    let mut row = submitted_start_row.saturating_add(old_composer_height);
    row = row.saturating_add(agent_response_lines);
    if println_trailing_newline {
        row = row.saturating_add(1);
    }
    row = row.saturating_add(1); // next_chat_input_start_row = current + 1
    row
}

fn codex_inline_reserved_height_for_popup(header_visible: bool, popup_rows: usize) -> u16 {
    let header_rows = if header_visible {
        codex_chat_header_height()
    } else {
        0
    };
    let popup_rows = (popup_rows as u16).min(CODEX_POPUP_MAX_ROWS);
    header_rows
        .saturating_add(CODEX_COMPOSER_MIN_ROWS)
        .saturating_add(CODEX_COMPOSER_FOOTER_GAP)
        .saturating_add(CODEX_FOOTER_HEIGHT)
        .saturating_add(popup_rows)
}

fn codex_inline_start_row(header_visible: bool) -> u16 {
    codex_inline_start_row_for_popup(header_visible, 0)
}

fn codex_inline_start_row_for_popup(header_visible: bool, popup_rows: usize) -> u16 {
    let (_, terminal_rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let reserved_rows = codex_inline_reserved_height_for_popup(header_visible, popup_rows)
        .min(terminal_rows.max(1));
    terminal_rows.saturating_sub(reserved_rows)
}

fn render_composer_band(
    stdout: &mut io::Stdout,
    cols: u16,
    start_row: u16,
    height: u16,
    prompt: &str,
    line: &str,
    cursor_index: usize,
) -> Result<(u16, u16), String> {
    let width = (cols as usize).max(1);
    let height = height.max(CODEX_COMPOSER_MIN_ROWS);
    let display = composer_display(prompt, line, cursor_index, width);
    let content_rows = height.saturating_sub(2).max(1) as usize;
    let scroll_start = list_scroll_start(display.cursor_line, display.lines.len(), content_rows);
    let input_start = start_row.saturating_add(1);
    let fill = " ".repeat(width);
    for row in start_row..start_row.saturating_add(height) {
        queue!(
            stdout,
            ResetColor,
            cursor::MoveTo(0, row),
            Clear(ClearType::CurrentLine)
        )
        .map_err(|e| e.to_string())?;
        write!(stdout, "{CODEX_COMPOSER_BG}{fill}\x1b[0m").map_err(|e| e.to_string())?;
    }

    for (index, text) in display
        .lines
        .iter()
        .skip(scroll_start)
        .take(content_rows)
        .enumerate()
    {
        queue!(
            stdout,
            cursor::MoveTo(0, input_start.saturating_add(index as u16))
        )
        .map_err(|e| e.to_string())?;
        write!(stdout, "{CODEX_COMPOSER_BG}").map_err(|e| e.to_string())?;
        write_styled_prompt_line(stdout, text, width)?;
        write!(stdout, "\x1b[0m").map_err(|e| e.to_string())?;
    }

    let cursor_visible_offset = display.cursor_line.saturating_sub(scroll_start) as u16;
    let cursor_row = input_start
        .saturating_add(cursor_visible_offset)
        .min(input_start.saturating_add(content_rows.saturating_sub(1) as u16));
    let cursor_col = display.cursor_col.min(width.saturating_sub(1)) as u16;

    Ok((cursor_col, cursor_row))
}

struct ComposerDisplay {
    lines: Vec<String>,
    cursor_line: usize,
    cursor_col: usize,
}

fn composer_display(
    prompt: &str,
    line: &str,
    cursor_index: usize,
    width: usize,
) -> ComposerDisplay {
    let width = width.max(1);
    let prompt_text = truncate_display_width(prompt, width);
    let prompt_width = terminal_display_width(&prompt_text).min(width);
    let indent = " ".repeat(prompt_width);
    let indent_width = prompt_width;
    let chars = line.chars().collect::<Vec<_>>();
    let cursor_index = cursor_index.min(chars.len());
    let mut lines = Vec::new();
    let mut current = prompt_text;
    let mut current_width = prompt_width;
    let mut cursor_line = 0usize;
    let mut cursor_col = prompt_width;
    let mut cursor_recorded = false;

    for (index, ch) in chars.iter().copied().enumerate() {
        if index == cursor_index {
            cursor_line = lines.len();
            cursor_col = current_width;
            cursor_recorded = true;
        }
        if ch == '\n' {
            lines.push(current);
            current = indent.clone();
            current_width = indent_width;
            continue;
        }
        let char_width = terminal_char_width(ch).max(1);
        if current_width > indent_width && current_width.saturating_add(char_width) > width {
            lines.push(current);
            current = indent.clone();
            current_width = indent_width;
        }
        current.push(ch);
        current_width = current_width.saturating_add(char_width);
    }

    if !cursor_recorded {
        cursor_line = lines.len();
        cursor_col = current_width;
    }
    lines.push(current);

    ComposerDisplay {
        lines,
        cursor_line,
        cursor_col,
    }
}

fn write_styled_prompt_line<W: Write>(
    stdout: &mut W,
    text: &str,
    width: usize,
) -> Result<(), String> {
    let text = truncate_display_width(text, width);
    if let Some(rest) = text.strip_prefix(CHAT_PROMPT) {
        let prompt = truncate_display_width(CHAT_PROMPT, width);
        write!(stdout, "\x1b[1m{prompt}\x1b[22m{rest}").map_err(|e| e.to_string())
    } else {
        write!(stdout, "{text}").map_err(|e| e.to_string())
    }
}

fn render_codex_footer(
    stdout: &mut io::Stdout,
    cols: u16,
    row: u16,
    session: &CliSession,
) -> Result<(), String> {
    let (left, right) = codex_footer_parts(session);
    let width = cols.max(1) as usize;
    let left = truncate_display_width(&left, width);
    let left_width = terminal_display_width(&left);
    let right_max_width = width.saturating_sub(left_width.saturating_add(1));
    let right = truncate_display_width(&right, right_max_width);
    let right_width = terminal_display_width(&right);
    let gap = if right_width == 0 {
        0
    } else {
        width.saturating_sub(left_width.saturating_add(right_width))
    };
    let text = if right_width == 0 {
        left
    } else {
        format!("{left}{}{right}", " ".repeat(gap))
    };
    queue!(
        stdout,
        ResetColor,
        cursor::MoveTo(0, row),
        Clear(ClearType::CurrentLine)
    )
    .map_err(|e| e.to_string())?;
    write!(stdout, "\x1b[2m{text}\x1b[0m").map_err(|e| e.to_string())
}

fn codex_footer_parts(session: &CliSession) -> (String, String) {
    let model = cli_session_model_label(session);
    let reasoning = cli_session_reasoning_label(session);
    let model_status = if reasoning == "default" || reasoning == "off" {
        model
    } else {
        format!("{model} {reasoning}")
    };
    let workspace = codex_footer_workspace_label(session);
    (
        format!("{model_status} · 100% context left · {workspace}"),
        String::new(),
    )
}

fn codex_footer_workspace_label(session: &CliSession) -> String {
    let label = session
        .workspace_label
        .clone()
        .unwrap_or_else(cli_current_directory_label);
    let Some(open_index) = label.find('(') else {
        return label;
    };
    let Some(close_offset) = label[open_index + 1..].find(')') else {
        return label;
    };
    label[open_index + 1..open_index + 1 + close_offset].to_string()
}

fn cli_session_model_label(session: &CliSession) -> String {
    session.model_id.as_deref().unwrap_or("auto").to_string()
}

fn cli_session_reasoning_label(session: &CliSession) -> String {
    cli_effective_reasoning_effort(session).unwrap_or_else(|| "default".to_string())
}

fn cli_session_reasoning_field_label(session: &CliSession) -> &'static str {
    if session
        .model_id
        .as_deref()
        .map(|model| model.eq_ignore_ascii_case("MiniMax-M3"))
        .unwrap_or(false)
    {
        "thinking"
    } else {
        "reasoning"
    }
}

fn cli_current_directory_label() -> String {
    std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

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

fn terminal_display_width(text: &str) -> usize {
    text.chars().map(terminal_char_width).sum()
}

fn pad_display_width(text: &str, width: usize) -> String {
    let clipped = truncate_display_width(text, width);
    let padding = " ".repeat(width.saturating_sub(terminal_display_width(&clipped)));
    format!("{clipped}{padding}")
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

async fn run_task(
    state: &AppState,
    cli: &Cli,
    session: &CliSession,
    task: &str,
    messages: Option<Vec<AgentConversationMessage>>,
    show_working_status: bool,
    emit_terminal_output: bool,
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
        messages,
        provider_id: session.provider_id.clone(),
        credential_id: session.credential_id.clone(),
        model_id: session.model_id.clone(),
        reasoning_effort: cli_effective_reasoning_effort(session),
        allow_shell: session.shell_allowed,
        assume_yes: cli.yes,
        cli_mode: true,
        goal: session.goal.clone(),
        goal_slot: None,
    };

    let mut working_status = if show_working_status && !cli.json {
        Some(CliWorkingStatus::start()?)
    } else {
        None
    };
    let mut terminal_events = Vec::new();
    let run_result = agent_runner::run_agent(&project, &settings, input, |event| {
        terminal_events.push(event.clone());
        if emit_terminal_output && !cli.json {
            print_trace_event(event, cli.verbose);
        }
    })
    .await;
    if let Some(status) = working_status.take() {
        status.finish()?;
    }
    let run = run_result?;
    save_trace(&project.repo_root, &run)?;

    if emit_terminal_output && cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&cli_json_run_payload(session, &run, &terminal_events)?)
                .map_err(|e| e.to_string())?
        );
    } else if emit_terminal_output {
        if let Some(parts) = final_response(&run) {
            print_cli_visible_response(session, &parts, !show_working_status);
        } else if let Some(last) = terminal_events
            .last()
            .and_then(|event| event.output_summary.clone())
        {
            let parts = FinalResponseParts {
                reasoning: None,
                content: last,
            };
            print_cli_visible_response(session, &parts, !show_working_status);
        }
    }
    Ok(run)
}

fn cli_json_run_payload(
    session: &CliSession,
    run: &MockAgentRun,
    terminal_events: &[ToolTraceEvent],
) -> Result<serde_json::Value, String> {
    let mut payload = serde_json::to_value(run).map_err(|error| error.to_string())?;
    let parts_response = final_response(run);
    let fallback_summary = terminal_events
        .last()
        .and_then(|event| event.output_summary.clone());
    let hide_thinking = cli_should_hide_thinking(session);
    let visible_response = parts_response
        .as_ref()
        .map(|parts| {
            if hide_thinking {
                strip_think_blocks(&parts.content)
            } else {
                parts.content.clone()
            }
        })
        .or_else(|| {
            fallback_summary.as_ref().map(|text| {
                if hide_thinking {
                    strip_think_blocks(text)
                } else {
                    text.clone()
                }
            })
        });
    let visible_reasoning = parts_response
        .as_ref()
        .and_then(|parts| parts.reasoning.clone())
        .filter(|text| !text.trim().is_empty())
        .filter(|_| !hide_thinking);
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "visibleResponse".to_string(),
            visible_response
                .map(|text| serde_json::Value::String(text.trim().to_string()))
                .unwrap_or(serde_json::Value::Null),
        );
        object.insert(
            "visibleReasoning".to_string(),
            visible_reasoning
                .map(serde_json::Value::String)
                .unwrap_or(serde_json::Value::Null),
        );
    }
    Ok(payload)
}

fn print_cli_visible_response(
    session: &CliSession,
    parts: &FinalResponseParts,
    leading_blank: bool,
) {
    let hide_thinking = cli_should_hide_thinking(session);
    let visible_content = if hide_thinking {
        strip_think_blocks(&parts.content)
    } else {
        parts.content.clone()
    };
    let trimmed_content = visible_content.trim();
    let reasoning_text = if hide_thinking {
        None
    } else {
        parts
            .reasoning
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
    };

    if trimmed_content.is_empty() && reasoning_text.is_none() {
        if hide_thinking && find_ascii_case_insensitive(&parts.content, "<think>").is_some() {
            if leading_blank {
                println!();
            }
            println!("• \x1b[2mthinking hidden; no visible response\x1b[0m");
            println!();
        }
        return;
    }

    if leading_blank {
        println!();
    }
    if let Some(reasoning) = reasoning_text {
        println!("• {}", reasoning);
        println!("----");
    }
    if !trimmed_content.is_empty() {
        println!("• {}", trimmed_content);
    }
    // No trailing println!() — keep the cursor on the agent reply's last
    // line so the next composer renders with 0 blank lines of gap.
}

fn cli_effective_reasoning_effort(session: &CliSession) -> Option<String> {
    match session.reasoning_effort.as_deref() {
        Some(value) => Some(value.to_string()),
        None => session
            .model_id
            .as_deref()
            .filter(|model| model.eq_ignore_ascii_case("MiniMax-M3"))
            .map(|_| "off".to_string()),
    }
}

fn hydrate_cli_session_defaults(state: &AppState, session: &mut CliSession) -> Result<(), String> {
    let explicit_selection = session.provider_id.is_some()
        || session.credential_id.is_some()
        || session.model_id.is_some();
    let settings_guard = state
        .settings
        .lock()
        .map_err(|_| "settings lock failed".to_string())?;
    let settings = current_settings(&settings_guard);
    let choices = cli_model_choices(&settings);
    let Some(choice) = choices.into_iter().find(|choice| {
        session
            .provider_id
            .as_deref()
            .map(|provider| provider == choice.provider_id)
            .unwrap_or(true)
            && session
                .credential_id
                .as_deref()
                .map(|credential| choice.credential_id.as_deref() == Some(credential))
                .unwrap_or(true)
            && session
                .model_id
                .as_deref()
                .map(|model| model == choice.model_id)
                .unwrap_or(true)
    }) else {
        if explicit_selection {
            return Err(format!(
                "No enabled model matches provider={}, credential={}, model={}.",
                session.provider_id.as_deref().unwrap_or("auto"),
                session.credential_id.as_deref().unwrap_or("auto"),
                session.model_id.as_deref().unwrap_or("auto")
            ));
        }
        return Ok(());
    };
    apply_cli_choice_to_session(session, &choice);
    session.reasoning_effort =
        normalize_cli_reasoning_for_choice(session.reasoning_effort.as_deref(), &choice);
    if session.reasoning_effort.is_none() {
        if cli_choice_reasoning_mode(&choice) == "toggle" {
            // For thinking-only models the model config decides whether
            // thinking starts on or off; pass it through the same alias
            // table so the wire value stays consistent.
            session.reasoning_effort = normalize_cli_reasoning_for_choice(
                Some(choice.default_reasoning.as_str()),
                &choice,
            );
        } else if cli_choice_reasoning_mode(&choice) == "effort" {
            // Effort-mode models keep `default` as the sentinel meaning
            // "use whatever the provider recommends" (the picker surfaces
            // it as the first option). Leave the session unset so the
            // backend can pick the provider's own default.
            session.reasoning_effort = Some("default".to_string());
        }
    }
    Ok(())
}

fn apply_cli_choice_to_session(session: &mut CliSession, choice: &CliModelChoice) {
    session.provider_id = Some(choice.provider_id.clone());
    session.provider_name = Some(choice.provider_name.clone());
    session.credential_id = choice.credential_id.clone();
    session.credential_name = choice.credential_name.clone();
    session.model_id = Some(choice.model_id.clone());
}

fn normalize_cli_reasoning_for_choice(
    value: Option<&str>,
    choice: &CliModelChoice,
) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("default") {
        return None;
    }
    let value = value.to_ascii_lowercase();
    match cli_choice_reasoning_mode(choice) {
        // The CLI picker for thinking-only models still exposes Off/On, but
        // --reasoning low|medium|high|xhigh may also arrive via flags. Keep
        // the Low/High/Extra High → on and `low` → off aliasing so the wire
        // value stays consistent with the desktop picker's semantics.
        "toggle" => match value.as_str() {
            "off" | "none" | "low" => Some("off".to_string()),
            "on" | "minimal" | "medium" | "high" | "xhigh" => Some("on".to_string()),
            _ => None,
        },
        "effort" => match value.as_str() {
            "minimal" | "low" | "medium" | "high" | "xhigh" => Some(value),
            "on" => Some("medium".to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn cli_should_hide_thinking(session: &CliSession) -> bool {
    match session.reasoning_effort.as_deref() {
        Some(value) => value.eq_ignore_ascii_case("off") || value.eq_ignore_ascii_case("none"),
        None => session
            .model_id
            .as_deref()
            .map(|model| model.eq_ignore_ascii_case("MiniMax-M3"))
            .unwrap_or(false),
    }
}

fn strip_think_blocks(text: &str) -> String {
    let mut output = String::new();
    let mut remaining = text;
    while let Some(start) = find_ascii_case_insensitive(remaining, "<think>") {
        output.push_str(&remaining[..start]);
        let content_start = start + "<think>".len();
        let after_start = &remaining[content_start..];
        let Some(end) = find_ascii_case_insensitive(after_start, "</think>") else {
            remaining = "";
            break;
        };
        remaining = &after_start[end + "</think>".len()..];
    }
    output.push_str(remaining);
    output
        .trim_start_matches(|ch| ch == '\r' || ch == '\n')
        .to_string()
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}

enum ChatCommandResult {
    NotCommand,
    Handled,
    NewSession,
    Exit,
}

fn is_picker_chat_command(task: &str) -> bool {
    matches!(
        task.trim().to_ascii_lowercase().as_str(),
        "/model" | "/models" | "/reason" | "/reasoning" | "/reasion"
    )
}

async fn handle_chat_command(
    state: &AppState,
    stdin: &io::Stdin,
    session: &mut CliSession,
    requested_project: Option<&str>,
    task: &str,
) -> Result<ChatCommandResult, String> {
    match task.trim().to_ascii_lowercase().as_str() {
        "/" => {
            print_slash_commands();
            Ok(ChatCommandResult::Handled)
        }
        "/exit" | "/quit" => Ok(ChatCommandResult::Exit),
        "/new" => Ok(ChatCommandResult::NewSession),
        "/model" | "/models" => {
            choose_cli_model(state, stdin, session)?;
            Ok(ChatCommandResult::Handled)
        }
        "/reason" | "/reasoning" | "/reasion" => {
            choose_cli_reasoning_for_current_model(state, stdin, session)?;
            Ok(ChatCommandResult::Handled)
        }
        "/status" => {
            let project = select_project(state, requested_project)?;
            let limits = fetch_coding_plan_limits(state, session).await;
            print_cli_session(session, Some(&project), Some(&limits), false);
            Ok(ChatCommandResult::Handled)
        }
        "/clear" => {
            clear_terminal()?;
            Ok(ChatCommandResult::Handled)
        }
        "/fast"
        | "/ide"
        | "/permissions"
        | "/keymap"
        | "/vim"
        | "/sandbox-add-read-dir"
        | "/experimental" => {
            println!("{} is not implemented yet.", task.trim());
            Ok(ChatCommandResult::Handled)
        }
        "/help" => {
            print_slash_commands();
            Ok(ChatCommandResult::Handled)
        }
        value if value.starts_with('/') => {
            run_slash_command_prefix(state, stdin, session, requested_project, value).await
        }
        _ => Ok(ChatCommandResult::NotCommand),
    }
}

async fn run_slash_command_prefix(
    state: &AppState,
    stdin: &io::Stdin,
    session: &mut CliSession,
    requested_project: Option<&str>,
    prefix: &str,
) -> Result<ChatCommandResult, String> {
    let matches = slash_commands()
        .iter()
        .filter(|(name, _)| name.starts_with(prefix))
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        print_slash_command_matches(prefix);
        return Ok(ChatCommandResult::Handled);
    }

    match matches[0].0 {
        "/model" => {
            choose_cli_model(state, stdin, session)?;
            Ok(ChatCommandResult::Handled)
        }
        "/new" => Ok(ChatCommandResult::NewSession),
        "/reason" => {
            choose_cli_reasoning_for_current_model(state, stdin, session)?;
            Ok(ChatCommandResult::Handled)
        }
        "/status" => {
            let project = select_project(state, requested_project)?;
            let limits = fetch_coding_plan_limits(state, session).await;
            print_cli_session(session, Some(&project), Some(&limits), false);
            Ok(ChatCommandResult::Handled)
        }
        "/help" => {
            print_slash_commands();
            Ok(ChatCommandResult::Handled)
        }
        "/clear" => {
            clear_terminal()?;
            Ok(ChatCommandResult::Handled)
        }
        "/exit" | "/quit" => Ok(ChatCommandResult::Exit),
        command => {
            println!("{command} is not implemented yet.");
            Ok(ChatCommandResult::Handled)
        }
    }
}

fn print_slash_commands() {
    println!();
    for (name, description) in slash_commands() {
        println!("{}", slash_command_list_line(name, description));
    }
}

fn print_slash_command_matches(prefix: &str) {
    let mut stdout = io::stdout();
    let _ = write_slash_command_matches(&mut stdout, prefix);
    let _ = stdout.flush();
}

fn clear_terminal() -> Result<(), String> {
    let mut stdout = io::stdout();
    execute!(
        stdout,
        ResetColor,
        cursor::MoveTo(0, 0),
        Clear(ClearType::All)
    )
    .map_err(|e| e.to_string())
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
    if matches.is_empty() {
        let content_width = crossterm::terminal::size()
            .map(|(cols, _)| cols.max(1) as usize)
            .unwrap_or(80);
        lines.push(truncate_display_width(
            &format!("  No command matches `{prefix}`."),
            content_width,
        ));
        lines.push(truncate_display_width(
            "  Type / to show all commands.",
            content_width,
        ));
        return lines;
    }
    for (name, description) in matches {
        lines.push(slash_command_list_line(name, description));
    }
    lines
}

fn slash_command_list_line(name: &str, description: &str) -> String {
    let content_width = crossterm::terminal::size()
        .map(|(cols, _)| cols.max(1) as usize)
        .unwrap_or(80);
    let prefix = format!("  {name:<7} ");
    truncate_display_width(&format!("{prefix}{description}"), content_width)
}

fn slash_command_popup_lines(prefix: &str, selected_index: usize) -> Vec<String> {
    let matches = slash_command_matches(prefix);
    let content_width = crossterm::terminal::size()
        .map(|(cols, _)| cols.saturating_sub(2).max(1) as usize)
        .unwrap_or(78);
    let mut lines = Vec::new();
    if matches.is_empty() {
        lines.push(truncate_display_width(
            &format!("  No command matches `{prefix}`."),
            content_width,
        ));
        lines.push(truncate_display_width(
            "  Type / to show all commands.",
            content_width,
        ));
        return lines;
    }
    for (index, (name, description)) in matches.into_iter().enumerate() {
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

fn slash_popup_scroll_start(
    selected_index: usize,
    total_lines: usize,
    visible_rows: usize,
) -> usize {
    list_scroll_start(selected_index, total_lines, visible_rows)
}

fn popup_scrollbar_thumb(
    total_count: usize,
    visible_count: usize,
    scroll_start: usize,
) -> Option<usize> {
    if visible_count == 0 || total_count <= visible_count {
        return None;
    }
    let max_scroll = total_count.saturating_sub(visible_count).max(1);
    Some(scroll_start.saturating_mul(visible_count.saturating_sub(1)) / max_scroll)
}

fn chat_popup_page_size(start_row: u16, header_visible: bool) -> usize {
    let (_, terminal_rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let header_rows = if header_visible {
        codex_chat_header_height()
    } else {
        0
    };
    let bottom_reserved = CODEX_COMPOSER_MIN_ROWS
        .saturating_add(CODEX_COMPOSER_FOOTER_GAP)
        .saturating_add(CODEX_FOOTER_HEIGHT);
    terminal_rows
        .saturating_sub(start_row)
        .saturating_sub(header_rows.saturating_add(bottom_reserved))
        .min(CODEX_POPUP_MAX_ROWS)
        .max(1) as usize
}

fn cli_picker_page_size(start_row: u16) -> usize {
    let (_, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    rows.saturating_sub(start_row.min(rows.saturating_sub(1)))
        .max(1)
        .saturating_sub(4)
        .max(1) as usize
}

fn list_scroll_start(selected_index: usize, total_count: usize, visible_count: usize) -> usize {
    if visible_count == 0 || total_count <= visible_count || selected_index < visible_count {
        return 0;
    }
    selected_index
        .saturating_add(1)
        .saturating_sub(visible_count)
        .min(total_count.saturating_sub(visible_count))
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
        ("/new", "start a new chat and clear conversation context"),
        ("/model", "choose what model and reasoning/thinking to use"),
        ("/reason", "choose reasoning/thinking"),
        ("/status", "show model, provider, directory, workspace, permissions, account, session, agents.md, and usage/limits"),
        ("/clear", "clear the terminal"),
        ("/help", "show commands"),
        ("/exit", "quit"),
        ("/quit", "quit"),
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
        println!("No enabled models. Configure providers and models in CodeForge settings first.");
        return Ok(());
    }

    if stdin.is_terminal() {
        let items = choices
            .iter()
            .map(|choice| CliPickerItem {
                label: cli_model_display_name(choice),
                description: cli_model_description(choice),
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
            "Access legacy models by running codeforge -m <model_name> or in your config.toml",
            &items,
            current_index,
        )?
        else {
            return Ok(());
        };
        let choice = &choices[index];
        apply_cli_choice_to_session(session, choice);
        return choose_cli_reasoning_for_choice(stdin, session, choice);
    }

    println!();
    println!("  Select Model and Effort");
    println!("  Models are loaded from the shared CodeForge settings.");
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
    apply_cli_choice_to_session(session, choice);
    choose_cli_reasoning_for_choice(stdin, session, choice)
}

fn choose_cli_reasoning_for_choice(
    stdin: &io::Stdin,
    session: &mut CliSession,
    choice: &CliModelChoice,
) -> Result<(), String> {
    match cli_choice_reasoning_mode(choice) {
        "toggle" => choose_cli_thinking_toggle(stdin, session, choice.default_reasoning.as_str()),
        "effort" => choose_cli_reasoning(stdin, session),
        _ => {
            session.reasoning_effort = None;
            Ok(())
        }
    }
}

fn cli_choice_reasoning_mode(choice: &CliModelChoice) -> &str {
    if choice.model_id.eq_ignore_ascii_case("MiniMax-M3")
        || choice.model_name.eq_ignore_ascii_case("MiniMax-M3")
    {
        "toggle"
    } else {
        choice.reasoning_mode.trim()
    }
}

fn choose_cli_thinking_toggle(
    stdin: &io::Stdin,
    session: &mut CliSession,
    default_reasoning: &str,
) -> Result<(), String> {
    let choices = [("off", "Off"), ("on", "On")];
    // The default lives in the model config (`defaultReasoning` in
    // settings.json) so admins can flip it per provider/model without
    // touching the CLI. Fall back to `off` only when the config omits it.
    let fallback = if default_reasoning.eq_ignore_ascii_case("on") {
        "on"
    } else {
        "off"
    };
    let current = session
        .reasoning_effort
        .as_deref()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| fallback.to_string());
    if stdin.is_terminal() {
        let items = choices
            .iter()
            .map(|(value, label)| CliPickerItem {
                label: (*label).to_string(),
                description: if *value == "on" {
                    "Enable MiniMax-M3 thinking output.".to_string()
                } else {
                    "Do not request thinking output.".to_string()
                },
            })
            .collect::<Vec<_>>();
        let current_index = choices
            .iter()
            .position(|(value, _)| *value == current)
            .unwrap_or(0);
        let Some(index) = run_cli_picker(session, "Select Thinking", "", &items, current_index)?
        else {
            return Ok(());
        };
        session.reasoning_effort = Some(choices[index].0.to_string());
        return Ok(());
    }

    println!();
    println!("  Select Thinking");
    println!();
    for (index, (value, label)) in choices.iter().enumerate() {
        let marker = if *value == current { "›" } else { " " };
        let current_label = if *value == current { " (current)" } else { "" };
        println!("{marker} {}. {}{}", index + 1, label, current_label);
    }
    println!();
    println!("  Press enter to keep current or type a number.");
    let Some(line) = read_prompt(stdin, "› ")? else {
        return Ok(());
    };
    if line.trim().is_empty() {
        return Ok(());
    }
    let selected = match line.trim() {
        "1" => "off",
        "2" => "on",
        value if value.eq_ignore_ascii_case("off") => "off",
        value if value.eq_ignore_ascii_case("on") => "on",
        value => return Err(format!("Unknown thinking option: {value}")),
    };
    session.reasoning_effort = Some(selected.to_string());
    Ok(())
}

fn choose_cli_reasoning_for_current_model(
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
    let current_choice = cli_model_choices(&settings)
        .into_iter()
        .find(|choice| {
            session.provider_id.as_deref() == Some(choice.provider_id.as_str())
                && session.credential_id.as_deref() == choice.credential_id.as_deref()
                && session.model_id.as_deref() == Some(choice.model_id.as_str())
        })
        .or_else(|| cli_model_choices(&settings).into_iter().next());
    if let Some(choice) = current_choice {
        choose_cli_reasoning_for_choice(stdin, session, &choice)
    } else {
        choose_cli_reasoning(stdin, session)
    }
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
        let Some(index) = run_cli_picker(
            session,
            "Select Reasoning Effort",
            "",
            &items,
            current_index,
        )?
        else {
            return Ok(());
        };
        session.reasoning_effort = normalize_cli_reasoning(Some(choices[index].0));
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

    let start_row = reserve_terminal_rows(
        current_append_row()?,
        cli_picker_reserved_height(subtitle, items.len()),
    )?;
    enable_raw_mode().map_err(|e| e.to_string())?;
    let raw_mode = RawModeGuard;
    let inline_screen = InlineScreenGuard { start_row };
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
                KeyCode::Up | KeyCode::Char('p') | KeyCode::Char('P')
                    if matches!(code, KeyCode::Up) || modifiers.contains(KeyModifiers::CONTROL) =>
                {
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
                KeyCode::Down | KeyCode::Char('n') | KeyCode::Char('N')
                    if matches!(code, KeyCode::Down)
                        || modifiers.contains(KeyModifiers::CONTROL) =>
                {
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
                KeyCode::PageUp => {
                    let page_size = cli_picker_page_size(inline_screen.start_row());
                    selected_index = selected_index.saturating_sub(page_size);
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
                KeyCode::PageDown => {
                    let page_size = cli_picker_page_size(inline_screen.start_row());
                    selected_index = selected_index
                        .saturating_add(page_size)
                        .min(items.len().saturating_sub(1));
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
                KeyCode::Home => {
                    selected_index = 0;
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
                KeyCode::End => {
                    selected_index = items.len().saturating_sub(1);
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
                KeyCode::Char(ch)
                    if ch.is_ascii_digit()
                        && !modifiers.contains(KeyModifiers::CONTROL)
                        && !modifiers.contains(KeyModifiers::ALT) =>
                {
                    let Some(digit) = ch.to_digit(10) else {
                        continue;
                    };
                    let Some(index) = (digit as usize).checked_sub(1) else {
                        continue;
                    };
                    if index < items.len() {
                        inline_screen.clear()?;
                        drop(inline_screen);
                        drop(raw_mode);
                        return Ok(Some(index));
                    }
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

fn cli_picker_reserved_height(subtitle: &str, item_count: usize) -> u16 {
    let item_start_offset: u16 = if subtitle.trim().is_empty() { 1 } else { 3 };
    item_start_offset
        .saturating_add((item_count as u16).min(CODEX_POPUP_MAX_ROWS))
        .saturating_add(3)
}

fn render_cli_picker(
    _session: &CliSession,
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
    let panel_start = start_row.saturating_add(1).min(rows.saturating_sub(1));
    let available_panel_rows = rows.saturating_sub(panel_start).max(1);
    let has_subtitle = !subtitle.trim().is_empty();
    let item_start_offset: u16 = if has_subtitle { 3 } else { 1 };
    let max_items = available_panel_rows
        .saturating_sub(item_start_offset.saturating_add(2))
        .max(1) as usize;
    let visible_count = items.len().min(max_items);
    let scroll_start = list_scroll_start(selected_index, items.len(), visible_count);
    let scrollbar_col = cols.saturating_sub(2);
    let scrollbar_thumb = popup_scrollbar_thumb(items.len(), visible_count, scroll_start);
    let panel_rows = item_start_offset.saturating_add(visible_count as u16);
    let footer_row = panel_start
        .saturating_add(panel_rows)
        .saturating_add(1)
        .min(rows.saturating_sub(1));
    let clear_to = footer_row.saturating_add(2).min(rows);
    let fill = " ".repeat(width);
    for row in start_row.min(rows.saturating_sub(1))..clear_to {
        queue!(
            stdout,
            ResetColor,
            cursor::MoveTo(0, row),
            Clear(ClearType::CurrentLine)
        )
        .map_err(|e| e.to_string())?;
        if row < footer_row {
            write!(stdout, "{CODEX_COMPOSER_BG}{fill}\x1b[0m").map_err(|e| e.to_string())?;
        }
    }

    queue!(stdout, cursor::MoveTo(2, panel_start)).map_err(|e| e.to_string())?;
    write!(
        stdout,
        "{CODEX_COMPOSER_BG}\x1b[1m{}\x1b[0m",
        truncate_display_width(title, width.saturating_sub(4))
    )
    .map_err(|e| e.to_string())?;
    if has_subtitle && available_panel_rows > 2 {
        queue!(stdout, cursor::MoveTo(2, panel_start + 1)).map_err(|e| e.to_string())?;
        write!(
            stdout,
            "{CODEX_COMPOSER_BG}\x1b[2m{}\x1b[0m",
            truncate_display_width(subtitle, width.saturating_sub(4))
        )
        .map_err(|e| e.to_string())?;
    }

    for (index, item) in items
        .iter()
        .enumerate()
        .skip(scroll_start)
        .take(visible_count)
    {
        let row_offset = index.saturating_sub(scroll_start);
        let row = panel_start + item_start_offset + row_offset as u16;
        if row >= rows {
            break;
        }
        let marker = if index == selected_index { "›" } else { " " };
        let current = if index == current_index {
            " (current)"
        } else {
            ""
        };
        let label_width = width
            .saturating_mul(2)
            .saturating_div(5)
            .clamp(18, 36)
            .min(width.saturating_sub(4).max(1));
        let label = pad_display_width(
            &format!("{marker} {}. {}{}", index + 1, item.label, current),
            label_width,
        );
        let description = truncate_display_width(
            &item.description,
            width.saturating_sub(label_width.saturating_add(4)),
        );
        queue!(stdout, cursor::MoveTo(0, row)).map_err(|e| e.to_string())?;
        if index == selected_index {
            write!(
                stdout,
                "{CODEX_COMPOSER_BG}\x1b[36;1m{}  {}\x1b[0m",
                label, description
            )
            .map_err(|e| e.to_string())?;
        } else {
            write!(
                stdout,
                "{CODEX_COMPOSER_BG}{}  \x1b[2m{}\x1b[0m",
                label, description
            )
            .map_err(|e| e.to_string())?;
        }
        if items.len() > visible_count {
            let marker = if Some(row_offset) == scrollbar_thumb {
                "█"
            } else {
                "│"
            };
            queue!(stdout, cursor::MoveTo(scrollbar_col, row)).map_err(|e| e.to_string())?;
            write!(stdout, "{CODEX_COMPOSER_BG}\x1b[2m{marker}\x1b[0m")
                .map_err(|e| e.to_string())?;
        }
    }

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

fn cli_choice_is_current(session: &CliSession, choice: &CliModelChoice, index: usize) -> bool {
    if session.provider_id.is_none() && session.model_id.is_none() {
        return index == 0;
    }
    session.provider_id.as_deref() == Some(choice.provider_id.as_str())
        && session.credential_id.as_deref() == choice.credential_id.as_deref()
        && session.model_id.as_deref() == Some(choice.model_id.as_str())
}

fn cli_model_display_name(choice: &CliModelChoice) -> String {
    choice.model_name.clone()
}

fn cli_model_description(choice: &CliModelChoice) -> String {
    let source = match choice.credential_name.as_deref() {
        Some(credential) if !credential.trim().is_empty() => {
            format!("{} / {}", choice.provider_name, credential)
        }
        _ => choice.provider_name.clone(),
    };
    match choice.model_id.as_str() {
        "gpt-5.5" => {
            "Frontier model for complex coding, research, and real-world work.".to_string()
        }
        "gpt-5.4" => "Strong model for everyday coding.".to_string(),
        "gpt-5.4-mini" => {
            "Small, fast, and cost-efficient model for simpler coding tasks.".to_string()
        }
        "gpt-5.3-codex-spark" => "Ultra-fast coding model.".to_string(),
        "default" if choice.provider_id == "codex-cli" => {
            "Use the model configured by Codex CLI.".to_string()
        }
        _ if choice.provider_id == "codex-cli" => "Codex CLI model.".to_string(),
        _ => format!("Configured in {source}."),
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

fn cli_coding_plan_bar(percent_remaining: f64) -> String {
    const WIDTH: usize = 20;
    let clamped = percent_remaining.clamp(0.0, 100.0);
    let filled = ((clamped / 100.0) * WIDTH as f64).round() as usize;
    let filled = filled.min(WIDTH);
    let mut bar = String::with_capacity(WIDTH + 2);
    bar.push('[');
    for _ in 0..filled {
        bar.push('█');
    }
    for _ in filled..WIDTH {
        bar.push('░');
    }
    bar.push(']');
    bar
}

fn cli_coding_plan_reset_text(seconds_remaining: Option<i64>) -> String {
    let Some(seconds) = seconds_remaining else {
        return "reset unknown".to_string();
    };
    if seconds <= 0 {
        return "resetting".to_string();
    }
    if seconds > 7 * 24 * 3600 {
        let days = seconds / (24 * 3600);
        return format!("reset in {}d", days);
    }
    if seconds > 24 * 3600 {
        let days = seconds / (24 * 3600);
        let hours = (seconds % (24 * 3600)) / 3600;
        return format!("reset in {}d {}h", days, hours);
    }
    if seconds >= 3600 {
        let hours = seconds / 3600;
        let minutes = (seconds % 3600) / 60;
        return format!("reset in {}h {}m", hours, minutes);
    }
    let minutes = seconds / 60;
    if minutes == 0 {
        return "reset <1m".to_string();
    }
    format!("reset in {}m", minutes)
}

fn cli_format_limit_row(
    label: &str,
    percent_remaining: Option<f64>,
    reset_seconds: Option<i64>,
    fallback: &str,
) -> (String, String) {
    let value = match percent_remaining {
        Some(percent) if percent.is_finite() => {
            let bar = cli_coding_plan_bar(percent);
            let reset = cli_coding_plan_reset_text(reset_seconds);
            format!("{} {}% left ({})", bar, percent.round() as i64, reset)
        }
        _ => fallback.to_string(),
    };
    (label.to_string(), value)
}

fn resolve_gateway_endpoint(state: &AppState, session: &CliSession) -> Option<(String, String)> {
    let provider_id = session.provider_id.as_deref()?;
    if provider_id == "codex-cli" {
        return None;
    }
    let settings_guard = state.settings.lock().ok()?;
    let settings = current_settings(&settings_guard);
    let provider = settings
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)?;
    let base_url = provider.base_url.trim();
    if base_url.is_empty() {
        return None;
    }
    let api_key = if let Some(credential_id) = session.credential_id.as_deref() {
        provider
            .credentials
            .iter()
            .find(|credential| credential.id == credential_id)
            .map(|credential| credential.api_key.trim().to_string())
            .filter(|value| !value.is_empty())
    } else {
        None
    }
    .or_else(|| {
        let legacy = provider.api_key.trim();
        if legacy.is_empty() {
            None
        } else {
            Some(legacy.to_string())
        }
    })?;
    Some((base_url.to_string(), api_key))
}

async fn fetch_coding_plan_limits(state: &AppState, session: &CliSession) -> CodingPlanLimits {
    let Some((base_url, api_key)) = resolve_gateway_endpoint(state, session) else {
        return CodingPlanLimits::default();
    };
    let trimmed = base_url.trim_end_matches('/');
    let url = if trimmed.ends_with("/v1") {
        format!("{trimmed}/internal/usage/coding-plan")
    } else {
        format!("{trimmed}/v1/internal/usage/coding-plan")
    };
    let model_filter = session.model_id.clone().or_else(|| {
        session
            .model_id
            .as_deref()
            .map(|value| value.to_ascii_lowercase())
    });
    let client = match HttpClient::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return CodingPlanLimits {
                error: Some(format!("Gateway client init failed: {error}")),
                ..Default::default()
            };
        }
    };
    let response = match client
        .get(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return CodingPlanLimits {
                error: Some(format!("Gateway request failed: {error}")),
                ..Default::default()
            };
        }
    };
    let status = response.status();
    if !status.is_success() {
        return CodingPlanLimits {
            error: Some(format!("Gateway returned HTTP {status}")),
            ..Default::default()
        };
    }
    let body: Value = match response.json().await {
        Ok(value) => value,
        Err(error) => {
            return CodingPlanLimits {
                error: Some(format!("Gateway response parse failed: {error}")),
                ..Default::default()
            };
        }
    };
    let providers = body
        .get("providers")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    if providers.is_empty() {
        return CodingPlanLimits {
            error: Some("Gateway returned no providers".to_string()),
            ..Default::default()
        };
    }
    let mut chosen_model: Option<Value> = None;
    let mut chosen_provider_name: Option<String> = None;
    for provider in &providers {
        let ok = provider
            .get("ok")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if !ok {
            continue;
        }
        let model_remains = provider
            .get("model_remains")
            .and_then(|value| value.as_array());
        let Some(models) = model_remains else {
            continue;
        };
        if let Some(filter) = model_filter.as_deref() {
            if let Some(match_model) = models.iter().find(|model| {
                model
                    .get("model_name")
                    .and_then(|value| value.as_str())
                    .map(|name| name.eq_ignore_ascii_case(filter))
                    .unwrap_or(false)
            }) {
                chosen_model = Some(match_model.clone());
                chosen_provider_name = provider
                    .get("provider_name")
                    .and_then(|value| value.as_str())
                    .map(String::from);
                break;
            }
        } else if let Some(first) = models.first() {
            chosen_model = Some(first.clone());
            chosen_provider_name = provider
                .get("provider_name")
                .and_then(|value| value.as_str())
                .map(String::from);
            break;
        }
    }
    if chosen_model.is_none() {
        for provider in &providers {
            let ok = provider
                .get("ok")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            if !ok {
                continue;
            }
            let Some(models) = provider
                .get("model_remains")
                .and_then(|value| value.as_array())
            else {
                continue;
            };
            if let Some(first) = models.first() {
                chosen_model = Some(first.clone());
                chosen_provider_name = provider
                    .get("provider_name")
                    .and_then(|value| value.as_str())
                    .map(String::from);
                break;
            }
        }
    }
    let Some(model) = chosen_model else {
        return CodingPlanLimits {
            error: Some("No matching coding plan model".to_string()),
            ..Default::default()
        };
    };
    CodingPlanLimits {
        interval_percent_remaining: model
            .get("current_interval_remaining_percent")
            .and_then(|value| value.as_f64()),
        weekly_percent_remaining: model
            .get("current_weekly_remaining_percent")
            .and_then(|value| value.as_f64()),
        interval_reset_seconds: model
            .get("remains_time")
            .and_then(coding_plan_reset_seconds),
        weekly_reset_seconds: coding_plan_weekly_reset_seconds(&model),
        source_provider: chosen_provider_name,
        source_model: model
            .get("model_name")
            .and_then(|value| value.as_str())
            .map(String::from),
        error: None,
    }
}

fn coding_plan_reset_seconds(value: &Value) -> Option<i64> {
    let raw = value
        .as_i64()
        .or_else(|| value.as_f64().map(|n| n as i64))?;
    // Upstream returns milliseconds; convert to seconds when the value
    // is clearly too large to be a direct seconds count.
    if raw > 24 * 3600 {
        Some(raw / 1000)
    } else {
        Some(raw)
    }
}

fn coding_plan_weekly_reset_seconds(model: &Value) -> Option<i64> {
    let end = model.get("weekly_end_time")?;
    let raw = end
        .as_i64()
        .or_else(|| end.as_f64().map(|n| n as i64))
        .or_else(|| {
            end.as_str().and_then(|text| {
                chrono::DateTime::parse_from_rfc3339(text)
                    .ok()
                    .map(|dt| dt.timestamp())
            })
        })?;
    // Upstream returns millisecond timestamps; a Unix-second timestamp
    // today is ~1.7e9, so anything above 1e11 is almost certainly ms.
    let seconds = if raw > 100_000_000_000 {
        raw / 1000
    } else {
        raw
    };
    let now = Utc::now().timestamp();
    Some((seconds - now).max(0))
}

fn print_cli_session(
    session: &CliSession,
    project: Option<&ProjectSession>,
    limits: Option<&CodingPlanLimits>,
    reserve_followup_input: bool,
) {
    let reasoning =
        cli_effective_reasoning_effort(session).unwrap_or_else(|| "default".to_string());
    let shell_label = if session.shell_allowed {
        "allowed (policy permits shell commands)".to_string()
    } else {
        "disabled (safe-by-default)".to_string()
    };
    let permissions = if session.shell_allowed {
        "Full Access (Ask for approval)"
    } else {
        "Custom (workspace only, Ask for approval)"
    };
    let account_label = cli_session_provider_label(session);
    let session_id = Uuid::new_v4().to_string();
    let agents_summary = cli_agents_summary_label(project);

    let mut selection: Vec<(&'static str, String)> = Vec::new();
    selection.push(("Model", cli_session_model_label(session)));
    selection.push(("Directory", cli_current_directory_label()));
    if let Some(project) = project {
        selection.push(("Workspace", cli_project_label(project)));
    }
    selection.push(("Provider", cli_session_provider_label(session)));
    if let Some(credential) = session.credential_id.as_deref() {
        selection.push((
            "Credential",
            cli_session_credential_label(session, credential),
        ));
    }
    selection.push(("Permissions", permissions.to_string()));
    selection.push(("Account", account_label));
    selection.push(("Session", session_id));
    selection.push(("Agents.md", agents_summary));
    selection.push(("Shell", shell_label));
    let reasoning_field = cli_session_reasoning_field_label(session);
    selection.push((reasoning_field, reasoning));

    let limits = limits.cloned().unwrap_or_default();
    let (interval_label, interval_value) = cli_format_limit_row(
        "5h limit",
        limits.interval_percent_remaining,
        limits.interval_reset_seconds,
        "[░░░░░░░░░░░░░░░░░░░░] not tracked (local CLI)",
    );
    let (weekly_label, weekly_value) = cli_format_limit_row(
        "Weekly limit",
        limits.weekly_percent_remaining,
        limits.weekly_reset_seconds,
        "[░░░░░░░░░░░░░░░░░░░░] not tracked (local CLI)",
    );
    let usage: Vec<(&'static str, String)> = vec![
        ("Context window", "100% left (0 used / unknown)".to_string()),
        ("5h limit", interval_value),
        ("Weekly limit", weekly_value),
    ];
    let _ = (interval_label, weekly_label);

    let label_width = selection
        .iter()
        .map(|(label, _)| terminal_display_width(label))
        .chain(usage.iter().map(|(label, _)| terminal_display_width(label)))
        .max()
        .unwrap_or(0);

    let mut body_lines: Vec<String> = Vec::new();
    body_lines.push(format!(
        "  >_ CodeForge Codex (v{})",
        env!("CARGO_PKG_VERSION")
    ));
    body_lines.push(String::new());
    for (label, value) in &selection {
        body_lines.push(format!("  {label:<label_width$}:  {value}"));
    }
    body_lines.push(String::new());
    for (label, value) in &usage {
        body_lines.push(format!("  {label:<label_width$}:  {value}"));
    }

    let natural_width = body_lines
        .iter()
        .map(|line| terminal_display_width(line))
        .max()
        .unwrap_or(0)
        .saturating_add(2)
        .max(40);
    let term_cols = crossterm::terminal::size()
        .map(|(cols, _)| cols.max(40) as usize)
        .unwrap_or(80);
    // Leave a 2-column safety margin beyond the borders so the right edge and
    // bottom border never get clipped by the REPL chrome or terminal width.
    // Also cap the absolute width so inflated terminal reports in the REPL
    // (alternate screen buffer, scrollback) cannot push the box off-screen.
    let max_inner_width = term_cols.saturating_sub(4).max(20).min(120);
    let inner_width = natural_width.min(max_inner_width);

    let horizontal = "─".repeat(inner_width);
    let total_rows = body_lines.len() as u16 + 3;
    let reserve_rows = if reserve_followup_input {
        total_rows.saturating_add(codex_inline_reserved_height_for_popup(false, 0))
    } else {
        total_rows
    };
    let mut stdout = io::stdout();
    if !stdout.is_terminal() {
        let _ = writeln!(stdout);
        let _ = writeln!(stdout, "╭{horizontal}╮");
        for line in &body_lines {
            let padded = pad_display_width(&truncate_display_width(line, inner_width), inner_width);
            let _ = writeln!(stdout, "│{padded}│");
        }
        let _ = writeln!(stdout, "╰{horizontal}╯");
        let _ = stdout.flush();
        return;
    }

    let _ = writeln!(stdout);
    let append_row = current_append_row().unwrap_or(0);
    let start_row = reserve_terminal_rows(append_row, reserve_rows).unwrap_or(append_row);
    let _ = move_to_append_row(&mut stdout, start_row);
    let _ = writeln!(stdout, "╭{horizontal}╮");
    for line in &body_lines {
        let padded = pad_display_width(&truncate_display_width(line, inner_width), inner_width);
        let _ = writeln!(stdout, "│{padded}│");
    }
    let _ = writeln!(stdout, "╰{horizontal}╯");
    let _ = stdout.flush();
}

fn cli_agents_summary_label(project: Option<&ProjectSession>) -> String {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(project) = project {
        candidates.push(PathBuf::from(&project.repo_root).join("AGENTS.md"));
        candidates.push(PathBuf::from(&project.repo_root).join("agents.md"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("AGENTS.md"));
        candidates.push(cwd.join("agents.md"));
    }
    for candidate in &candidates {
        if candidate.is_file() {
            return cli_display_path_text(&candidate.display().to_string());
        }
    }
    "<none>".to_string()
}

fn cli_coding_plan_limits_json(limits: Option<&CodingPlanLimits>) -> Value {
    let mut rate_limits = serde_json::Map::new();
    let Some(limits) = limits else {
        rate_limits.insert(
            "5h".to_string(),
            json!({ "tracked": false, "label": "not tracked (local CLI)" }),
        );
        rate_limits.insert(
            "weekly".to_string(),
            json!({ "tracked": false, "label": "not tracked (local CLI)" }),
        );
        return Value::Object(rate_limits);
    };
    let build_entry = |percent: Option<f64>, reset_seconds: Option<i64>| -> Value {
        match percent {
            Some(value) if value.is_finite() => {
                let bar = cli_coding_plan_bar(value);
                let reset = cli_coding_plan_reset_text(reset_seconds);
                json!({
                    "tracked": true,
                    "percent_remaining": value,
                    "bar": bar,
                    "reset_in": reset,
                    "reset_seconds": reset_seconds,
                    "label": format!("{} {}% left ({})", bar, value.round() as i64, reset),
                })
            }
            _ => {
                json!({
                    "tracked": false,
                    "label": "not tracked (local CLI)",
                    "error": limits.error.clone(),
                })
            }
        }
    };
    rate_limits.insert(
        "5h".to_string(),
        build_entry(
            limits.interval_percent_remaining,
            limits.interval_reset_seconds,
        ),
    );
    rate_limits.insert(
        "weekly".to_string(),
        build_entry(limits.weekly_percent_remaining, limits.weekly_reset_seconds),
    );
    if let Some(provider) = limits.source_provider.as_deref() {
        rate_limits.insert(
            "source_provider".to_string(),
            Value::String(provider.to_string()),
        );
    }
    if let Some(model) = limits.source_model.as_deref() {
        rate_limits.insert("source_model".to_string(), Value::String(model.to_string()));
    }
    if let Some(error) = limits.error.as_deref() {
        rate_limits.insert("error".to_string(), Value::String(error.to_string()));
    }
    Value::Object(rate_limits)
}

fn print_cli_session_json(
    session: &CliSession,
    project: Option<&ProjectSession>,
    limits: Option<&CodingPlanLimits>,
) -> Result<(), String> {
    let reasoning =
        cli_effective_reasoning_effort(session).unwrap_or_else(|| "default".to_string());
    let credential = session.credential_id.as_deref().map(|credential| {
        json!({
            "id": mask_cli_status_value(credential),
            "name": session.credential_name,
            "label": cli_session_credential_label(session, credential),
        })
    });
    let workspace = project.map(|project| {
        json!({
            "id": project.id,
            "name": project.name,
            "root": cli_display_path_text(&project.repo_root),
            "source": cli_project_source(project),
        })
    });
    let permissions = if session.shell_allowed {
        "Full Access (Ask for approval)"
    } else {
        "Custom (workspace only, Ask for approval)"
    };
    let payload = json!({
        "app": "codeforge",
        "surface": "cli",
        "version": env!("CARGO_PKG_VERSION"),
        "model": cli_session_model_label(session),
        "directory": cli_current_directory_label(),
        "workspace": workspace,
        "shell": {
            "allowed": session.shell_allowed,
            "label": if session.shell_allowed {
                "allowed (policy permits shell commands)"
            } else {
                "disabled (safe-by-default)"
            },
        },
        "provider": {
            "id": session.provider_id,
            "name": session.provider_name,
            "label": cli_session_provider_label(session),
        },
        "credential": credential,
        "permissions": permissions,
        "account": cli_session_provider_label(session),
        "session": Uuid::new_v4().to_string(),
        "agents_md": cli_agents_summary_label(project),
        "reasoning": {
            "field": cli_session_reasoning_field_label(session),
            "value": reasoning,
        },
        "usage": {
            "context_window": {
                "percent_remaining": 100,
                "tokens_used": 0,
                "window": null,
                "label": "100% left (0 used / unknown)",
            },
            "rate_limits": cli_coding_plan_limits_json(limits),
        },
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn cli_project_label(project: &ProjectSession) -> String {
    let repo_root = cli_display_path_text(&project.repo_root);
    let label = if project.name.trim().is_empty() || project.name == project.repo_root {
        repo_root
    } else {
        format!("{} ({})", project.name, repo_root)
    };
    if project.id == "cli-current-directory" {
        format!("{label} [current directory]")
    } else if project.id == "cli-explicit-path" {
        format!("{label} [explicit path]")
    } else {
        label
    }
}

fn cli_project_source(project: &ProjectSession) -> &'static str {
    match project.id.as_str() {
        "cli-current-directory" => "currentDirectory",
        "cli-explicit-path" => "explicitPath",
        _ => "registered",
    }
}

fn cli_display_path_text(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        path.to_string()
    }
}

fn cli_session_provider_label(session: &CliSession) -> String {
    match (
        session.provider_name.as_deref(),
        session.provider_id.as_deref(),
    ) {
        (Some(name), Some(id)) if !name.trim().is_empty() && name != id => {
            format!("{name} ({id})")
        }
        (Some(name), _) if !name.trim().is_empty() => name.to_string(),
        (_, Some(id)) => id.to_string(),
        _ => "auto".to_string(),
    }
}

fn cli_session_credential_label(session: &CliSession, credential_id: &str) -> String {
    match session.credential_name.as_deref() {
        Some(name) if !name.trim().is_empty() && name != credential_id => {
            format!("{} ({})", name, mask_cli_status_value(credential_id))
        }
        Some(name) if !name.trim().is_empty() => name.to_string(),
        _ => mask_cli_status_value(credential_id),
    }
}

fn mask_cli_status_value(value: &str) -> String {
    let value = value.trim();
    if value.len() <= 8 || value.starts_with("key-") {
        return value.to_string();
    }
    let prefix = value.chars().take(4).collect::<String>();
    let suffix = value
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("{prefix}...{suffix}")
}

fn cli_model_choices(settings: &AppSettings) -> Vec<CliModelChoice> {
    settings
        .providers
        .iter()
        .filter(|provider| provider.enabled || provider.models.iter().any(|model| model.enabled))
        .flat_map(provider_model_choices)
        .filter(|choice| !(choice.provider_id == "codex-cli" && choice.model_id == "default"))
        .collect()
}

fn provider_model_choices(provider: &ProviderConfig) -> Vec<CliModelChoice> {
    if !provider_uses_credentials(provider) {
        let enabled_models = provider
            .models
            .iter()
            .filter(|model| model.enabled)
            .cloned()
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
                    reasoning_mode: "effort".to_string(),
                    default_reasoning: "medium".to_string(),
                })
                .into_iter()
                .collect();
        }
        return enabled_models
            .into_iter()
            .map(|model| CliModelChoice {
                provider_id: provider.id.clone(),
                provider_name: provider.name.clone(),
                credential_id: None,
                credential_name: None,
                model_id: model.id,
                model_name: model.name,
                reasoning_mode: model.reasoning_mode,
                default_reasoning: model.default_reasoning,
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
            reasoning_mode: model.reasoning_mode.clone(),
            default_reasoning: model.default_reasoning.clone(),
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
    if value.eq_ignore_ascii_case("none") {
        return Some("off".to_string());
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
    read_stdin_line(stdin)
}

fn read_stdin_line(stdin: &io::Stdin) -> Result<Option<String>, String> {
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
        let normalized = PathBuf::from(value).canonicalize().ok();
        if let Some(project) = projects.into_iter().find(|project| {
            project.id == value
                || project.name == value
                || normalized
                    .as_ref()
                    .map(|path| {
                        Path::new(&project.repo_root).canonicalize().ok().as_ref() == Some(path)
                    })
                    .unwrap_or(false)
        }) {
            return Ok(project);
        }
        if let Some(path) = normalized.as_deref() {
            return Ok(cli_project_from_explicit_path(path));
        }
        return Err(format!("Project not found: {value}"));
    }
    let cwd = std::env::current_dir()
        .map_err(|e| e.to_string())?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if let Some(project) = projects
        .iter()
        .find(|project| Path::new(&project.repo_root).canonicalize().ok().as_ref() == Some(&cwd))
        .cloned()
    {
        return Ok(project);
    }
    Ok(cli_project_from_path(&cwd))
}

fn cli_project_from_path(root: &Path) -> ProjectSession {
    let repo_root = cli_display_path_text(&root.display().to_string());
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("workspace")
        .to_string();
    let now = Utc::now().to_rfc3339();
    ProjectSession {
        id: "cli-current-directory".to_string(),
        name,
        repo_root,
        solution_path: None,
        uproject_path: None,
        build_command: None,
        vs_process_id: None,
        vs_bridge_endpoint: None,
        created_at: now.clone(),
        updated_at: now,
    }
}

fn cli_project_from_explicit_path(root: &Path) -> ProjectSession {
    let mut project = cli_project_from_path(root);
    project.id = "cli-explicit-path".to_string();
    project
}

struct FinalResponseParts {
    reasoning: Option<String>,
    content: String,
}

fn final_response(run: &MockAgentRun) -> Option<FinalResponseParts> {
    run.traces.iter().rev().find_map(|event| {
        if !matches!(
            event.event_type,
            TraceEventType::FinalResponse | TraceEventType::ModelMessage
        ) {
            return None;
        }
        let (response_content, response_reasoning) =
            response_content_and_reasoning(event.output.as_ref());
        let content = response_content
            .or_else(|| event.output_summary.clone())
            .unwrap_or_default();
        let reasoning = response_reasoning
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
        if content.is_empty() && reasoning.is_none() {
            return event
                .output_summary
                .clone()
                .map(|summary| FinalResponseParts {
                    reasoning: None,
                    content: summary,
                });
        }
        Some(FinalResponseParts { reasoning, content })
    })
}

fn response_content_and_reasoning(
    output: Option<&serde_json::Value>,
) -> (Option<String>, Option<String>) {
    let Some(output) = output else {
        return (None, None);
    };
    // Prefer the full OpenAI-shaped response body (written by
    // record_plain_provider_completion and push_final_response_trace) so we
    // can read both `choices[0].message.content` and the separated
    // `choices[0].message.reasoning_content` field that MiniMax-M3 returns
    // when `reasoning_split: true` is set on the request.
    if let Some(response) = output.get("response") {
        if let Some(choice) = response
            .get("choices")
            .and_then(|choices| choices.as_array())
            .and_then(|choices| choices.first())
        {
            if let Some(message) = choice.get("message") {
                let content = message
                    .get("content")
                    .and_then(|value| value.as_str())
                    .map(ToString::to_string);
                let reasoning = message
                    .get("reasoning_content")
                    .and_then(|value| value.as_str())
                    .map(ToString::to_string);
                if content.is_some() || reasoning.is_some() {
                    return (content, reasoning);
                }
            }
        }
    }
    let content = output
        .get("message")
        .and_then(|value| value.as_str())
        .map(ToString::to_string);
    (content, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_input_start_row_is_current_plus_one() {
        // The current implementation: next_chat_input_start_row = current + 1.
        assert_eq!(compute_next_chat_input_start_row(0), 1);
        assert_eq!(compute_next_chat_input_start_row(10), 11);
        assert_eq!(compute_next_chat_input_start_row(100), 101);
    }

    #[test]
    fn composer_height_is_at_least_three() {
        // Empty input + short input both produce a 3-row composer.
        let cols = 80;
        let empty = codex_composer_height_for_input(cols, CHAT_PROMPT, "", 0);
        let short = codex_composer_height_for_input(cols, CHAT_PROMPT, "hello", 5);
        assert!(empty >= CODEX_COMPOSER_MIN_ROWS);
        assert!(short >= CODEX_COMPOSER_MIN_ROWS);
    }

    #[test]
    fn predict_next_composer_row_baseline_has_1_gap() {
        // Baseline: finalize +1, agent 3 lines, println!() +1, next_chat +1
        // → old composer end at N+2, agent 3 lines N+3..N+5, println to N+6,
        // next_chat N+7. So 1 line gap between agent's last line (N+5) and
        // new composer (N+7).
        let old_composer_start = 6;
        let old_composer_height = 3;
        let agent_lines = 3;
        let with_println = true;
        let row = predict_next_composer_row(
            old_composer_start,
            old_composer_height,
            agent_lines,
            with_println,
        );
        // 6 + 3 + 3 + 1 + 1 = 14
        assert_eq!(row, 14);
    }

    #[test]
    fn predict_next_composer_row_without_println_has_0_gap() {
        // If we remove the trailing println!(), the gap drops by 1.
        let row = predict_next_composer_row(6, 3, 3, false);
        // 6 + 3 + 3 + 0 + 1 = 13
        assert_eq!(row, 13);
    }
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
