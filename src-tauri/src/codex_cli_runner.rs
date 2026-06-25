use std::env;
use std::io::{Read, Write};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

pub const CODEX_CLI_PROVIDER_TYPE: &str = "codex-cli";
pub const CODEX_CLI_DEFAULT_MODEL: &str = "default";
pub const CODEX_CLI_TOOL_NAME: &str = "codex_exec";

const CODEX_EXEC_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const CODEX_EXEC_SANDBOX: &str = "workspace-write";
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Debug)]
pub struct CodexCliExecution {
    pub executable: String,
    pub args: Vec<String>,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    pub prompt_write_error: Option<String>,
    pub events: Vec<Value>,
    pub non_json_stdout_lines: Vec<String>,
    pub final_message: String,
    pub usage: Option<Value>,
}

pub async fn execute(
    workspace_root: &str,
    prompt: &str,
    model_id: Option<&str>,
    reasoning_effort: Option<&str>,
) -> Result<CodexCliExecution, String> {
    let executable = resolve_codex_cli_path()?;
    let args = codex_exec_args(workspace_root, model_id, reasoning_effort);
    let workspace_root = workspace_root.to_string();
    let prompt = prompt.to_string();

    tauri::async_runtime::spawn_blocking(move || {
        run_codex_process(executable, args, workspace_root, prompt)
    })
    .await
    .map_err(|error| format!("Codex CLI worker failed: {error}"))?
}

pub fn is_codex_cli_provider(provider_type: &str) -> bool {
    provider_type == CODEX_CLI_PROVIDER_TYPE
}

pub fn model_override(model_id: &str) -> Option<&str> {
    let model_id = model_id.trim();
    if model_id.is_empty() || model_id == CODEX_CLI_DEFAULT_MODEL {
        None
    } else {
        Some(model_id)
    }
}

fn run_codex_process(
    executable: PathBuf,
    args: Vec<String>,
    workspace_root: String,
    prompt: String,
) -> Result<CodexCliExecution, String> {
    let started = Instant::now();
    let mut command = Command::new(&executable);
    hide_child_console(&mut command);
    let mut child = command
        .args(&args)
        .current_dir(&workspace_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            format!(
                "Failed to start Codex CLI at {}: {error}",
                executable.display()
            )
        })?;

    let prompt_write_error = child.stdin.take().and_then(|mut stdin| {
        stdin
            .write_all(prompt.as_bytes())
            .err()
            .map(|error| format!("Failed to write prompt to Codex CLI stdin: {error}"))
    });

    let stdout_handle = child
        .stdout
        .take()
        .map(read_pipe_to_string)
        .ok_or_else(|| "Failed to capture Codex CLI stdout.".to_string())?;
    let stderr_handle = child
        .stderr
        .take()
        .map(read_pipe_to_string)
        .ok_or_else(|| "Failed to capture Codex CLI stderr.".to_string())?;

    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("Failed to poll Codex CLI: {error}"))?
        {
            break status;
        }
        if started.elapsed() > CODEX_EXEC_TIMEOUT {
            timed_out = true;
            let _ = child.kill();
            break child
                .wait()
                .map_err(|error| format!("Failed to wait for killed Codex CLI: {error}"))?;
        }
        thread::sleep(Duration::from_millis(100));
    };

    let stdout = stdout_handle
        .join()
        .map_err(|_| "Codex CLI stdout reader panicked.".to_string())??;
    let stderr = stderr_handle
        .join()
        .map_err(|_| "Codex CLI stderr reader panicked.".to_string())??;
    let (events, non_json_stdout_lines) = parse_jsonl_stdout(&stdout);
    let final_message = find_final_message(&events).unwrap_or_default();
    let usage = find_usage(&events);

    Ok(CodexCliExecution {
        executable: executable.to_string_lossy().to_string(),
        args,
        duration_ms: started.elapsed().as_millis() as u64,
        exit_code: status.code(),
        timed_out,
        stdout,
        stderr,
        prompt_write_error,
        events,
        non_json_stdout_lines,
        final_message,
        usage,
    })
}

fn hide_child_console(command: &mut Command) {
    #[cfg(windows)]
    {
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}

fn read_pipe_to_string<R>(mut pipe: R) -> thread::JoinHandle<Result<String, String>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut text = String::new();
        pipe.read_to_string(&mut text)
            .map_err(|error| format!("Failed to read Codex CLI pipe: {error}"))?;
        Ok(text)
    })
}

fn codex_exec_args(
    workspace_root: &str,
    model_id: Option<&str>,
    reasoning_effort: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        "exec".to_string(),
        "--json".to_string(),
        "--ephemeral".to_string(),
        "--sandbox".to_string(),
        CODEX_EXEC_SANDBOX.to_string(),
        "--cd".to_string(),
        workspace_root.to_string(),
    ];
    if let Some(model_id) = model_id.filter(|value| !value.trim().is_empty()) {
        args.push("--model".to_string());
        args.push(model_id.to_string());
    }
    if let Some(reasoning_effort) = reasoning_effort.filter(|value| !value.trim().is_empty()) {
        args.push("--config".to_string());
        args.push(format!("model_reasoning_effort=\"{reasoning_effort}\""));
    }
    args.push("-".to_string());
    args
}

