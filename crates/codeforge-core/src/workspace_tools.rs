use std::cmp;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use regex::{Regex, RegexBuilder};
use serde_json::{json, Value};

use crate::file_search::{FileSearchOptions, MatchType};
use crate::path_utils::normalize_display_path;

const DEFAULT_MAX_READ_LINES: usize = 300;
const DEFAULT_MAX_RESULTS: usize = 100;
const MAX_RESULTS_LIMIT: usize = 500;
const DEFAULT_CONTENT_CONTEXT_LINES: usize = 2;
const DEFAULT_FILE_CONTEXT_BEFORE: usize = 30;
const DEFAULT_FILE_CONTEXT_AFTER: usize = 30;
const MAX_CONTEXT_LINES: usize = 200;
const BINARY_SAMPLE_BYTES: usize = 8192;
const SEARCH_CONTENT_SCAN_LIMIT: usize = 25_000;
const SEARCH_SCAN_TIMEOUT_MS: u128 = 12_000;
const READ_FILE_RECOVERY_SCAN_LIMIT: usize = 8_000;
const READ_FILE_RECOVERY_MAX_CANDIDATES: usize = 8;
const READ_FILE_RECOVERY_MAX_ROOTS: usize = 8;
const DEFAULT_GIT_OUTPUT_MAX_BYTES: usize = 200_000;
const MAX_GIT_OUTPUT_MAX_BYTES: usize = 500_000;
const GIT_COMMAND_TIMEOUT_MS: u64 = 120_000;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

const IGNORED_DIRS: &[&str] = &[
    ".git",
    ".vs",
    "bin",
    "obj",
    "build",
    "out",
    "node_modules",
    ".cache",
];

const IGNORED_SEARCH_PATHS: &[&[&str]] = &[&[".claude", "worktrees"]];

const DEFAULT_CONTENT_EXTENSIONS: &[&str] = &[
    ".h", ".hpp", ".c", ".cpp", ".cc", ".cxx", ".inl", ".ixx", ".cs", ".sln", ".vcxproj", ".props",
    ".targets", ".json", ".xml", ".txt", ".md", ".log",
];

pub fn list_dir(workspace_root: &str, arguments: &Value) -> Result<Value, String> {
    let workspace = canonical_workspace_root(workspace_root)?;
    let raw_path = required_string(arguments, "path")?;
    let dir = resolve_existing_read_path(&workspace, &raw_path)?;
    if !dir.is_dir() {
        return Err(format!(
            "not_directory: {}",
            relative_or_display(&workspace, &dir)
        ));
    }

    let mut directories = Vec::new();
    let mut files = Vec::new();
    let mut entries = Vec::new();
    for entry in sorted_read_dir(&dir)? {
        let path = entry.path();
        if path.is_dir() {
            if is_ignored_dir(&path) {
                continue;
            }
            let canonical = canonicalize_path(&path)
                .map_err(|error| format!("read_dir_failed: {}: {error}", path.display()))?;
            let relative = relative_path(&workspace, &canonical);
            directories.push(relative.clone());
            entries.push(json!({
                "path": relative,
                "type": "directory",
            }));
        } else if path.is_file() {
            let canonical = canonicalize_path(&path)
                .map_err(|error| format!("file_not_found: {}: {error}", path.display()))?;
            let relative = relative_path(&workspace, &canonical);
            files.push(relative.clone());
            entries.push(json!({
                "path": relative,
                "type": "file",
            }));
        }
    }

    directories.sort_by_key(|path| path.to_ascii_lowercase());
    files.sort_by_key(|path| path.to_ascii_lowercase());
    entries.sort_by_key(|entry| {
        entry
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase()
    });

    Ok(json!({
        "path": relative_path(&workspace, &dir),
        "directories": directories,
        "files": files,
        "entries": entries,
    }))
}

