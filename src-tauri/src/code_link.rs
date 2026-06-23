use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::path_utils::normalize_display_path;
use crate::project_registry::ProjectSession;
use crate::tool_trace::ToolTraceEvent;

const MAX_SUFFIX_SCAN_FILES: usize = 50_000;
const IGNORED_SUFFIX_SCAN_DIRS: &[&str] = &[
    ".git",
    ".vs",
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
    let cleaned = raw_link
        .trim()
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'');
    let (path_part, line, column) = split_code_link(cleaned)?;
    let path = resolve_path(&project.repo_root, path_part)?;

    Ok(OpenFilePayload { path, line, column })
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
    if suffix.is_empty() || !suffix.chars().all(|character| character.is_ascii_digit()) {
        return None;
    }
    suffix
        .parse::<u32>()
        .ok()
        .map(|number| (&value[..index], number))
}

fn resolve_path(repo_root: &str, path_part: &str) -> Result<String, String> {
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
            if let Some(suffix_match) = resolve_by_unique_suffix(repo_root_path, path_part)? {
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

fn resolve_by_unique_suffix(repo_root: &Path, path_part: &str) -> Result<Option<String>, String> {
    let repo_root = match repo_root.canonicalize() {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    let suffix = normalize_display_path(path_part)
        .replace('\\', "/")
        .trim_matches('/')
        .to_ascii_lowercase();
    if suffix.is_empty() || !suffix.contains('/') {
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
        _ => Err(format!(
            "Code link path is ambiguous: {} matched {} files; include more path segments",
            normalize_display_path(path_part),
            matches.len()
        )),
    }
}

fn find_suffix_matches(
    repo_root: &Path,
    dir: &Path,
    suffix: &str,
    matches: &mut Vec<PathBuf>,
    scanned: &mut usize,
) -> Result<(), String> {
    if *scanned >= MAX_SUFFIX_SCAN_FILES || matches.len() > 1 {
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
            if *scanned >= MAX_SUFFIX_SCAN_FILES || matches.len() > 1 {
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
                if matches.len() > 1 {
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
