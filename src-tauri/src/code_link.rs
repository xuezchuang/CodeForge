use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::path_utils::normalize_display_path;
use crate::project_registry::ProjectSession;
use crate::tool_trace::ToolTraceEvent;

const MAX_SUFFIX_SCAN_FILES: usize = 50_000;
const MAX_SUFFIX_SCAN_MATCHES: usize = 64;
const IGNORED_SUFFIX_SCAN_DIRS: &[&str] = &[
    ".git",
    ".vs",
    ".claude",
    "bin",
    "obj",
    "build",
    "out",
    "node_modules",
    ".cache",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenFilePayload {
    pub path: String,
    pub line: u32,
    pub column: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeLinkResult {
    pub resolved_path: String,
    pub line: u32,
    pub column: Option<u32>,
    pub bridge_called: bool,
    pub fallback_started_vs: bool,
    pub message: String,
    pub trace_event: Option<ToolTraceEvent>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VsBridgeOpenFileResponse {
    ok: Option<bool>,
    message: Option<String>,
}

pub fn parse_code_link(
    project: &ProjectSession,
    raw_link: &str,
) -> Result<OpenFilePayload, String> {
    parse_code_link_with_context(project, raw_link, &[])
}

pub fn parse_code_link_with_context(
    project: &ProjectSession,
    raw_link: &str,
    context_links: &[String],
) -> Result<OpenFilePayload, String> {
    let cleaned = clean_raw_link(raw_link);
    let (path_part, line, column) = split_code_link(cleaned)?;
    let decoded_path_part = decode_percent_encoded_path(path_part);
    let path = resolve_path(&project.repo_root, &decoded_path_part, context_links)?;

    Ok(OpenFilePayload { path, line, column })
}

fn clean_raw_link(raw_link: &str) -> &str {
    raw_link
        .trim()
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
}

pub async fn call_vs_open_file(endpoint: &str, payload: &OpenFilePayload) -> Result<(), String> {
    let url = format!("{}/openFile", endpoint.trim_end_matches('/'));
    let response = reqwest::Client::new()
        .post(&url)
        .json(payload)
        .send()
        .await
        .map_err(|error| {
            format!(
                "VS Bridge openFile failed. endpoint={endpoint}; status=network_error; error={error}"
            )
        })?;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "VS Bridge openFile failed. endpoint={endpoint}; status={}; error={}",
            status.as_u16(),
            body
        ));
    }

    if let Ok(parsed) = serde_json::from_str::<VsBridgeOpenFileResponse>(&body) {
        if parsed.ok == Some(false) {
            return Err(format!(
                "VS Bridge openFile failed. endpoint={endpoint}; status={}; error={}",
                status.as_u16(),
                parsed
                    .message
                    .unwrap_or_else(|| "VS Bridge returned ok=false".to_string())
            ));
        }
    }

    Ok(())
}

fn split_code_link(raw_link: &str) -> Result<(&str, u32, Option<u32>), String> {
    let (without_last, last_value) = split_numeric_suffix(raw_link)
        .ok_or_else(|| format!("Code link path could not be parsed: {raw_link}"))?;

    if let Some((without_second, second_value)) = split_numeric_suffix(without_last) {
        if without_second.trim().is_empty() {
            return Err(format!("Code link path could not be parsed: {raw_link}"));
        }
        return Ok((without_second, second_value, Some(last_value)));
    }

    if without_last.trim().is_empty() {
        return Err(format!("Code link path could not be parsed: {raw_link}"));
    }
    Ok((without_last, last_value, None))
}

fn split_numeric_suffix(value: &str) -> Option<(&str, u32)> {
    let index = value.rfind(':')?;
    let suffix = &value[index + 1..];
    let start_line = if let Some((start_line, end_line)) = suffix.split_once('-') {
        if start_line.is_empty()
            || end_line.is_empty()
            || !start_line
                .chars()
                .all(|character| character.is_ascii_digit())
            || !end_line.chars().all(|character| character.is_ascii_digit())
        {
            return None;
        }
        start_line
    } else {
        if suffix.is_empty() || !suffix.chars().all(|character| character.is_ascii_digit()) {
            return None;
        }
        suffix
    };

    start_line
        .parse::<u32>()
        .ok()
        .map(|number| (&value[..index], number))
}