pub fn read_file(workspace_root: &str, arguments: &Value) -> Result<Value, String> {
    let workspace = canonical_workspace_root(workspace_root)?;
    let raw_path = required_string(arguments, "path")?;
    let resolved = match resolve_existing_read_path(&workspace, &raw_path) {
        Ok(file) => ResolvedReadFile {
            file,
            recovery: None,
        },
        Err(error) if error.starts_with("file_not_found:") => {
            match recover_missing_read_file(&workspace, &raw_path)? {
                Some(resolved) => resolved,
                None => return Err(error),
            }
        }
        Err(error) => return Err(error),
    };
    let file = resolved.file;
    ensure_regular_text_file(&workspace, &file)?;

    let start_line = optional_usize(arguments, "start_line", 1)?;
    if start_line == 0 {
        return Err("invalid_range: start_line must be >= 1".to_string());
    }
    let requested_end_line = optional_usize_value(arguments, "end_line")?;
    if let Some(end_line) = requested_end_line {
        if end_line < start_line {
            return Err("invalid_range: end_line must be >= start_line".to_string());
        }
    }

    let max_end_line = start_line.saturating_add(DEFAULT_MAX_READ_LINES - 1);
    let requested_end = requested_end_line.unwrap_or(max_end_line);
    let desired_end = cmp::min(requested_end, max_end_line);
    let (lines, total_lines) = collect_line_range(&file, start_line, desired_end, true)?;

    let actual_start_line = if lines.is_empty() { 0 } else { start_line };
    let actual_end_line = lines
        .last()
        .and_then(|line| line.get("line"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let truncated = requested_end > desired_end || total_lines > actual_end_line as usize;
    let message = if truncated {
        Some(format!(
            "too_many_results: file has {total_lines} lines; use start_line/end_line to read more"
        ))
    } else {
        None
    };
    let text = lines
        .iter()
        .filter_map(|line| line.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");

    let mut result = json!({
        "file": relative_path(&workspace, &file),
        "totalLines": total_lines,
        "startLine": actual_start_line,
        "endLine": actual_end_line,
        "maxLines": DEFAULT_MAX_READ_LINES,
        "truncated": truncated,
        "message": message,
        "text": text,
        "lines": lines,
    });
    if let Some(recovery) = resolved.recovery {
        result["recovered"] = json!(true);
        result["recovery"] = recovery.to_json(&workspace, &file);
    }
    Ok(result)
}

pub fn search_file(workspace_root: &str, arguments: &Value) -> Result<Value, String> {
    let workspace = canonical_workspace_root(workspace_root)?;
    let pattern = required_string(arguments, "pattern")?;
    let root = resolve_search_root(&workspace, arguments)?;
    let max_results = max_results(arguments)?;
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return Err("invalid_arguments: pattern must not be empty".to_string());
    }
    if wildcard_pattern(pattern) {
        return search_file_with_wildcard(&workspace, &root, pattern, max_results);
    }

    let search_result = crate::file_search::run(
        pattern,
        vec![root.clone()],
        FileSearchOptions {
            limit: std::num::NonZero::new(max_results)
                .ok_or_else(|| "invalid_arguments: max_results must be >= 1".to_string())?,
            exclude: ignored_search_excludes(),
            threads: std::num::NonZero::new(2).expect("2 is non-zero"),
            compute_indices: true,
            respect_gitignore: true,
        },
        Some(Arc::new(AtomicBool::new(false))),
    )
    .map_err(|error| format!("search_failed: {error}"))?;
    let total_matches = search_result.total_match_count;
    let matches = search_result
        .matches
        .into_iter()
        .map(|entry| {
            let absolute = entry.full_path();
            let canonical = canonicalize_path(&absolute).unwrap_or(absolute);
            json!({
                "path": relative_path(&workspace, &canonical),
                "type": match entry.match_type {
                    MatchType::Directory => "directory",
                    MatchType::File => "file",
                },
                "score": entry.score,
                "indices": entry.indices.unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();
    let truncated = total_matches > matches.len();
    let paths = matches
        .iter()
        .filter_map(|entry| entry.get("path").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();

    Ok(json!({
        "root": relative_path(&workspace, &root),
        "pattern": pattern,
        "matches": matches,
        "paths": paths,
        "count": cmp::min(total_matches, max_results),
        "totalMatches": total_matches,
        "shown": matches.len(),
        "complete": true,
        "maxResults": max_results,
        "truncated": truncated,
        "engine": "codex-file-search",
        "message": if truncated {
            Some(format!("too_many_results: returned first {max_results} of {total_matches} matches"))
        } else {
            None
        }
    }))
}

fn search_file_with_wildcard(
    workspace: &Path,
    root: &Path,
    pattern: &str,
    max_results: usize,
) -> Result<Value, String> {
    let mut matches = Vec::new();
    let mut truncated = false;
    let scanned_files = walk_files_until(workspace, root, &mut |path, scanned| {
        if scanned > SEARCH_CONTENT_SCAN_LIMIT {
            truncated = true;
            return Ok(WalkControl::Stop);
        }
        let relative = relative_path(workspace, path);
        if matches_file_glob(&relative, pattern) {
            if matches.len() >= max_results {
                truncated = true;
                return Ok(WalkControl::Stop);
            }
            matches.push(json!({
                "path": relative,
                "type": "file",
                "score": 0,
                "indices": [],
            }));
        }
        Ok(WalkControl::Continue)
    })?;
    let paths = matches
        .iter()
        .filter_map(|entry| entry.get("path").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();

    Ok(json!({
        "root": relative_path(workspace, root),
        "pattern": pattern,
        "matches": matches,
        "paths": paths,
        "count": paths.len(),
        "totalMatches": paths.len(),
        "shown": paths.len(),
        "complete": !truncated,
        "maxResults": max_results,
        "truncated": truncated,
        "scannedFiles": scanned_files,
        "engine": "wildcard-file-search",
        "message": if truncated {
            Some(format!("search_limited: returned {max_results} matches before scanning all files"))
        } else {
            None
        }
    }))
}

pub fn search_content(workspace_root: &str, arguments: &Value) -> Result<Value, String> {
    let workspace = canonical_workspace_root(workspace_root)?;
    let query = required_string(arguments, "query")?;
    if query.trim().is_empty() {
        return Err("invalid_arguments: query must not be empty".to_string());
    }

    let root = resolve_search_path(&workspace, arguments)?;
    let file_glob = optional_string(arguments, "file_glob")?;
    let max_results = max_results(arguments)?;
    let context_lines = optional_usize(arguments, "context_lines", DEFAULT_CONTENT_CONTEXT_LINES)?
        .min(MAX_CONTEXT_LINES);
    let case_sensitive = optional_bool(arguments, "case_sensitive", false)?;
    let regex = optional_bool(arguments, "regex", false)?;
    let compiled_regex = if regex {
        Some(compile_regex(&query, case_sensitive)?)
    } else {
        None
    };

    if root.is_dir() && ripgrep_available() {
        match search_content_with_rg(
            &workspace,
            &root,
            &query,
            file_glob.as_deref(),
            max_results,
            context_lines,
            case_sensitive,
            regex,
        ) {
            Ok(output) => return Ok(output),
            Err(error) if error.starts_with("invalid_regex:") => return Err(error),
            Err(_) => {}
        }
    }

    search_content_with_fallback(
        &workspace,
        &root,
        &query,
        file_glob.as_deref(),
        max_results,
        context_lines,
        case_sensitive,
        compiled_regex.as_ref(),
    )
}

pub fn edit_file(workspace_root: &str, arguments: &Value) -> Result<Value, String> {
    let workspace = canonical_workspace_root(workspace_root)?;
    let raw_file = required_string(arguments, "file")?;
    let search = required_string(arguments, "search")?;
    let replace = required_string(arguments, "replace")?;
    if search.is_empty() {
        return Err("invalid_arguments: search must not be empty".to_string());
    }
    let file = resolve_existing_path(&workspace, &raw_file)?;
    ensure_regular_text_file(&workspace, &file)?;
    let original = fs::read_to_string(&file).map_err(|error| {
        format!(
            "read_failed: {}: {error}",
            relative_or_display(&workspace, &file)
        )
    })?;
    let count = original.matches(&search).count();
    if count == 0 {
        return Err(format!(
            "edit_not_applied: search text not found in {}",
            relative_path(&workspace, &file)
        ));
    }
    if count > 1 {
        return Err(format!("ambiguous_edit: search text matched {count} times in {}; provide a larger unique block", relative_path(&workspace, &file)));
    }
    let updated = original.replacen(&search, &replace, 1);
    fs::write(&file, updated).map_err(|error| {
        format!(
            "write_failed: {}: {error}",
            relative_or_display(&workspace, &file)
        )
    })?;
    Ok(json!({
        "file": relative_path(&workspace, &file),
        "replacements": 1,
    }))
}

pub fn write_file(workspace_root: &str, arguments: &Value) -> Result<Value, String> {
    let workspace = canonical_workspace_root(workspace_root)?;
    let raw_file = required_string(arguments, "file")?;
    let content = required_string(arguments, "content")?;
    let file = resolve_write_path(&workspace, &raw_file)?;
    let bytes = content.len();
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create_dir_failed: {}: {error}", parent.display()))?;
    }
    fs::write(&file, &content).map_err(|error| {
        format!(
            "write_failed: {}: {error}",
            relative_or_display(&workspace, &file)
        )
    })?;
    Ok(json!({
        "file": relative_path(&workspace, &file),
        "bytes": bytes,
    }))
}

pub async fn shell_command(
    workspace_root: &str,
    arguments: &Value,
    allow_shell: bool,
    assume_yes: bool,
) -> Result<Value, String> {
    if !allow_shell {
        return Err("rejected: shell_command is disabled for this run".to_string());
    }
    let workspace = canonical_workspace_root(workspace_root)?;
    let command = required_string(arguments, "command")?;
    let command = command.trim();
    if command.is_empty() {
        return Err("invalid_arguments: command must not be empty".to_string());
    }
    assess_shell_command(command, assume_yes)?;
    let timeout_ms = optional_usize(arguments, "timeout_ms", 60_000)?.min(60_000) as u64;

    let mut process = if cfg!(windows) {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(command);
        cmd
    } else {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd
    };
    hide_child_console(&mut process);
    process
        .current_dir(&workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());

    let mut child = process
        .spawn()
        .map_err(|error| format!("shell_spawn_failed: {error}"))?;
    let started = Instant::now();
    loop {
        if child
            .try_wait()
            .map_err(|error| format!("shell_wait_failed: {error}"))?
            .is_some()
        {
            break;
        }
        if started.elapsed() >= Duration::from_millis(timeout_ms) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("timeout: shell command exceeded {timeout_ms}ms"));
        }
        thread::sleep(Duration::from_millis(25));
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("shell_wait_failed: {error}"))?;
    Ok(json!({
        "command": command,
        "statusCode": output.status.code(),
        "success": output.status.success(),
        "stdout": String::from_utf8_lossy(&output.stdout),
        "stderr": String::from_utf8_lossy(&output.stderr),
    }))
}

pub fn git_status(workspace_root: &str, arguments: &Value) -> Result<Value, String> {
    let workspace = canonical_workspace_root(workspace_root)?;
    let porcelain = optional_bool(arguments, "porcelain", true)?;
    let branch = optional_bool(arguments, "branch", true)?;
    let pathspecs = optional_string_array(arguments, "pathspecs")?;
    let max_bytes = git_max_bytes(arguments)?;
    let mut args = vec!["status".to_string()];
    if porcelain {
        args.push("--short".to_string());
    }
    if branch {
        args.push("--branch".to_string());
    }
    add_pathspec_args(&mut args, &pathspecs);
    run_git_command(&workspace, args, max_bytes)
}

pub fn git_diff(workspace_root: &str, arguments: &Value) -> Result<Value, String> {
    let workspace = canonical_workspace_root(workspace_root)?;
    let cached = optional_bool(arguments, "cached", false)?;
    let stat = optional_bool(arguments, "stat", false)?;
    let name_only = optional_bool(arguments, "name_only", false)?;
    let pathspecs = optional_string_array(arguments, "pathspecs")?;
    let max_bytes = git_max_bytes(arguments)?;
    let mut args = vec!["diff".to_string(), "--no-ext-diff".to_string()];
    if cached {
        args.push("--cached".to_string());
    }
    if stat {
        args.push("--stat".to_string());
    }
    if name_only {
        args.push("--name-only".to_string());
    }
    if let Some(unified) = optional_usize_value(arguments, "unified")? {
        args.push(format!("--unified={}", unified.min(200)));
    }
    add_pathspec_args(&mut args, &pathspecs);
    run_git_command(&workspace, args, max_bytes)
}

pub fn git_log(workspace_root: &str, arguments: &Value) -> Result<Value, String> {
    let workspace = canonical_workspace_root(workspace_root)?;
    let max_count = optional_usize(arguments, "max_count", 10)?.clamp(1, 100);
    let oneline = optional_bool(arguments, "oneline", true)?;
    let max_bytes = git_max_bytes(arguments)?;
    let mut args = vec!["log".to_string(), format!("--max-count={max_count}")];
    if oneline {
        args.push("--oneline".to_string());
        args.push("--decorate".to_string());
    } else {
        args.push("--stat".to_string());
    }
    run_git_command(&workspace, args, max_bytes)
}

pub fn git_show(workspace_root: &str, arguments: &Value) -> Result<Value, String> {
    let workspace = canonical_workspace_root(workspace_root)?;
    let revision = optional_string(arguments, "revision")?
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "HEAD".to_string());
    let stat = optional_bool(arguments, "stat", true)?;
    let name_only = optional_bool(arguments, "name_only", false)?;
    let pathspecs = optional_string_array(arguments, "pathspecs")?;
    let max_bytes = git_max_bytes(arguments)?;
    let mut args = vec!["show".to_string(), "--no-ext-diff".to_string()];
    if stat {
        args.push("--stat".to_string());
    }
    if name_only {
        args.push("--name-only".to_string());
    }
    args.push(revision);
    add_pathspec_args(&mut args, &pathspecs);
    run_git_command(&workspace, args, max_bytes)
}

pub fn git_add(workspace_root: &str, arguments: &Value) -> Result<Value, String> {
    let workspace = canonical_workspace_root(workspace_root)?;
    let all = optional_bool(arguments, "all", false)?;
    let pathspecs = optional_string_array(arguments, "pathspecs")?;
    let max_bytes = git_max_bytes(arguments)?;
    let mut args = vec!["add".to_string()];
    if all {
        args.push("--all".to_string());
    } else if pathspecs.is_empty() {
        return Err("invalid_arguments: git/add requires `pathspecs` or all=true".to_string());
    }
    add_pathspec_args(&mut args, &pathspecs);
    run_git_command(&workspace, args, max_bytes)
}

pub fn git_reset(workspace_root: &str, arguments: &Value) -> Result<Value, String> {
    let workspace = canonical_workspace_root(workspace_root)?;
    let all = optional_bool(arguments, "all", false)?;
    let pathspecs = optional_string_array(arguments, "pathspecs")?;
    let max_bytes = git_max_bytes(arguments)?;
    if all && !pathspecs.is_empty() {
        return Err("invalid_arguments: use either all=true or pathspecs, not both".to_string());
    }
    if !all && pathspecs.is_empty() {
        return Err("invalid_arguments: git/reset requires `pathspecs` or all=true".to_string());
    }
    let mut args = vec!["reset".to_string()];
    add_pathspec_args(&mut args, &pathspecs);
    run_git_command(&workspace, args, max_bytes)
}

pub fn git_commit(workspace_root: &str, arguments: &Value) -> Result<Value, String> {
    let workspace = canonical_workspace_root(workspace_root)?;
    let message = required_string(arguments, "message")?;
    let message = message.trim();
    if message.is_empty() {
        return Err("invalid_arguments: message must not be empty".to_string());
    }
    let allow_empty = optional_bool(arguments, "allow_empty", false)?;
    let max_bytes = git_max_bytes(arguments)?;
    let mut args = vec!["commit".to_string()];
    if allow_empty {
        args.push("--allow-empty".to_string());
    }
    args.push("-m".to_string());
    args.push(message.to_string());
    for paragraph in optional_body_paragraphs(arguments)? {
        args.push("-m".to_string());
        args.push(paragraph);
    }
    let mut result = run_git_command(&workspace, args, max_bytes)?;
    if result.get("success").and_then(Value::as_bool) == Some(true) {
        if let Ok(hash) = git_head_short_hash(&workspace) {
            result["commit"] = json!(hash);
        }
    }
    Ok(result)
}

pub fn git_staged_paths(workspace_root: &str) -> Result<Vec<String>, String> {
    let workspace = canonical_workspace_root(workspace_root)?;
    let args = vec![
        "diff".to_string(),
        "--cached".to_string(),
        "--name-only".to_string(),
        "-z".to_string(),
    ];
    let output = run_git_process(&workspace, &args, GIT_COMMAND_TIMEOUT_MS)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("git_failed: {}", stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(|path| normalize_display_path(path).replace('\\', "/"))
        .collect())
}

fn run_git_command(workspace: &Path, args: Vec<String>, max_bytes: usize) -> Result<Value, String> {
    let output = run_git_process(workspace, &args, GIT_COMMAND_TIMEOUT_MS)?;
    let (stdout, stdout_truncated) = truncate_output(&output.stdout, max_bytes);
    let (stderr, stderr_truncated) = truncate_output(&output.stderr, max_bytes);
    Ok(json!({
        "command": command_vector(&args),
        "displayCommand": display_git_command(&args),
        "statusCode": output.status.code(),
        "success": output.status.success(),
        "stdout": stdout,
        "stderr": stderr,
        "stdoutTruncated": stdout_truncated,
        "stderrTruncated": stderr_truncated,
        "maxBytes": max_bytes,
    }))
}

fn run_git_process(
    workspace: &Path,
    args: &[String],
    timeout_ms: u64,
) -> Result<std::process::Output, String> {
    let mut process = Command::new("git");
    hide_child_console(&mut process);
    process
        .args(args)
        .current_dir(workspace)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());

    let mut child = process
        .spawn()
        .map_err(|error| format!("git_spawn_failed: {error}"))?;
    let started = Instant::now();
    loop {
        if child
            .try_wait()
            .map_err(|error| format!("git_wait_failed: {error}"))?
            .is_some()
        {
            break;
        }
        if started.elapsed() >= Duration::from_millis(timeout_ms) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("timeout: git command exceeded {timeout_ms}ms"));
        }
        thread::sleep(Duration::from_millis(25));
    }
    child
        .wait_with_output()
        .map_err(|error| format!("git_wait_failed: {error}"))
}

fn git_head_short_hash(workspace: &Path) -> Result<String, String> {
    let args = vec![
        "rev-parse".to_string(),
        "--short".to_string(),
        "HEAD".to_string(),
    ];
    let output = run_git_process(workspace, &args, GIT_COMMAND_TIMEOUT_MS)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("git_failed: {}", stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn command_vector(args: &[String]) -> Vec<String> {
    std::iter::once("git".to_string())
        .chain(args.iter().cloned())
        .collect()
}

fn display_git_command(args: &[String]) -> String {
    command_vector(args)
        .iter()
        .map(|arg| {
            if arg.chars().any(char::is_whitespace) {
                format!("\"{}\"", arg.replace('"', "\\\""))
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate_output(bytes: &[u8], max_bytes: usize) -> (String, bool) {
    if bytes.len() <= max_bytes {
        return (String::from_utf8_lossy(bytes).to_string(), false);
    }
    let mut text = String::from_utf8_lossy(&bytes[..max_bytes]).to_string();
    text.push_str(&format!("\n<truncated: output exceeded {max_bytes} bytes>"));
    (text, true)
}

fn add_pathspec_args(args: &mut Vec<String>, pathspecs: &[String]) {
    if pathspecs.is_empty() {
        return;
    }
    args.push("--".to_string());
    args.extend(pathspecs.iter().cloned());
}

fn git_max_bytes(arguments: &Value) -> Result<usize, String> {
    let max_bytes = optional_usize(arguments, "max_bytes", DEFAULT_GIT_OUTPUT_MAX_BYTES)?;
    if max_bytes == 0 {
        return Err("invalid_arguments: max_bytes must be >= 1".to_string());
    }
    Ok(max_bytes.min(MAX_GIT_OUTPUT_MAX_BYTES))
}

fn optional_string_array(arguments: &Value, key: &str) -> Result<Vec<String>, String> {
    match arguments.get(key) {
        Some(Value::Null) | None => Ok(Vec::new()),
        Some(Value::String(value)) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                Ok(Vec::new())
            } else {
                Ok(vec![trimmed.to_string()])
            }
        }
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| format!("invalid_arguments: `{key}` entries must be strings"))
            })
            .collect(),
        _ => Err(format!(
            "invalid_arguments: `{key}` must be a string or array of strings"
        )),
    }
}

fn optional_body_paragraphs(arguments: &Value) -> Result<Vec<String>, String> {
    let mut paragraphs = Vec::new();
    if let Some(body) = optional_string(arguments, "body")? {
        let body = body.trim();
        if !body.is_empty() {
            paragraphs.push(body.to_string());
        }
    }
    match arguments.get("body_paragraphs") {
        Some(Value::Null) | None => {}
        Some(Value::Array(items)) => {
            for item in items {
                let paragraph = item
                    .as_str()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        "invalid_arguments: `body_paragraphs` entries must be strings".to_string()
                    })?;
                paragraphs.push(paragraph.to_string());
            }
        }
        _ => {
            return Err(
                "invalid_arguments: `body_paragraphs` must be an array of strings".to_string(),
            )
        }
    }
    Ok(paragraphs)
}

