use std::cmp;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use regex::{Regex, RegexBuilder};
use serde_json::{json, Value};

use crate::path_utils::normalize_display_path;

const DEFAULT_MAX_READ_LINES: usize = 300;
const DEFAULT_MAX_RESULTS: usize = 100;
const MAX_RESULTS_LIMIT: usize = 500;
const DEFAULT_CONTENT_CONTEXT_LINES: usize = 2;
const DEFAULT_FILE_CONTEXT_BEFORE: usize = 30;
const DEFAULT_FILE_CONTEXT_AFTER: usize = 30;
const MAX_CONTEXT_LINES: usize = 200;
const BINARY_SAMPLE_BYTES: usize = 8192;
const SEARCH_FILE_SCAN_LIMIT: usize = 50_000;
const SEARCH_CONTENT_SCAN_LIMIT: usize = 25_000;
const SEARCH_SCAN_TIMEOUT_MS: u128 = 12_000;

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

const DEFAULT_CONTENT_EXTENSIONS: &[&str] = &[
    ".h", ".hpp", ".c", ".cpp", ".cc", ".cxx", ".inl", ".ixx", ".cs", ".sln", ".vcxproj", ".props",
    ".targets", ".json", ".xml", ".txt", ".md",
];

pub fn list_dir(workspace_root: &str, arguments: &Value) -> Result<Value, String> {
    let workspace = canonical_workspace_root(workspace_root)?;
    let raw_path = required_string(arguments, "path")?;
    let dir = resolve_existing_path(&workspace, &raw_path)?;
    if !dir.is_dir() {
        return Err(format!(
            "not_directory: {}",
            relative_or_display(&workspace, &dir)
        ));
    }

    let mut directories = Vec::new();
    let mut files = Vec::new();
    for entry in sorted_read_dir(&dir)? {
        let path = entry.path();
        if path.is_dir() {
            if is_ignored_dir(&path) {
                continue;
            }
            let canonical = canonicalize_path(&path)
                .map_err(|error| format!("read_dir_failed: {}: {error}", path.display()))?;
            directories.push(relative_path(&workspace, &canonical));
        } else if path.is_file() {
            let canonical = canonicalize_path(&path)
                .map_err(|error| format!("file_not_found: {}: {error}", path.display()))?;
            files.push(relative_path(&workspace, &canonical));
        }
    }

    directories.sort_by_key(|path| path.to_ascii_lowercase());
    files.sort_by_key(|path| path.to_ascii_lowercase());

    Ok(json!({
        "path": relative_path(&workspace, &dir),
        "directories": directories,
        "files": files,
    }))
}

pub fn read_file(workspace_root: &str, arguments: &Value) -> Result<Value, String> {
    let workspace = canonical_workspace_root(workspace_root)?;
    let raw_path = required_string(arguments, "path")?;
    let file = resolve_existing_path(&workspace, &raw_path)?;
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

    Ok(json!({
        "file": relative_path(&workspace, &file),
        "totalLines": total_lines,
        "startLine": actual_start_line,
        "endLine": actual_end_line,
        "maxLines": DEFAULT_MAX_READ_LINES,
        "truncated": truncated,
        "message": message,
        "lines": lines,
    }))
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

    let mut matches = Vec::new();
    let started = Instant::now();
    let mut scan_limited = false;
    let scanned_files = walk_files_until(&workspace, &root, &mut |path, scanned| {
        if scanned > SEARCH_FILE_SCAN_LIMIT || started.elapsed().as_millis() >= SEARCH_SCAN_TIMEOUT_MS
        {
            scan_limited = true;
            return Ok(WalkControl::Stop);
        }
        let rel = relative_path(&workspace, path);
        if let Some(score) = file_match_score(&rel, pattern) {
            matches.push((score, rel));
        }
        Ok(WalkControl::Continue)
    })?;

    matches.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    let total_matches = matches.len();
    let truncated = total_matches > max_results || scan_limited;
    let paths = matches
        .into_iter()
        .take(max_results)
        .map(|(_, path)| path)
        .collect::<Vec<_>>();

    Ok(json!({
        "root": relative_path(&workspace, &root),
        "pattern": pattern,
        "matches": paths,
        "count": cmp::min(total_matches, max_results),
        "totalMatches": total_matches,
        "scannedFiles": scanned_files,
        "complete": !scan_limited,
        "maxResults": max_results,
        "truncated": truncated,
        "message": if scan_limited {
            Some(format!(
                "search_limited: scanned {scanned_files} files before returning partial results; narrow root or pattern"
            ))
        } else if truncated {
            Some(format!("too_many_results: returned first {max_results} of {total_matches} matches"))
        } else {
            None
        },
    }))
}