fn decode_percent_encoded_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    let mut changed = false;

    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
            {
                decoded.push(high << 4 | low);
                index += 3;
                changed = true;
                continue;
            }
        }

        decoded.push(bytes[index]);
        index += 1;
    }

    if changed {
        String::from_utf8(decoded).unwrap_or_else(|_| path.to_string())
    } else {
        path.to_string()
    }
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn resolve_path(
    repo_root: &str,
    path_part: &str,
    context_links: &[String],
) -> Result<String, String> {
    let normalized = path_part.trim().replace('/', "\\");
    let path = Path::new(&normalized);
    let repo_root_path = Path::new(repo_root);
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo_root_path.join(path)
    };

    if !candidate.exists() {
        if !path.is_absolute() {
            if let Some(suffix_match) =
                resolve_by_unique_suffix(repo_root_path, path_part, context_links)?
            {
                return Ok(suffix_match);
            }
        }

        return Err(format!(
            "File does not exist: {}",
            normalize_display_path(&candidate.to_string_lossy())
        ));
    }

    let canonical = candidate.canonicalize().map_err(|error| {
        format!(
            "Code link path canonicalization failed {}: {error}",
            candidate.display()
        )
    })?;
    Ok(normalize_display_path(&canonical.to_string_lossy()))
}

fn resolve_by_unique_suffix(
    repo_root: &Path,
    path_part: &str,
    context_links: &[String],
) -> Result<Option<String>, String> {
    let repo_root = match repo_root.canonicalize() {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    let suffix = normalize_display_path(path_part)
        .replace('\\', "/")
        .trim_matches('/')
        .to_ascii_lowercase();
    if suffix.is_empty() {
        return Ok(None);
    }

    let mut matches = Vec::new();
    let mut scanned = 0usize;
    find_suffix_matches(&repo_root, &repo_root, &suffix, &mut matches, &mut scanned)?;

    match matches.len() {
        0 => Ok(None),
        1 => {
            let canonical = matches[0].canonicalize().map_err(|error| {
                format!(
                    "Code link path canonicalization failed {}: {error}",
                    matches[0].display()
                )
            })?;
            Ok(Some(normalize_display_path(&canonical.to_string_lossy())))
        }
        _ => {
            if let Some(selected) =
                select_suffix_match_by_context(&repo_root, &matches, context_links)
            {
                let canonical = selected.canonicalize().map_err(|error| {
                    format!(
                        "Code link path canonicalization failed {}: {error}",
                        selected.display()
                    )
                })?;
                return Ok(Some(normalize_display_path(&canonical.to_string_lossy())));
            }
            Err(format!(
                "Code link path is ambiguous: {} matched {} files; include more path segments",
                normalize_display_path(path_part),
                matches.len()
            ))
        }
    }
}

fn select_suffix_match_by_context(
    repo_root: &Path,
    matches: &[PathBuf],
    context_links: &[String],
) -> Option<PathBuf> {
    let context_paths = context_links
        .iter()
        .filter_map(|raw_link| context_link_path(repo_root, raw_link))
        .collect::<Vec<_>>();
    if context_paths.is_empty() {
        return None;
    }

    let mut best_match = None;
    let mut best_score = 0usize;
    let mut tied = false;
    for candidate in matches {
        let score = context_paths
            .iter()
            .map(|context_path| common_path_prefix_score(repo_root, candidate, context_path))
            .max()
            .unwrap_or(0);
        if score > best_score {
            best_score = score;
            best_match = Some(candidate.clone());
            tied = false;
        } else if score == best_score && score > 0 {
            tied = true;
        }
    }

    if best_score > 0 && !tied {
        best_match
    } else {
        None
    }
}

fn context_link_path(repo_root: &Path, raw_link: &str) -> Option<PathBuf> {
    let cleaned = clean_raw_link(raw_link);
    let (path_part, _, _) = split_code_link(cleaned).ok()?;
    let decoded_path_part = decode_percent_encoded_path(path_part);
    let path_part = decoded_path_part.as_str();
    let has_path_segments = path_part.contains('\\') || path_part.contains('/');
    let normalized = path_part.trim().replace('/', "\\");
    let path = Path::new(&normalized);
    if !has_path_segments && !path.is_absolute() {
        return None;
    }

    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo_root.join(path)
    };
    if candidate.exists() {
        return candidate.canonicalize().ok();
    }

    resolve_context_path_by_unique_suffix(repo_root, path_part)
}