fn resolve_write_path(workspace: &Path, raw_path: &str) -> Result<PathBuf, String> {
    let normalized = normalize_display_path(raw_path);
    let trimmed = normalized.trim();
    if trimmed.is_empty() || trimmed == "." {
        return Err("invalid_arguments: file must not be empty".to_string());
    }
    let path = PathBuf::from(trimmed);
    if path.is_absolute() {
        return Err(format!(
            "path_outside_workspace: {}",
            display_input_path(raw_path)
        ));
    }
    let candidate = workspace.join(path);
    let parent = candidate.parent().unwrap_or(workspace);
    let canonical_parent = if parent.exists() {
        canonicalize_path(parent).map_err(|_| format!("path_not_found: {}", parent.display()))?
    } else {
        let existing = nearest_existing_parent(parent)?;
        canonicalize_path(&existing)
            .map_err(|_| format!("path_not_found: {}", existing.display()))?
    };
    ensure_inside_workspace(workspace, &canonical_parent, raw_path)?;
    Ok(candidate)
}

fn nearest_existing_parent(path: &Path) -> Result<PathBuf, String> {
    let mut current = path;
    loop {
        if current.exists() {
            return Ok(current.to_path_buf());
        }
        current = current
            .parent()
            .ok_or_else(|| format!("path_not_found: {}", path.display()))?;
    }
}