fn resolve_codex_cli_path() -> Result<PathBuf, String> {
    if let Ok(path) = env::var("CODEX_CLI_PATH") {
        if let Some(path) = existing_file(path) {
            return Ok(path);
        }
    }

    let mut candidates = Vec::new();
    if let Ok(local_app_data) = env::var("LOCALAPPDATA") {
        candidates.push(
            Path::new(&local_app_data)
                .join("OpenAI")
                .join("Codex")
                .join("bin")
                .join(executable_name()),
        );
        candidates.push(
            Path::new(&local_app_data)
                .join("Programs")
                .join("OpenAI")
                .join("Codex")
                .join("bin")
                .join(executable_name()),
        );
    }

    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    if let Ok(path_var) = env::var("PATH") {
        for dir in env::split_paths(&path_var) {
            let candidate = dir.join(executable_name());
            if candidate.is_file() && !is_windows_apps_path(&candidate) {
                return Ok(candidate);
            }
        }
    }

    Err(format!(
        "Codex CLI executable was not found. Install it with the official Windows standalone installer or set CODEX_CLI_PATH to codex.exe."
    ))
}

fn existing_file(path: String) -> Option<PathBuf> {
    let path = PathBuf::from(path.trim_matches('"'));
    path.is_file().then_some(path)
}

fn executable_name() -> &'static str {
    if cfg!(windows) {
        "codex.exe"
    } else {
        "codex"
    }
}

fn is_windows_apps_path(path: &Path) -> bool {
    path.to_string_lossy()
        .to_ascii_lowercase()
        .contains("\\windowsapps\\")
}

fn parse_jsonl_stdout(stdout: &str) -> (Vec<Value>, Vec<String>) {
    let mut events = Vec::new();
    let mut non_json = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(value) => events.push(value),
            Err(_) => non_json.push(line.to_string()),
        }
    }
    (events, non_json)
}

fn find_final_message(events: &[Value]) -> Option<String> {
    events.iter().rev().find_map(agent_message_text)
}

fn agent_message_text(event: &Value) -> Option<String> {
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let item = event.get("item").unwrap_or(event);
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
    if event_type != "item.completed"
        && event_type != "agent_message"
        && item_type != "agent_message"
        && item_type != "message"
    {
        return None;
    }

    item.get("text")
        .and_then(Value::as_str)
        .or_else(|| item.get("message").and_then(Value::as_str))
        .or_else(|| item.get("content").and_then(Value::as_str))
        .map(str::to_string)
}

fn find_usage(events: &[Value]) -> Option<Value> {
    events
        .iter()
        .rev()
        .find_map(|event| event.get("usage").cloned())
}

pub fn limited_events(events: &[Value]) -> Value {
    const MAX_EVENTS: usize = 500;
    let captured = events.iter().take(MAX_EVENTS).cloned().collect::<Vec<_>>();
    json!({
        "items": captured,
        "total": events.len(),
        "truncated": events.len() > MAX_EVENTS,
    })
}

pub fn token_usage_from_codex_usage(usage: Option<&Value>) -> Value {
    let input_tokens = usage.and_then(|value| value.get("input_tokens")).cloned();
    let output_tokens = usage.and_then(|value| value.get("output_tokens")).cloned();
    let cached_input_tokens = usage
        .and_then(|value| value.get("cached_input_tokens"))
        .cloned();
    let total_tokens = sum_u64_values(input_tokens.as_ref(), output_tokens.as_ref());
    json!({
        "inputTokens": input_tokens,
        "outputTokens": output_tokens,
        "totalTokens": total_tokens,
        "inputCachedTokens": cached_input_tokens,
        "usage": usage.cloned(),
    })
}

fn sum_u64_values(left: Option<&Value>, right: Option<&Value>) -> Option<u64> {
    Some(left?.as_u64()? + right?.as_u64()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_codex_jsonl_final_message_and_usage() {
        let stdout = r#"{"type":"thread.started","thread_id":"t1"}
{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"Done."}}
{"type":"turn.completed","usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}"#;
        let (events, non_json) = parse_jsonl_stdout(stdout);

        assert!(non_json.is_empty());
        assert_eq!(events.len(), 3);
        assert_eq!(find_final_message(&events).as_deref(), Some("Done."));
        assert_eq!(
            find_usage(&events).unwrap()["cached_input_tokens"],
            json!(2)
        );
    }

    #[test]
    fn codex_exec_args_use_stdin_prompt() {
        let args = codex_exec_args("D:\\code\\snowAgents", Some("gpt-5.5"), Some("high"));

        assert!(args.contains(&"--json".to_string()));
        assert!(args.contains(&"--ephemeral".to_string()));
        assert_eq!(args.last().map(String::as_str), Some("-"));
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "--model" && pair[1] == "gpt-5.5"));
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "--config" && pair[1] == "model_reasoning_effort=\"high\""));
    }
}