fn resolve_context_path_by_unique_suffix(repo_root: &Path, path_part: &str) -> Option<PathBuf> {
    let suffix = normalize_display_path(path_part)
        .replace('\\', "/")
        .trim_matches('/')
        .to_ascii_lowercase();
    if suffix.is_empty() {
        return None;
    }

    let mut matches = Vec::new();
    let mut scanned = 0usize;
    find_suffix_matches(repo_root, repo_root, &suffix, &mut matches, &mut scanned).ok()?;
    if matches.len() == 1 {
        matches[0].canonicalize().ok()
    } else {
        None
    }
}

fn common_path_prefix_score(repo_root: &Path, left: &Path, right: &Path) -> usize {
    let left_parts = relative_path_parts(repo_root, left);
    let right_parts = relative_path_parts(repo_root, right);
    left_parts
        .iter()
        .zip(right_parts.iter())
        .take_while(|(left, right)| left == right)
        .count()
}

fn relative_path_parts(repo_root: &Path, path: &Path) -> Vec<String> {
    let relative = path.strip_prefix(repo_root).unwrap_or(path);
    normalize_display_path(&relative.to_string_lossy())
        .replace('\\', "/")
        .split('/')
        .filter(|part| !part.is_empty())
        .map(|part| part.to_ascii_lowercase())
        .collect()
}

fn find_suffix_matches(
    repo_root: &Path,
    dir: &Path,
    suffix: &str,
    matches: &mut Vec<PathBuf>,
    scanned: &mut usize,
) -> Result<(), String> {
    if *scanned >= MAX_SUFFIX_SCAN_FILES || matches.len() >= MAX_SUFFIX_SCAN_MATCHES {
        return Ok(());
    }

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        if path.is_dir() {
            if is_ignored_suffix_scan_dir(&path) {
                continue;
            }
            find_suffix_matches(repo_root, &path, suffix, matches, scanned)?;
            if *scanned >= MAX_SUFFIX_SCAN_FILES || matches.len() >= MAX_SUFFIX_SCAN_MATCHES {
                return Ok(());
            }
        } else if path.is_file() {
            *scanned += 1;
            let relative = path.strip_prefix(repo_root).unwrap_or(&path);
            let relative = normalize_display_path(&relative.to_string_lossy())
                .replace('\\', "/")
                .to_ascii_lowercase();
            if relative.ends_with(suffix) {
                matches.push(path);
                if matches.len() >= MAX_SUFFIX_SCAN_MATCHES {
                    return Ok(());
                }
            }
        }
    }

    Ok(())
}