pub fn search_content(workspace_root: &str, arguments: &Value) -> Result<Value, String> {
    let workspace = canonical_workspace_root(workspace_root)?;
    let query = required_string(arguments, "query")?;
    if query.trim().is_empty() {
        return Err("invalid_arguments: query must not be empty".to_string());
    }

    let root = resolve_search_root(&workspace, arguments)?;
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

pub fn get_file_context(workspace_root: &str, arguments: &Value) -> Result<Value, String> {
    let workspace = canonical_workspace_root(workspace_root)?;
    let raw_path = required_string(arguments, "path")?;
    let line = required_usize(arguments, "line")?;
    if line == 0 {
        return Err("invalid_range: line must be >= 1".to_string());
    }

    let file = resolve_existing_path(&workspace, &raw_path)?;
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
    command
        .current_dir(root)
        .arg("--json")
        .arg("--line-number")
        .arg("--column")
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
        let column = data
            .get("submatches")
            .and_then(Value::as_array)
            .and_then(|submatches| submatches.first())
            .and_then(|submatch| submatch.get("start"))
            .and_then(Value::as_u64)
            .map(|column| column + 1)
            .unwrap_or(1);
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
            "column": column,
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
        if is_binary_file(path)? {
            return Ok(WalkControl::Continue);
        }

        let mut line_number = 0usize;
        let file = File::open(path).map_err(|error| {
            format!(
                "file_not_found: {}: {error}",
                relative_path(workspace, path)
            )
        })?;
        let mut reader = BufReader::new(file);
        let mut bytes = Vec::new();
        while reader
            .read_until(b'\n', &mut bytes)
            .map_err(|error| format!("read_failed: {}: {error}", relative_path(workspace, path)))?
            > 0
        {
            line_number += 1;
            let line = bytes_to_line(&bytes);
            let columns = find_columns(&line, &normalized_query, case_sensitive, regex);
            for column in columns {
                if matches.len() >= max_results {
                    truncated = true;
                    return Ok(WalkControl::Stop);
                }
                let (before, after) =
                    read_before_after(path, line_number, context_lines, context_lines)?;
                matches.push(json!({
                    "file": relative_path(workspace, path),
                    "line": line_number,
                    "column": column,
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

fn resolve_search_root(workspace: &Path, arguments: &Value) -> Result<PathBuf, String> {
    let root = optional_string(arguments, "root")?.unwrap_or_else(|| ".".to_string());
    let path = resolve_existing_path(workspace, &root)?;
    if !path.is_dir() {
        return Err(format!(
            "not_directory: {}",
            relative_or_display(workspace, &path)
        ));
    }
    Ok(path)
}

fn resolve_rg_path(workspace: &Path, root: &Path, path_text: &str) -> Result<PathBuf, String> {
    let raw = normalize_display_path(path_text);
    let path = PathBuf::from(raw.trim());
    let candidate = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    let canonical = canonicalize_path(&candidate)
        .map_err(|_| format!("file_not_found: {}", candidate.display()))?;
    ensure_inside_workspace(workspace, &canonical, path_text)?;
    Ok(canonical)
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

fn walk_files(
    workspace: &Path,
    root: &Path,
    visit: &mut impl FnMut(&Path) -> Result<(), String>,
) -> Result<(), String> {
    walk_files_until(workspace, root, &mut |path, _scanned| {
        visit(path)?;
        Ok(WalkControl::Continue)
    })
    .map(|_| ())
}

fn walk_files_until(
    workspace: &Path,
    root: &Path,
    visit: &mut impl FnMut(&Path, usize) -> Result<WalkControl, String>,
) -> Result<usize, String> {
    let mut stack = vec![root.to_path_buf()];
    let mut scanned = 0usize;
    while let Some(dir) = stack.pop() {
        for entry in sorted_read_dir(&dir)? {
            let path = entry.path();
            if path.is_dir() {
                if is_ignored_dir(&path) {
                    continue;
                }
                let canonical = canonicalize_path(&path)
                    .map_err(|error| format!("read_dir_failed: {}: {error}", path.display()))?;
                ensure_inside_workspace(workspace, &canonical, &path.to_string_lossy())?;
                stack.push(canonical);
            } else if path.is_file() {
                let canonical = canonicalize_path(&path)
                    .map_err(|error| format!("file_not_found: {}: {error}", path.display()))?;
                ensure_inside_workspace(workspace, &canonical, &path.to_string_lossy())?;
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

fn file_match_score(relative_path: &str, pattern: &str) -> Option<(u8, String)> {
    let pattern = pattern.to_ascii_lowercase();
    let path = relative_path.to_ascii_lowercase();
    let file_name = Path::new(relative_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(relative_path)
        .to_ascii_lowercase();

    let score = if file_name == pattern {
        0
    } else if file_name.contains(&pattern) {
        1
    } else if fuzzy_subsequence(&file_name, &pattern) {
        2
    } else if path.contains(&pattern) {
        3
    } else if fuzzy_subsequence(&path, &pattern) {
        4
    } else {
        return None;
    };
    Some((score, path))
}

fn fuzzy_subsequence(text: &str, pattern: &str) -> bool {
    if pattern.is_empty() {
        return true;
    }
    let mut chars = pattern.chars();
    let mut current = chars.next();
    for ch in text.chars() {
        if Some(ch) == current {
            current = chars.next();
            if current.is_none() {
                return true;
            }
        }
    }
    false
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
    Command::new("rg")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn add_ignore_globs(command: &mut Command) {
    for ignored in IGNORED_DIRS {
        command.arg("--glob").arg(format!("!**/{ignored}/**"));
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
            self.root.join(relative)
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
    fn paths_cannot_escape_workspace() {
        let workspace = TestWorkspace::new();
        let outside = std::env::temp_dir().join(format!(
            "snowagent-outside-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&outside, "outside").unwrap();

        let error = read_file(
            &workspace.root_str(),
            &json!({ "path": outside.to_string_lossy() }),
        )
        .unwrap_err();

        let _ = fs::remove_file(outside);
        assert!(error.contains("path_outside_workspace"));
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

        assert_eq!(result["matches"][0], json!("src/main.cpp"));
        assert_eq!(result["matches"][1], json!("src/my_main.cpp"));
    }

    #[test]
    fn search_content_finds_matches_with_context() {
        let workspace = TestWorkspace::new();
        workspace.write_text("src/code.cpp", "before\nneedle here\nafter\n");

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
        assert_eq!(first["before"][0]["text"], json!("before"));
        assert_eq!(first["after"][0]["text"], json!("after"));
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