fn assess_shell_command(command: &str, assume_yes: bool) -> Result<(), String> {
    let lower = command.to_ascii_lowercase();
    let compact = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    let high_risk = [
        "rm -rf",
        "del /",
        "format",
        "shutdown",
        "invoke-webrequest",
        "| iex",
        "curl",
        "| sh",
    ];
    if compact.contains("rm -rf")
        || compact.contains("del /s")
        || compact.contains("del /q")
        || compact.contains("format")
        || compact.contains("shutdown")
        || (compact.contains("invoke-webrequest") && compact.contains("| iex"))
        || (compact.contains("curl") && compact.contains("| sh"))
    {
        return Err("rejected: high-risk shell command is blocked".to_string());
    }
    let install_like = ["npm install", "pip install", "cargo install"];
    if install_like.iter().any(|pattern| compact.contains(pattern)) && !assume_yes {
        return Err("rejected: install commands require --yes confirmation".to_string());
    }
    let _ = high_risk;
    Ok(())
}

pub fn get_file_context(workspace_root: &str, arguments: &Value) -> Result<Value, String> {
    let workspace = canonical_workspace_root(workspace_root)?;
    let raw_path = required_string(arguments, "path")?;
    let line = required_usize(arguments, "line")?;
    if line == 0 {
        return Err("invalid_range: line must be >= 1".to_string());
    }

    let file = resolve_existing_read_path(&workspace, &raw_path)?;
    ensure_regular_text_file(&workspace, &file)?;
    let before =
        optional_usize(arguments, "before", DEFAULT_FILE_CONTEXT_BEFORE)?.min(MAX_CONTEXT_LINES);
    let after =
        optional_usize(arguments, "after", DEFAULT_FILE_CONTEXT_AFTER)?.min(MAX_CONTEXT_LINES);
    let start = line.saturating_sub(before).max(1);
    let end = line.saturating_add(after);
    let (lines, _) = collect_line_range(&file, start, end, false)?;
    if !lines
        .iter()
        .any(|entry| entry.get("line").and_then(Value::as_u64) == Some(line as u64))
    {
        return Err(format!(
            "line_out_of_range: {}:{}",
            relative_path(&workspace, &file),
            line
        ));
    }

    let actual_start_line = lines
        .first()
        .and_then(|entry| entry.get("line"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let actual_end_line = lines
        .last()
        .and_then(|entry| entry.get("line"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    Ok(json!({
        "file": relative_path(&workspace, &file),
        "line": line,
        "before": before,
        "after": after,
        "startLine": actual_start_line,
        "endLine": actual_end_line,
        "lines": lines,
    }))
}

fn search_content_with_rg(
    workspace: &Path,
    root: &Path,
    query: &str,
    file_glob: Option<&str>,
    max_results: usize,
    context_lines: usize,
    case_sensitive: bool,
    regex: bool,
) -> Result<Value, String> {
    let mut command = Command::new("rg");
    hide_child_console(&mut command);
    command
        .current_dir(root)
        .arg("--json")
        .arg("--line-number")
        .arg("--column")
        .arg("--hidden")
        .arg("--follow")
        .arg("--color")
        .arg("never");
    if !case_sensitive {
        command.arg("--ignore-case");
    }
    if !regex {
        command.arg("--fixed-strings");
    }
    add_ignore_globs(&mut command);
    if let Some(glob) = file_glob.filter(|glob| !glob.trim().is_empty()) {
        command.arg("--glob").arg(glob);
    } else {
        add_default_content_globs(&mut command);
    }
    command.arg(query).arg(".");

    let output = command
        .output()
        .map_err(|error| format!("search_failed: failed to run rg: {error}"))?;
    if !output.status.success() && output.status.code() != Some(1) {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if regex {
            return Err(format!("invalid_regex: {stderr}"));
        }
        return Err(format!("search_failed: {stderr}"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut matches = Vec::new();
    let mut truncated = false;
    for line in stdout.lines() {
        let parsed = match serde_json::from_str::<Value>(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if parsed.get("type").and_then(Value::as_str) != Some("match") {
            continue;
        }
        if matches.len() >= max_results {
            truncated = true;
            break;
        }
        let Some(data) = parsed.get("data") else {
            continue;
        };
        let Some(path_text) = data
            .get("path")
            .and_then(|path| path.get("text"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let path = resolve_rg_path(workspace, root, path_text)?;
        let line_number = data.get("line_number").and_then(Value::as_u64).unwrap_or(0) as usize;
        if line_number == 0 {
            continue;
        }
        let columns = data
            .get("submatches")
            .and_then(Value::as_array)
            .map(|submatches| {
                submatches
                    .iter()
                    .filter_map(|submatch| {
                        submatch
                            .get("start")
                            .and_then(Value::as_u64)
                            .map(|column| column + 1)
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|columns| !columns.is_empty())
            .unwrap_or_else(|| vec![1]);
        let text = data
            .get("lines")
            .and_then(|lines| lines.get("text"))
            .and_then(Value::as_str)
            .map(trim_line_end)
            .unwrap_or_default();
        let (before, after) = read_before_after(&path, line_number, context_lines, context_lines)?;
        matches.push(json!({
            "file": relative_path(workspace, &path),
            "line": line_number,
            "column": columns[0],
            "columns": columns,
            "text": text,
            "before": before,
            "after": after,
        }));
    }

    Ok(json!({
        "query": query,
        "root": relative_path(workspace, root),
        "fileGlob": file_glob,
        "maxResults": max_results,
        "contextLines": context_lines,
        "caseSensitive": case_sensitive,
        "regex": regex,
        "engine": "rg",
        "matches": matches,
        "count": matches.len(),
        "truncated": truncated,
        "message": if truncated {
            Some(format!("too_many_results: returned first {max_results} matches"))
        } else {
            None
        },
    }))
}

fn search_content_with_fallback(
    workspace: &Path,
    root: &Path,
    query: &str,
    file_glob: Option<&str>,
    max_results: usize,
    context_lines: usize,
    case_sensitive: bool,
    regex: Option<&Regex>,
) -> Result<Value, String> {
    let mut matches = Vec::new();
    let mut truncated = false;
    let mut scan_limited = false;
    let mut skipped_files = 0usize;
    let started = Instant::now();
    let normalized_query = if case_sensitive {
        query.to_string()
    } else {
        query.to_ascii_lowercase()
    };

    let scanned_files = walk_files_until(workspace, root, &mut |path, scanned| {
        if matches.len() >= max_results {
            truncated = true;
            return Ok(WalkControl::Stop);
        }
        if scanned > SEARCH_CONTENT_SCAN_LIMIT
            || started.elapsed().as_millis() >= SEARCH_SCAN_TIMEOUT_MS
        {
            scan_limited = true;
            return Ok(WalkControl::Stop);
        }
        if !content_file_allowed(workspace, path, file_glob) {
            return Ok(WalkControl::Continue);
        }
        match is_binary_file(path) {
            Ok(true) => return Ok(WalkControl::Continue),
            Ok(false) => {}
            Err(_) => {
                skipped_files += 1;
                return Ok(WalkControl::Continue);
            }
        }

        let mut line_number = 0usize;
        let file = match File::open(path) {
            Ok(file) => file,
            Err(_) => {
                skipped_files += 1;
                return Ok(WalkControl::Continue);
            }
        };
        let mut reader = BufReader::new(file);
        let mut bytes = Vec::new();
        loop {
            let read = match reader.read_until(b'\n', &mut bytes) {
                Ok(read) => read,
                Err(_) => {
                    skipped_files += 1;
                    return Ok(WalkControl::Continue);
                }
            };
            if read == 0 {
                break;
            }
            line_number += 1;
            let line = bytes_to_line(&bytes);
            let columns = find_columns(&line, &normalized_query, case_sensitive, regex);
            if !columns.is_empty() {
                if matches.len() >= max_results {
                    truncated = true;
                    return Ok(WalkControl::Stop);
                }
                let (before, after) =
                    match read_before_after(path, line_number, context_lines, context_lines) {
                        Ok(context) => context,
                        Err(_) => {
                            skipped_files += 1;
                            return Ok(WalkControl::Continue);
                        }
                    };
                matches.push(json!({
                    "file": relative_path(workspace, path),
                    "line": line_number,
                    "column": columns[0],
                    "columns": columns,
                    "text": line,
                    "before": before,
                    "after": after,
                }));
            }
            bytes.clear();
        }
        Ok(WalkControl::Continue)
    })?;

    Ok(json!({
        "query": query,
        "root": relative_path(workspace, root),
        "fileGlob": file_glob,
        "maxResults": max_results,
        "contextLines": context_lines,
        "caseSensitive": case_sensitive,
        "regex": regex.is_some(),
        "engine": "fallback",
        "matches": matches,
        "count": matches.len(),
        "scannedFiles": scanned_files,
        "skippedFiles": skipped_files,
        "complete": !truncated && !scan_limited,
        "truncated": truncated || scan_limited,
        "message": if scan_limited {
            Some(format!(
                "search_limited: scanned {scanned_files} files before returning partial results; narrow root or file_glob"
            ))
        } else if truncated {
            Some(format!("too_many_results: returned first {max_results} matches"))
        } else {
            None
        },
    }))
}

fn canonical_workspace_root(workspace_root: &str) -> Result<PathBuf, String> {
    let raw = normalize_display_path(workspace_root);
    let path = PathBuf::from(raw.trim());
    let canonical = canonicalize_path(&path)
        .map_err(|error| format!("workspace_not_found: {}: {error}", path.display()))?;
    if !canonical.is_dir() {
        return Err(format!("workspace_not_found: {}", canonical.display()));
    }
    Ok(canonical)
}

fn canonicalize_path(path: &Path) -> Result<PathBuf, std::io::Error> {
    fs::canonicalize(path)
}

fn resolve_existing_path(workspace: &Path, raw_path: &str) -> Result<PathBuf, String> {
    let normalized = normalize_display_path(raw_path);
    let trimmed = normalized.trim();
    let candidate = if trimmed.is_empty() || trimmed == "." {
        workspace.to_path_buf()
    } else {
        let path = PathBuf::from(trimmed);
        if path.is_absolute() {
            path
        } else {
            workspace.join(path)
        }
    };
    let canonical = canonicalize_path(&candidate)
        .map_err(|_| format!("file_not_found: {}", display_input_path(raw_path)))?;
    ensure_inside_workspace(workspace, &canonical, raw_path)?;
    Ok(canonical)
}

fn resolve_existing_read_path(base: &Path, raw_path: &str) -> Result<PathBuf, String> {
    let normalized = normalize_display_path(raw_path);
    let trimmed = normalized.trim();
    let candidate = if trimmed.is_empty() || trimmed == "." {
        base.to_path_buf()
    } else {
        let path = PathBuf::from(trimmed);
        if path.is_absolute() {
            path
        } else {
            base.join(path)
        }
    };
    canonicalize_path(&candidate)
        .map_err(|_| format!("file_not_found: {}", display_input_path(raw_path)))
}

struct ResolvedReadFile {
    file: PathBuf,
    recovery: Option<ReadFileRecovery>,
}

struct ReadFileRecovery {
    requested_path: String,
    search_root: PathBuf,
    candidate_count: usize,
    candidates: Vec<PathBuf>,
}

impl ReadFileRecovery {
    fn to_json(&self, workspace: &Path, resolved_file: &Path) -> Value {
        json!({
            "reason": "file_not_found",
            "requestedPath": self.requested_path,
            "resolvedPath": relative_or_display(workspace, resolved_file),
            "searchRoot": relative_or_display(workspace, &self.search_root),
            "candidateCount": self.candidate_count,
            "candidates": self
                .candidates
                .iter()
                .map(|candidate| relative_or_display(workspace, candidate))
                .collect::<Vec<_>>(),
        })
    }
}

fn recover_missing_read_file(
    workspace: &Path,
    raw_path: &str,
) -> Result<Option<ResolvedReadFile>, String> {
    let Some(file_name) = requested_file_name(raw_path) else {
        return Ok(None);
    };
    let requested_path = display_input_path(raw_path);
    for root in recovery_search_roots(workspace, raw_path) {
        let search = find_same_named_files(workspace, &root, &file_name)?;
        match search.matches.len() {
            0 => {
                if search.scan_limited {
                    return Err(format!(
                        "file_not_found: {requested_path}; recovery_search_limited: scanned {} files under {} while looking for {file_name}",
                        search.scanned_files,
                        relative_or_display(workspace, &root)
                    ));
                }
            }
            1 => {
                let file = search.matches[0].clone();
                return Ok(Some(ResolvedReadFile {
                    file,
                    recovery: Some(ReadFileRecovery {
                        requested_path,
                        search_root: root,
                        candidate_count: search.matches.len(),
                        candidates: search.matches,
                    }),
                }));
            }
            _ => {
                let candidates = search
                    .matches
                    .iter()
                    .map(|candidate| relative_or_display(workspace, candidate))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(format!(
                    "file_not_found: {requested_path}; recovery_ambiguous: found {} candidates under {}: {candidates}",
                    search.matches.len(),
                    relative_or_display(workspace, &root)
                ));
            }
        }
    }
    Ok(None)
}

fn requested_file_name(raw_path: &str) -> Option<String> {
    let normalized = normalize_display_path(raw_path);
    let trimmed = normalized
        .trim()
        .trim_end_matches(|ch| ch == '/' || ch == '\\');
    if trimmed.is_empty() {
        return None;
    }
    Path::new(trimmed)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn unresolved_read_candidate(workspace: &Path, raw_path: &str) -> PathBuf {
    let normalized = normalize_display_path(raw_path);
    let trimmed = normalized.trim();
    if trimmed.is_empty() || trimmed == "." {
        return workspace.to_path_buf();
    }
    let path = PathBuf::from(trimmed);
    if path.is_absolute() {
        path
    } else {
        workspace.join(path)
    }
}

fn recovery_search_roots(workspace: &Path, raw_path: &str) -> Vec<PathBuf> {
    let normalized = normalize_display_path(raw_path);
    let raw_is_absolute = PathBuf::from(normalized.trim()).is_absolute();
    let candidate = unresolved_read_candidate(workspace, raw_path);
    let mut roots = Vec::new();
    let mut current = candidate.parent();

    while let Some(parent) = current {
        if let Ok(canonical) = canonicalize_path(parent) {
            if canonical.is_dir()
                && !is_filesystem_root(&canonical)
                && !roots.iter().any(|root| root == &canonical)
            {
                roots.push(canonical.clone());
            }
            if !raw_is_absolute && canonical == workspace {
                break;
            }
            if roots.len() >= READ_FILE_RECOVERY_MAX_ROOTS {
                break;
            }
        }
        current = parent.parent();
    }

    roots
}

struct SameNameSearch {
    matches: Vec<PathBuf>,
    scanned_files: usize,
    scan_limited: bool,
}

fn find_same_named_files(
    workspace: &Path,
    root: &Path,
    file_name: &str,
) -> Result<SameNameSearch, String> {
    let mut matches = Vec::new();
    let mut scan_limited = false;
    let scanned_files = walk_files_until(workspace, root, &mut |path, scanned| {
        if scanned > READ_FILE_RECOVERY_SCAN_LIMIT {
            scan_limited = true;
            return Ok(WalkControl::Stop);
        }
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(file_name))
        {
            matches.push(path.to_path_buf());
            if matches.len() > READ_FILE_RECOVERY_MAX_CANDIDATES {
                return Ok(WalkControl::Stop);
            }
        }
        Ok(WalkControl::Continue)
    })?;
    Ok(SameNameSearch {
        matches,
        scanned_files,
        scan_limited,
    })
}

fn is_filesystem_root(path: &Path) -> bool {
    path.parent().is_none() || path.parent().is_some_and(|parent| parent == path)
}

fn resolve_search_root(workspace: &Path, arguments: &Value) -> Result<PathBuf, String> {
    let root = optional_string(arguments, "root")?.unwrap_or_else(|| ".".to_string());
    let path = resolve_existing_read_path(workspace, &root)?;
    if !path.is_dir() {
        return Err(format!(
            "not_directory: {}",
            relative_or_display(workspace, &path)
        ));
    }
    Ok(path)
}

fn resolve_search_path(workspace: &Path, arguments: &Value) -> Result<PathBuf, String> {
    let root = optional_string(arguments, "root")?.unwrap_or_else(|| ".".to_string());
    let path = resolve_existing_read_path(workspace, &root)?;
    if path.is_dir() || path.is_file() {
        return Ok(path);
    }
    Err(format!(
        "not_file_or_directory: {}",
        relative_or_display(workspace, &path)
    ))
}

fn resolve_rg_path(_workspace: &Path, root: &Path, path_text: &str) -> Result<PathBuf, String> {
    resolve_existing_read_path(root, path_text)
}

fn ensure_inside_workspace(workspace: &Path, path: &Path, raw_path: &str) -> Result<(), String> {
    if path == workspace || path.starts_with(workspace) {
        return Ok(());
    }
    Err(format!(
        "path_outside_workspace: {}",
        display_input_path(raw_path)
    ))
}

fn ensure_regular_text_file(workspace: &Path, file: &Path) -> Result<(), String> {
    if !file.is_file() {
        return Err(format!(
            "not_file: {}",
            relative_or_display(workspace, file)
        ));
    }
    if is_binary_file(file)? {
        return Err(format!(
            "binary_file: {}",
            relative_or_display(workspace, file)
        ));
    }
    Ok(())
}

fn is_binary_file(path: &Path) -> Result<bool, String> {
    let mut file =
        File::open(path).map_err(|error| format!("file_not_found: {}: {error}", path.display()))?;
    let mut buffer = [0u8; BINARY_SAMPLE_BYTES];
    let read = file
        .read(&mut buffer)
        .map_err(|error| format!("read_failed: {}: {error}", path.display()))?;
    Ok(buffer[..read].contains(&0))
}

fn sorted_read_dir(path: &Path) -> Result<Vec<fs::DirEntry>, String> {
    let mut entries = fs::read_dir(path)
        .map_err(|error| format!("read_dir_failed: {}: {error}", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("read_dir_failed: {}: {error}", path.display()))?;
    entries.sort_by_key(|entry| entry.file_name().to_string_lossy().to_ascii_lowercase());
    Ok(entries)
}

enum WalkControl {
    Continue,
    Stop,
}

fn walk_files_until(
    _workspace: &Path,
    root: &Path,
    visit: &mut impl FnMut(&Path, usize) -> Result<WalkControl, String>,
) -> Result<usize, String> {
    if root.is_file() {
        if matches!(visit(root, 1)?, WalkControl::Stop) {
            return Ok(1);
        }
        return Ok(1);
    }

    let mut stack = vec![root.to_path_buf()];
    let mut scanned = 0usize;
    while let Some(dir) = stack.pop() {
        let entries = match sorted_read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if dir == root => return Err(error),
            Err(_) => continue,
        };
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                if is_ignored_dir(&path) {
                    continue;
                }
                let Ok(canonical) = canonicalize_path(&path) else {
                    continue;
                };
                stack.push(canonical);
            } else if path.is_file() {
                let Ok(canonical) = canonicalize_path(&path) else {
                    continue;
                };
                scanned += 1;
                if matches!(visit(&canonical, scanned)?, WalkControl::Stop) {
                    return Ok(scanned);
                }
            }
        }
    }
    Ok(scanned)
}

fn is_ignored_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            IGNORED_DIRS
                .iter()
                .any(|ignored| name.eq_ignore_ascii_case(ignored))
        })
}

fn ignored_search_excludes() -> Vec<String> {
    let mut excludes = IGNORED_DIRS
        .iter()
        .map(|ignored| format!("**/{ignored}/**"))
        .collect::<Vec<_>>();
    excludes.extend(
        IGNORED_SEARCH_PATHS
            .iter()
            .map(|ignored_path| format!("**/{}/**", ignored_path.join("/"))),
    );
    excludes
}

fn collect_line_range(
    file: &Path,
    start_line: usize,
    end_line: usize,
    count_total: bool,
) -> Result<(Vec<Value>, usize), String> {
    let opened =
        File::open(file).map_err(|error| format!("file_not_found: {}: {error}", file.display()))?;
    let mut reader = BufReader::new(opened);
    let mut line_number = 0usize;
    let mut lines = Vec::new();
    let mut bytes = Vec::new();
    while reader
        .read_until(b'\n', &mut bytes)
        .map_err(|error| format!("read_failed: {}: {error}", file.display()))?
        > 0
    {
        line_number += 1;
        if line_number >= start_line && line_number <= end_line {
            lines.push(json!({
                "line": line_number,
                "text": bytes_to_line(&bytes),
            }));
        }
        bytes.clear();
        if !count_total && line_number >= end_line {
            break;
        }
    }
    Ok((lines, line_number))
}

fn read_before_after(
    file: &Path,
    line: usize,
    before: usize,
    after: usize,
) -> Result<(Vec<Value>, Vec<Value>), String> {
    let start = line.saturating_sub(before).max(1);
    let end = line.saturating_add(after);
    let (lines, _) = collect_line_range(file, start, end, false)?;
    let before_lines = lines
        .iter()
        .filter(|entry| entry.get("line").and_then(Value::as_u64).unwrap_or(0) < line as u64)
        .cloned()
        .collect::<Vec<_>>();
    let after_lines = lines
        .iter()
        .filter(|entry| entry.get("line").and_then(Value::as_u64).unwrap_or(0) > line as u64)
        .cloned()
        .collect::<Vec<_>>();
    Ok((before_lines, after_lines))
}

fn bytes_to_line(bytes: &[u8]) -> String {
    let mut end = bytes.len();
    if end > 0 && bytes[end - 1] == b'\n' {
        end -= 1;
    }
    if end > 0 && bytes[end - 1] == b'\r' {
        end -= 1;
    }
    String::from_utf8_lossy(&bytes[..end]).to_string()
}

fn trim_line_end(text: &str) -> String {
    text.trim_end_matches(&['\r', '\n'][..]).to_string()
}

fn find_columns(
    line: &str,
    normalized_query: &str,
    case_sensitive: bool,
    regex: Option<&Regex>,
) -> Vec<usize> {
    if let Some(regex) = regex {
        return regex
            .find_iter(line)
            .map(|matched| matched.start() + 1)
            .collect();
    }

    let haystack = if case_sensitive {
        line.to_string()
    } else {
        line.to_ascii_lowercase()
    };
    let mut columns = Vec::new();
    let mut offset = 0usize;
    while let Some(found) = haystack[offset..].find(normalized_query) {
        let column = offset + found + 1;
        columns.push(column);
        offset += found + normalized_query.len().max(1);
        if offset >= haystack.len() {
            break;
        }
    }
    columns
}

fn content_file_allowed(workspace: &Path, path: &Path, file_glob: Option<&str>) -> bool {
    let rel = relative_path(workspace, path);
    if let Some(glob) = file_glob.filter(|glob| !glob.trim().is_empty()) {
        return matches_file_glob(&rel, glob);
    }
    preferred_content_file(path)
}

fn preferred_content_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| format!(".{}", extension).to_ascii_lowercase())
        .is_some_and(|extension| DEFAULT_CONTENT_EXTENSIONS.contains(&extension.as_str()))
}

fn matches_file_glob(relative_path: &str, glob: &str) -> bool {
    let path = relative_path.replace('\\', "/").to_ascii_lowercase();
    let pattern = glob.replace('\\', "/").to_ascii_lowercase();
    let file_name = Path::new(relative_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(relative_path)
        .to_ascii_lowercase();
    wildcard_match(&path, &pattern) || wildcard_match(&file_name, &pattern)
}

fn wildcard_pattern(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?')
}

fn wildcard_match(text: &str, pattern: &str) -> bool {
    let text = text.chars().collect::<Vec<_>>();
    let pattern = pattern.chars().collect::<Vec<_>>();
    let (mut text_i, mut pattern_i) = (0usize, 0usize);
    let mut star_i = None;
    let mut star_text_i = 0usize;
    while text_i < text.len() {
        if pattern_i < pattern.len()
            && (pattern[pattern_i] == '?' || pattern[pattern_i] == text[text_i])
        {
            text_i += 1;
            pattern_i += 1;
        } else if pattern_i < pattern.len() && pattern[pattern_i] == '*' {
            star_i = Some(pattern_i);
            star_text_i = text_i;
            pattern_i += 1;
        } else if let Some(star) = star_i {
            pattern_i = star + 1;
            star_text_i += 1;
            text_i = star_text_i;
        } else {
            return false;
        }
    }
    while pattern_i < pattern.len() && pattern[pattern_i] == '*' {
        pattern_i += 1;
    }
    pattern_i == pattern.len()
}

fn compile_regex(query: &str, case_sensitive: bool) -> Result<Regex, String> {
    RegexBuilder::new(query)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|error| format!("invalid_regex: {error}"))
}

fn ripgrep_available() -> bool {
    let mut command = Command::new("rg");
    hide_child_console(&mut command);
    command
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
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

fn add_ignore_globs(command: &mut Command) {
    for ignored in IGNORED_DIRS {
        command.arg("--glob").arg(format!("!**/{ignored}/**"));
    }
    for ignored_path in IGNORED_SEARCH_PATHS {
        command
            .arg("--glob")
            .arg(format!("!**/{}/**", ignored_path.join("/")));
    }
}

fn add_default_content_globs(command: &mut Command) {
    for extension in DEFAULT_CONTENT_EXTENSIONS {
        command.arg("--glob").arg(format!("**/*{extension}"));
    }
}

fn relative_path(workspace: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(workspace).unwrap_or(path);
    let text = normalize_display_path(&relative.to_string_lossy()).replace('\\', "/");
    if text.is_empty() {
        ".".to_string()
    } else {
        text
    }
}

fn relative_or_display(workspace: &Path, path: &Path) -> String {
    if path.starts_with(workspace) {
        relative_path(workspace, path)
    } else {
        normalize_display_path(&path.to_string_lossy())
    }
}

fn display_input_path(path: &str) -> String {
    normalize_display_path(path).replace('\\', "/")
}

fn required_string(arguments: &Value, key: &str) -> Result<String, String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("invalid_arguments: missing string field `{key}`"))
}

fn optional_string(arguments: &Value, key: &str) -> Result<Option<String>, String> {
    match arguments.get(key) {
        Some(Value::Null) | None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        _ => Err(format!("invalid_arguments: `{key}` must be a string")),
    }
}

fn required_usize(arguments: &Value, key: &str) -> Result<usize, String> {
    arguments
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| format!("invalid_arguments: missing numeric field `{key}`"))
}

fn optional_usize(arguments: &Value, key: &str, default: usize) -> Result<usize, String> {
    optional_usize_value(arguments, key).map(|value| value.unwrap_or(default))
}

fn optional_usize_value(arguments: &Value, key: &str) -> Result<Option<usize>, String> {
    match arguments.get(key) {
        Some(Value::Null) | None => Ok(None),
        Some(Value::Number(number)) => number
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| format!("invalid_arguments: `{key}` must be a non-negative integer")),
        _ => Err(format!("invalid_arguments: `{key}` must be a number")),
    }
}

fn optional_bool(arguments: &Value, key: &str, default: bool) -> Result<bool, String> {
    match arguments.get(key) {
        Some(Value::Null) | None => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        _ => Err(format!("invalid_arguments: `{key}` must be a boolean")),
    }
}

fn max_results(arguments: &Value) -> Result<usize, String> {
    let requested = optional_usize(arguments, "max_results", DEFAULT_MAX_RESULTS)?;
    if requested == 0 {
        return Err("invalid_arguments: max_results must be >= 1".to_string());
    }
    Ok(requested.min(MAX_RESULTS_LIMIT))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestWorkspace {
        root: PathBuf,
    }

    impl TestWorkspace {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!("snowagent-workspace-tools-{unique}"));
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.root
                .join(relative.replace('/', std::path::MAIN_SEPARATOR_STR))
        }

        fn write_text(&self, relative: &str, text: &str) {
            let path = self.path(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, text).unwrap();
        }

        fn write_bytes(&self, relative: &str, bytes: &[u8]) {
            let path = self.path(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, bytes).unwrap();
        }

        fn root_str(&self) -> String {
            self.root.to_string_lossy().to_string()
        }
    }

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn list_dir_sorts_and_ignores_default_dirs() {
        let workspace = TestWorkspace::new();
        fs::create_dir_all(workspace.path("src")).unwrap();
        fs::create_dir_all(workspace.path(".git")).unwrap();
        workspace.write_text("b.txt", "b");
        workspace.write_text("a.txt", "a");

        let result = list_dir(&workspace.root_str(), &json!({ "path": "." })).unwrap();

        assert_eq!(result["directories"], json!(["src"]));
        assert_eq!(result["files"], json!(["a.txt", "b.txt"]));
        assert_eq!(result["entries"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn read_file_returns_numbered_range_and_large_file_hint() {
        let workspace = TestWorkspace::new();
        let text = (1..=350)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        workspace.write_text("large.cpp", &text);

        let result = read_file(&workspace.root_str(), &json!({ "path": "large.cpp" })).unwrap();

        assert_eq!(result["totalLines"], json!(350));
        assert_eq!(result["startLine"], json!(1));
        assert_eq!(result["endLine"], json!(300));
        assert_eq!(result["truncated"], json!(true));
        assert!(result["text"]
            .as_str()
            .unwrap()
            .starts_with("line 1\nline 2"));
        assert_eq!(result["lines"].as_array().unwrap().len(), 300);
        assert!(result["message"]
            .as_str()
            .unwrap()
            .contains("too_many_results"));
    }

    #[test]
    fn read_file_rejects_binary_files() {
        let workspace = TestWorkspace::new();
        workspace.write_bytes("data.bin", &[0, 1, 2, 3]);

        let error = read_file(&workspace.root_str(), &json!({ "path": "data.bin" })).unwrap_err();

        assert!(error.contains("binary_file"));
    }

    #[test]
    fn read_file_recovers_unique_same_name_from_nearby_ancestor() {
        let workspace = TestWorkspace::new();
        fs::create_dir_all(workspace.path("Plugin/Source/WrongModule/Public")).unwrap();
        workspace.write_text(
            "Plugin/Source/CorrectModule/Public/ModelContextProtocol.h",
            "constexpr int Port = 8000;\n",
        );

        let result = read_file(
            &workspace.root_str(),
            &json!({
                "path": "Plugin/Source/WrongModule/Public/ModelContextProtocol.h"
            }),
        )
        .unwrap();

        assert_eq!(
            result["file"],
            json!("Plugin/Source/CorrectModule/Public/ModelContextProtocol.h")
        );
        assert_eq!(result["recovered"], json!(true));
        assert_eq!(result["recovery"]["reason"], json!("file_not_found"));
        assert_eq!(
            result["recovery"]["requestedPath"],
            json!("Plugin/Source/WrongModule/Public/ModelContextProtocol.h")
        );
        assert_eq!(result["recovery"]["searchRoot"], json!("Plugin/Source"));
        assert_eq!(result["text"], json!("constexpr int Port = 8000;"));
    }

    #[test]
    fn read_file_reports_ambiguous_recovery_candidates() {
        let workspace = TestWorkspace::new();
        fs::create_dir_all(workspace.path("Plugin/Source/WrongModule/Public")).unwrap();
        workspace.write_text("Plugin/Source/First/Public/Target.h", "first\n");
        workspace.write_text("Plugin/Source/Second/Public/Target.h", "second\n");

        let error = read_file(
            &workspace.root_str(),
            &json!({ "path": "Plugin/Source/WrongModule/Public/Target.h" }),
        )
        .unwrap_err();

        assert!(error.contains("recovery_ambiguous"));
        assert!(error.contains("Plugin/Source/First/Public/Target.h"));
        assert!(error.contains("Plugin/Source/Second/Public/Target.h"));
    }

    #[test]
    fn read_tools_allow_absolute_paths_outside_workspace() {
        let workspace = TestWorkspace::new();
        let outside_dir = std::env::temp_dir().join(format!(
            "snowagent-outside-dir-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&outside_dir).unwrap();
        let outside = outside_dir.join("outside.log");
        fs::write(&outside, "outside").unwrap();

        let listed = list_dir(
            &workspace.root_str(),
            &json!({ "path": outside_dir.to_string_lossy() }),
        )
        .unwrap();
        let read = read_file(
            &workspace.root_str(),
            &json!({ "path": outside.to_string_lossy() }),
        )
        .unwrap();

        let _ = fs::remove_dir_all(outside_dir);
        assert!(listed["files"][0]
            .as_str()
            .unwrap()
            .ends_with("outside.log"));
        assert_eq!(read["text"], json!("outside"));
        assert!(read["file"].as_str().unwrap().contains("outside.log"));
    }

    #[test]
    fn write_tools_still_reject_absolute_paths_outside_workspace() {
        let workspace = TestWorkspace::new();
        let outside = std::env::temp_dir().join(format!(
            "snowagent-outside-write-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        let error = write_file(
            &workspace.root_str(),
            &json!({ "file": outside.to_string_lossy(), "content": "outside" }),
        )
        .unwrap_err();

        assert!(error.contains("path_outside_workspace"));
        assert!(!outside.exists());
    }

    #[test]
    fn search_file_orders_exact_filename_before_contains() {
        let workspace = TestWorkspace::new();
        workspace.write_text("src/main.cpp", "main");
        workspace.write_text("src/my_main.cpp", "main");
        workspace.write_text("src/other.cpp", "main");

        let result = search_file(
            &workspace.root_str(),
            &json!({ "pattern": "main.cpp", "max_results": 10 }),
        )
        .unwrap();

        assert_eq!(result["matches"][0]["path"], json!("src/main.cpp"));
        assert_eq!(result["matches"][0]["type"], json!("file"));
        assert_eq!(result["paths"][0], json!("src/main.cpp"));
        assert_eq!(result["paths"][1], json!("src/my_main.cpp"));
        assert_eq!(result["engine"], json!("codex-file-search"));
    }

    #[test]
    fn search_file_excludes_claude_worktrees() {
        let workspace = TestWorkspace::new();
        workspace.write_text(".claude/worktrees/tmp/src/main.cpp", "main");
        workspace.write_text("src/main.cpp", "main");

        let result = search_file(
            &workspace.root_str(),
            &json!({ "pattern": "main.cpp", "max_results": 10 }),
        )
        .unwrap();
        let paths = result["paths"].as_array().unwrap();

        assert_eq!(paths, &[json!("src/main.cpp")]);
    }

    #[test]
    fn search_file_allows_absolute_root_outside_workspace() {
        let workspace = TestWorkspace::new();
        let outside_dir = std::env::temp_dir().join(format!(
            "snowagent-outside-search-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&outside_dir).unwrap();
        fs::write(outside_dir.join("render_trace.log"), "trace").unwrap();

        let result = search_file(
            &workspace.root_str(),
            &json!({
                "pattern": "render_trace.log",
                "root": outside_dir.to_string_lossy(),
                "max_results": 10
            }),
        )
        .unwrap();

        let _ = fs::remove_dir_all(outside_dir);
        assert_eq!(result["paths"].as_array().unwrap().len(), 1);
        assert!(result["paths"][0]
            .as_str()
            .unwrap()
            .ends_with("render_trace.log"));
    }

    #[test]
    fn search_file_supports_wildcard_pattern() {
        let workspace = TestWorkspace::new();
        let outside_dir = std::env::temp_dir().join(format!(
            "snowagent-outside-wildcard-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&outside_dir).unwrap();
        fs::write(outside_dir.join("wz_render_frame_trace_9020.log"), "trace").unwrap();
        fs::write(outside_dir.join("wz_model_render_trace_9020.log"), "trace").unwrap();

        let result = search_file(
            &workspace.root_str(),
            &json!({
                "pattern": "wz_render_frame_trace_*.log",
                "root": outside_dir.to_string_lossy(),
                "max_results": 10
            }),
        )
        .unwrap();

        let _ = fs::remove_dir_all(outside_dir);
        assert_eq!(result["engine"], json!("wildcard-file-search"));
        assert_eq!(result["paths"].as_array().unwrap().len(), 1);
        assert!(result["paths"][0]
            .as_str()
            .unwrap()
            .ends_with("wz_render_frame_trace_9020.log"));
    }

    #[test]
    fn search_content_finds_matches_with_context_and_columns() {
        let workspace = TestWorkspace::new();
        workspace.write_text("src/code.cpp", "before\nneedle here needle\nafter\n");

        let result = search_content(
            &workspace.root_str(),
            &json!({
                "query": "needle",
                "file_glob": "*.cpp",
                "context_lines": 1,
                "max_results": 10
            }),
        )
        .unwrap();
        let first = &result["matches"][0];

        assert_eq!(first["file"], json!("src/code.cpp"));
        assert_eq!(first["line"], json!(2));
        assert_eq!(first["column"], json!(1));
        assert_eq!(first["columns"], json!([1, 13]));
        assert_eq!(first["before"][0]["text"], json!("before"));
        assert_eq!(first["after"][0]["text"], json!("after"));
        assert_eq!(result["matches"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn search_content_allows_absolute_root_outside_workspace() {
        let workspace = TestWorkspace::new();
        let outside_dir = std::env::temp_dir().join(format!(
            "snowagent-outside-content-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&outside_dir).unwrap();
        fs::write(outside_dir.join("render_trace.log"), "frame needle\n").unwrap();

        let result = search_content(
            &workspace.root_str(),
            &json!({
                "query": "needle",
                "root": outside_dir.to_string_lossy(),
                "file_glob": "*.log",
                "max_results": 10
            }),
        )
        .unwrap();

        let _ = fs::remove_dir_all(outside_dir);
        assert_eq!(result["matches"].as_array().unwrap().len(), 1);
        assert!(result["matches"][0]["file"]
            .as_str()
            .unwrap()
            .ends_with("render_trace.log"));
    }

    #[test]
    fn search_content_accepts_file_root() {
        let workspace = TestWorkspace::new();
        let outside_dir = std::env::temp_dir().join(format!(
            "snowagent-outside-file-root-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&outside_dir).unwrap();
        let log = outside_dir.join("wz_model_render_trace_9020.log");
        fs::write(&log, "before\nsubmitCachedTransparencyFaces.end ms=109\n").unwrap();

        let result = search_content(
            &workspace.root_str(),
            &json!({
                "query": "submitCachedTransparencyFaces.end",
                "root": log.to_string_lossy(),
                "max_results": 10,
                "context_lines": 0
            }),
        )
        .unwrap();

        let _ = fs::remove_dir_all(outside_dir);
        assert_eq!(result["matches"].as_array().unwrap().len(), 1);
        assert!(result["matches"][0]["file"]
            .as_str()
            .unwrap()
            .ends_with("wz_model_render_trace_9020.log"));
        assert_eq!(
            result["matches"][0]["text"],
            json!("submitCachedTransparencyFaces.end ms=109")
        );
    }

    #[test]
    fn search_content_reports_invalid_regex() {
        let workspace = TestWorkspace::new();
        workspace.write_text("src/code.cpp", "text");

        let error = search_content(
            &workspace.root_str(),
            &json!({ "query": "[", "regex": true }),
        )
        .unwrap_err();

        assert!(error.contains("invalid_regex"));
    }

    #[test]
    fn get_file_context_returns_requested_window() {
        let workspace = TestWorkspace::new();
        workspace.write_text("src/code.cpp", "one\ntwo\nthree\nfour\nfive\n");

        let result = get_file_context(
            &workspace.root_str(),
            &json!({ "path": "src/code.cpp", "line": 3, "before": 1, "after": 1 }),
        )
        .unwrap();

        assert_eq!(result["startLine"], json!(2));
        assert_eq!(result["endLine"], json!(4));
        assert_eq!(result["lines"].as_array().unwrap().len(), 3);
    }
}