fn is_ignored_suffix_scan_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            IGNORED_SUFFIX_SCAN_DIRS
                .iter()
                .any(|ignored| name.eq_ignore_ascii_case(ignored))
        })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn parses_relative_link_with_line() {
        let root = create_temp_project();
        let file = root.join("Source").join("Game").join("Foo.cpp");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "int main() {}\n").unwrap();

        let project = test_project(root.to_string_lossy().to_string());
        let payload = parse_code_link(&project, "Source/Game/Foo.cpp:128").unwrap();

        assert_eq!(payload.line, 128);
        assert_eq!(payload.column, None);
        assert!(payload.path.ends_with("Source\\Game\\Foo.cpp"));
    }

    #[test]
    fn parses_relative_link_with_line_and_column() {
        let root = create_temp_project();
        let file = root.join("Source").join("Game").join("Foo.cpp");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "int main() {}\n").unwrap();

        let project = test_project(root.to_string_lossy().to_string());
        let payload = parse_code_link(&project, "Source/Game/Foo.cpp:128:5").unwrap();

        assert_eq!(payload.line, 128);
        assert_eq!(payload.column, Some(5));
    }

    #[test]
    fn parses_absolute_windows_path_with_drive_colon() {
        let root = create_temp_project();
        let file = root.join("Source").join("Game").join("Foo.cpp");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "int main() {}\n").unwrap();

        let project = test_project(root.to_string_lossy().to_string());
        let raw_link = format!("{}:128", file.to_string_lossy());
        let payload = parse_code_link(&project, &raw_link).unwrap();

        assert_eq!(payload.line, 128);
        assert_eq!(payload.column, None);
        assert!(payload.path.ends_with("Source\\Game\\Foo.cpp"));
    }

    #[test]
    fn parses_absolute_forward_slash_path_with_column() {
        let root = create_temp_project();
        let file = root.join("Source").join("Game").join("Foo.cpp");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "int main() {}\n").unwrap();

        let project = test_project(root.to_string_lossy().to_string());
        let raw_link = format!("{}:128:5", file.to_string_lossy().replace('\\', "/"));
        let payload = parse_code_link(&project, &raw_link).unwrap();

        assert_eq!(payload.line, 128);
        assert_eq!(payload.column, Some(5));
        assert!(payload.path.ends_with("Source\\Game\\Foo.cpp"));
    }

    #[test]
    fn returns_file_does_not_exist_for_missing_file() {
        let root = create_temp_project();
        let project = test_project(root.to_string_lossy().to_string());
        let error = parse_code_link(&project, "Source/Game/Missing.cpp:1").unwrap_err();

        assert!(error.starts_with("File does not exist:"));
    }

    #[test]
    fn resolves_unique_suffix_link_when_rendered_path_was_truncated() {
        let root = create_temp_project();
        let file = root
            .join("src")
            .join("core")
            .join("wz_render_core")
            .join("02 中文目录")
            .join("part")
            .join("wzFigureNavigatorWidget.cpp");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "int main() {}\n").unwrap();

        let project = test_project(root.to_string_lossy().to_string());
        let payload = parse_code_link(&project, "part/wzFigureNavigatorWidget.cpp:240").unwrap();

        assert_eq!(payload.line, 240);
        assert!(payload.path.ends_with("part\\wzFigureNavigatorWidget.cpp"));
    }

    #[test]
    fn resolves_unique_bare_filename_link() {
        let root = create_temp_project();
        let file = root.join("src").join("core").join("wzFigureBrep.cpp");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "int main() {}\n").unwrap();

        let project = test_project(root.to_string_lossy().to_string());
        let payload = parse_code_link(&project, "wzFigureBrep.cpp:77").unwrap();

        assert_eq!(payload.line, 77);
        assert!(payload.path.ends_with("src\\core\\wzFigureBrep.cpp"));
    }

    #[test]
    fn resolves_bare_filename_link_ignores_claude_worktrees() {
        let root = create_temp_project();
        let source = root
            .join("src")
            .join("core")
            .join("wz_3D")
            .join("05 render")
            .join("wzFigureGLDrawer.cpp");
        let worktree = root
            .join(".claude")
            .join("worktrees")
            .join("copy")
            .join("src")
            .join("core")
            .join("wz_3D")
            .join("05 render")
            .join("wzFigureGLDrawer.cpp");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::create_dir_all(worktree.parent().unwrap()).unwrap();
        fs::write(&source, "int source;\n").unwrap();
        fs::write(&worktree, "int worktree;\n").unwrap();

        let project = test_project(root.to_string_lossy().to_string());
        let payload = parse_code_link(&project, "wzFigureGLDrawer.cpp:1106").unwrap();

        assert_eq!(payload.line, 1106);
        assert!(payload
            .path
            .ends_with("src\\core\\wz_3D\\05 render\\wzFigureGLDrawer.cpp"));
    }

    #[test]
    fn resolves_percent_encoded_space_in_link_path() {
        let root = create_temp_project();
        let file = root
            .join("src")
            .join("core")
            .join("wz_render_core")
            .join("05 render")
            .join("wzModelRenderer.cpp");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "int main() {}\n").unwrap();

        let project = test_project(root.to_string_lossy().to_string());
        let payload = parse_code_link(
            &project,
            "src/core/wz_render_core/05%20render/wzModelRenderer.cpp:1819",
        )
        .unwrap();

        assert_eq!(payload.line, 1819);
        assert!(payload
            .path
            .ends_with("src\\core\\wz_render_core\\05 render\\wzModelRenderer.cpp"));
    }

    #[test]
    fn parses_line_range_link_at_start_line() {
        let root = create_temp_project();
        let file = root.join("src").join("core").join("wzFigure.cpp");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "int main() {}\n").unwrap();

        let project = test_project(root.to_string_lossy().to_string());
        let payload = parse_code_link(&project, "wzFigure.cpp:6-9").unwrap();

        assert_eq!(payload.line, 6);
        assert_eq!(payload.column, None);
        assert!(payload.path.ends_with("src\\core\\wzFigure.cpp"));
    }

    #[test]
    fn resolves_ambiguous_bare_filename_link_from_message_context() {
        let root = create_temp_project();
        let first = root
            .join("src")
            .join("core")
            .join("wz_3D")
            .join("00 Interface")
            .join("wzFigureBrep.cpp");
        let second = root
            .join("src")
            .join("core")
            .join("wz_render_core")
            .join("00 Interface")
            .join("wzFigureBrep.cpp");
        let context = root
            .join("src")
            .join("core")
            .join("wz_render_core")
            .join("00 Interface")
            .join("wzFigure.h");
        fs::create_dir_all(first.parent().unwrap()).unwrap();
        fs::create_dir_all(second.parent().unwrap()).unwrap();
        fs::write(&first, "int a;\n").unwrap();
        fs::write(&second, "int b;\n").unwrap();
        fs::write(&context, "int context;\n").unwrap();

        let project = test_project(root.to_string_lossy().to_string());
        let context_links = vec!["src/core/wz_render_core/00 Interface/wzFigure.h:59".to_string()];
        let payload =
            parse_code_link_with_context(&project, "wzFigureBrep.cpp:77", &context_links).unwrap();

        assert_eq!(payload.line, 77);
        assert!(payload
            .path
            .ends_with("src\\core\\wz_render_core\\00 Interface\\wzFigureBrep.cpp"));
    }

    #[test]
    fn reports_ambiguous_suffix_link() {
        let root = create_temp_project();
        let first = root.join("src").join("a").join("part").join("Widget.cpp");
        let second = root.join("src").join("b").join("part").join("Widget.cpp");
        fs::create_dir_all(first.parent().unwrap()).unwrap();
        fs::create_dir_all(second.parent().unwrap()).unwrap();
        fs::write(&first, "int a;\n").unwrap();
        fs::write(&second, "int b;\n").unwrap();

        let project = test_project(root.to_string_lossy().to_string());
        let error = parse_code_link(&project, "part/Widget.cpp:10").unwrap_err();

        assert!(error.contains("Code link path is ambiguous"));
    }

    fn create_temp_project() -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("snowagent-code-link-test-{unique}"));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn test_project(repo_root: String) -> ProjectSession {
        ProjectSession {
            id: "project".to_string(),
            name: "Project".to_string(),
            repo_root: repo_root.clone(),
            solution_path: Some(format!("{repo_root}\\Project.sln")),
            uproject_path: None,
            build_command: None,
            vs_process_id: None,
            vs_bridge_endpoint: None,
            created_at: "now".to_string(),
            updated_at: "now".to_string(),
        }
    }
}
