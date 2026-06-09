use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::json;

use crate::agent_runner::{self, AgentRunInput};
use crate::app_state::{current_settings, AppState};
use crate::project_registry::{ProjectInput, ProjectSession};
use crate::tool_trace::{MockAgentRun, ToolTraceEvent, TraceEventType};

#[derive(Debug)]
pub struct Cli {
    pub project: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub no_shell: bool,
    pub yes: bool,
    pub json: bool,
    pub verbose: bool,
    pub command: Command,
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
        model: None,
        no_shell: false,
        yes: false,
        json: false,
        verbose: false,
        command: Command::Models {
            command: ModelsCommand::List,
        },
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
            "--model" => {
                i += 1;
                cli.model = Some(take_arg(&args, i, "--model")?);
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
            other => return Err(format!("Unknown projects add argument: {other}")),
        }
        i += 1;
    }
    Ok(parsed)
}

fn help_text() -> String {
    "Usage: codeforge [--project <name-or-path>] [--provider <provider>] [--model <model>] [--no-shell] [--yes] [--json] [--verbose] <chat|run|projects|models>\n\nCommands:\n  codeforge chat\n  codeforge run \"<task>\"\n  codeforge projects list\n  codeforge projects add [--name <name>] [--path <path>] [--solution <solution.sln>]\n  codeforge models list".to_string()
}

pub async fn run(cli: Cli) -> Result<(), String> {
    let state = AppState::load()?;
    match &cli.command {
        Command::Projects { command } => run_projects(&state, command, cli.json),
        Command::Models { command } => run_models(&state, command, cli.json),
        Command::Run { task } => run_task(&state, &cli, task).await.map(|_| ()),
        Command::Chat => run_chat(&state, &cli).await,
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
            let solution = args
                .solution
                .clone()
                .or_else(|| find_solution(&root))
                .ok_or_else(|| {
                    "No .sln found. Pass --solution <solution.sln> for this project.".to_string()
                })?;
            let input = ProjectInput {
                name,
                repo_root: root,
                solution_path: solution,
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
            let settings = current_settings(
                &state
                    .settings
                    .lock()
                    .map_err(|_| "settings lock failed".to_string())?,
            );
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
    println!("CodeForge chat. Type /exit to quit.");
    let stdin = io::stdin();
    loop {
        print!("> ");
        io::stdout().flush().map_err(|e| e.to_string())?;
        let mut line = String::new();
        let read = stdin.read_line(&mut line).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        let task = line.trim();
        if task.is_empty() {
            continue;
        }
        if matches!(task, "/exit" | "/quit") {
            break;
        }
        run_task(state, cli, task).await?;
    }
    Ok(())
}

async fn run_task(state: &AppState, cli: &Cli, task: &str) -> Result<MockAgentRun, String> {
    let project = select_project(state, cli.project.as_deref())?;
    let settings = current_settings(
        &state
            .settings
            .lock()
            .map_err(|_| "settings lock failed".to_string())?,
    );
    let input = AgentRunInput {
        project_id: project.id.clone(),
        user_prompt: task.to_string(),
        messages: None,
        provider_id: cli.provider.clone(),
        credential_id: None,
        model_id: cli.model.clone(),
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
    projects.iter().find(|project| Path::new(&project.repo_root).canonicalize().ok().as_ref() == Some(&cwd)).cloned()
        .or_else(|| projects.first().cloned())
        .ok_or_else(|| "No projects registered. Run `codeforge projects add --path <workspace> --solution <solution.sln>` first.".to_string())
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

fn find_solution(root: &str) -> Option<String> {
    fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("sln"))
                .unwrap_or(false)
        })
        .map(|path| path.to_string_lossy().to_string())
}
