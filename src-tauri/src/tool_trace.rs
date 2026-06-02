use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::project_registry::ProjectSession;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceEventType {
    UserMessage,
    LlmRequest,
    LlmResponse,
    ToolCall,
    ToolResult,
    FinalResponse,
    ModelMessage,
    SystemEvent,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceStatus {
    Running,
    Success,
    Warning,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolTraceEvent {
    pub id: String,
    pub task_id: String,
    pub step_index: u32,
    #[serde(rename = "type")]
    pub event_type: TraceEventType,
    pub tool_name: Option<String>,
    pub title: String,
    pub input: Option<serde_json::Value>,
    pub output: Option<serde_json::Value>,
    pub output_summary: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub duration_ms: Option<u64>,
    pub status: TraceStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MockAgentRun {
    pub task_id: String,
    pub traces: Vec<ToolTraceEvent>,
}

#[derive(Default)]
pub struct ToolTraceStore {
    traces_by_task: HashMap<String, Vec<ToolTraceEvent>>,
}

impl ToolTraceStore {
    pub fn insert_task(&mut self, task_id: String, traces: Vec<ToolTraceEvent>) {
        self.traces_by_task.insert(task_id, traces);
    }

    pub fn append_event(&mut self, task_id: &str, event: ToolTraceEvent) {
        self.traces_by_task
            .entry(task_id.to_string())
            .or_default()
            .push(event);
    }

    pub fn next_step_index(&self, task_id: &str) -> u32 {
        self.traces_by_task
            .get(task_id)
            .and_then(|events| events.iter().map(|event| event.step_index).max())
            .unwrap_or(0)
            + 1
    }

    pub fn list(&self, task_id: &str) -> Vec<ToolTraceEvent> {
        self.traces_by_task
            .get(task_id)
            .cloned()
            .unwrap_or_else(Vec::new)
    }
}

pub fn create_mock_agent_run(project: &ProjectSession, user_prompt: &str) -> MockAgentRun {
    let task_id = Uuid::new_v4().to_string();
    let sample = find_sample_code_link(project);
    let code_link = sample
        .as_ref()
        .map(|sample| sample.code_link.as_str())
        .unwrap_or("No C++ source or header file found");
    let code_path = sample
        .as_ref()
        .map(|sample| sample.relative_path.as_str())
        .unwrap_or("None");
    let traces = vec![
        event(
            &task_id,
            1,
            TraceEventType::SystemEvent,
            None,
            "Start task",
            Some(json!({ "projectId": project.id, "prompt": user_prompt })),
            None,
            Some("Task accepted by mock agent".to_string()),
            4,
        ),
        event(
            &task_id,
            2,
            TraceEventType::ToolResult,
            Some("list_files"),
            "list_files",
            Some(json!({ "root": project.repo_root, "pattern": "**/*.{cpp,h,hpp,hh,c,cc,cxx}" })),
            Some(json!({ "selected": code_path })),
            Some(format!("selected {code_path}")),
            18,
        ),
        event(
            &task_id,
            3,
            TraceEventType::ToolResult,
            Some("read_file"),
            "read_file",
            Some(json!({ "path": code_path })),
            Some(json!({
                "path": code_path,
                "line": sample.as_ref().map(|sample| sample.line),
            })),
            Some(format!("read_file {code_path}")),
            25,
        ),
        event(
            &task_id,
            4,
            TraceEventType::ModelMessage,
            None,
            "model_message",
            None,
            Some(json!({ "message": format!("Suggested edit {code_link}") })),
            Some(format!("Suggested edit {code_link}")),
            15,
        ),
        event(
            &task_id,
            5,
            TraceEventType::ToolResult,
            Some("open_code_link"),
            "open_code_link suggestion",
            Some(json!({ "rawLink": code_link })),
            Some(json!({ "rawLink": code_link })),
            Some(format!("open_code_link suggestion {code_link}")),
            5,
        ),
    ];

    MockAgentRun { task_id, traces }
}

#[derive(Clone, Debug)]
struct SampleCodeLink {
    relative_path: String,
    code_link: String,
    line: u32,
}

const MOCK_CODE_EXTENSIONS: [&str; 7] = ["cpp", "cxx", "cc", "c", "h", "hpp", "hh"];

fn find_sample_code_link(project: &ProjectSession) -> Option<SampleCodeLink> {
    let root = Path::new(&project.repo_root);
    let source_root = root.join("Source");
    for extension in MOCK_CODE_EXTENSIONS {
        let Some(relative_path) = find_first_source_file(root, &source_root, extension)
            .or_else(|| find_first_source_file(root, root, extension))
        else {
            continue;
        };
        let line = choose_sample_line(&root.join(&relative_path));
        let code_link = format!("{}:{line}", relative_path.replace('\\', "/"));
        return Some(SampleCodeLink {
            relative_path,
            code_link,
            line,
        });
    }

    None
}

fn find_first_source_file(
    repo_root: &Path,
    search_root: &Path,
    preferred_extension: &str,
) -> Option<String> {
    if !search_root.is_dir() {
        return None;
    }

    let mut stack = vec![search_root.to_path_buf()];
    let mut visited = 0usize;
    while let Some(dir) = stack.pop() {
        visited += 1;
        if visited > 5000 {
            break;
        }

        let mut entries = fs::read_dir(&dir)
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<PathBuf>>();
        entries.sort();

        for path in entries.into_iter().rev() {
            if path.is_dir() {
                if should_skip_dir(&path) {
                    continue;
                }
                stack.push(path);
            } else if is_source_file(&path, preferred_extension) {
                return path
                    .strip_prefix(repo_root)
                    .ok()
                    .map(|relative| relative.to_string_lossy().to_string());
            }
        }
    }

    None
}

fn choose_sample_line(path: &Path) -> u32 {
    let line_count = fs::read_to_string(path)
        .ok()
        .map(|contents| contents.lines().count())
        .unwrap_or(1);
    if line_count >= 10 {
        10
    } else {
        1
    }
}

fn should_skip_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    matches!(
        name,
        ".git" | ".vs" | "node_modules" | "target" | "Binaries" | "Intermediate" | "Saved"
    )
}

fn is_source_file(path: &Path, preferred_extension: &str) -> bool {
    let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
        return false;
    };
    extension.eq_ignore_ascii_case(preferred_extension)
}

fn event(
    task_id: &str,
    step_index: u32,
    event_type: TraceEventType,
    tool_name: Option<&str>,
    title: &str,
    input: Option<serde_json::Value>,
    output: Option<serde_json::Value>,
    output_summary: Option<String>,
    duration_ms: u64,
) -> ToolTraceEvent {
    tool_event(
        task_id,
        step_index,
        event_type,
        tool_name.map(str::to_string),
        title.to_string(),
        input,
        output,
        output_summary,
        TraceStatus::Success,
        duration_ms,
    )
}

pub fn tool_event(
    task_id: &str,
    step_index: u32,
    event_type: TraceEventType,
    tool_name: Option<String>,
    title: String,
    input: Option<serde_json::Value>,
    output: Option<serde_json::Value>,
    output_summary: Option<String>,
    status: TraceStatus,
    duration_ms: u64,
) -> ToolTraceEvent {
    let started_at = Utc::now().to_rfc3339();
    ToolTraceEvent {
        id: Uuid::new_v4().to_string(),
        task_id: task_id.to_string(),
        step_index,
        event_type,
        tool_name,
        title,
        input,
        output,
        output_summary,
        started_at: started_at.clone(),
        ended_at: Some(started_at),
        duration_ms: Some(duration_ms),
        status,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn mock_code_link_prefers_cpp_files() {
        let root = create_temp_project();
        let cs_file = root.join("Source").join("Project.Target.cs");
        let cpp_file = root
            .join("Source")
            .join("Project")
            .join("Private")
            .join("Project.cpp");
        fs::create_dir_all(cpp_file.parent().unwrap()).unwrap();
        fs::write(&cs_file, "using UnrealBuildTool;\n").unwrap();
        fs::write(&cpp_file, "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n").unwrap();

        let project = ProjectSession {
            id: "project".to_string(),
            name: "Project".to_string(),
            repo_root: root.to_string_lossy().to_string(),
            solution_path: root.join("Project.sln").to_string_lossy().to_string(),
            uproject_path: None,
            build_command: None,
            vs_process_id: None,
            vs_bridge_endpoint: None,
            created_at: "now".to_string(),
            updated_at: "now".to_string(),
        };

        let sample = find_sample_code_link(&project).unwrap();
        assert_eq!(sample.code_link, "Source/Project/Private/Project.cpp:10");
        assert_eq!(
            sample.relative_path,
            "Source\\Project\\Private\\Project.cpp"
        );
        assert_eq!(sample.line, 10);
    }

    #[test]
    fn mock_code_link_does_not_fall_back_to_cs_files() {
        let root = create_temp_project();
        let cs_file = root.join("Source").join("Project.Target.cs");
        fs::write(&cs_file, "using UnrealBuildTool;\n").unwrap();
        let project = test_project(&root);

        assert!(find_sample_code_link(&project).is_none());
    }

    #[test]
    fn mock_agent_trace_contains_clickable_model_message() {
        let root = create_temp_project();
        let cpp_file = root
            .join("Source")
            .join("Project")
            .join("Private")
            .join("Test.cpp");
        fs::create_dir_all(cpp_file.parent().unwrap()).unwrap();
        fs::write(&cpp_file, "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n").unwrap();
        let project = test_project(&root);

        let run = create_mock_agent_run(&project, "Check code");

        assert_eq!(run.traces.len(), 5);
        assert_eq!(run.traces[0].title, "Start task");
        assert_eq!(run.traces[1].title, "list_files");
        assert_eq!(run.traces[2].title, "read_file");
        assert!(matches!(
            run.traces[3].event_type,
            TraceEventType::ModelMessage
        ));
        assert_eq!(run.traces[3].title, "model_message");
        assert_eq!(run.traces[4].title, "open_code_link suggestion");
        assert_eq!(
            run.traces[3].output_summary.as_deref(),
            Some("Suggested edit Source/Project/Private/Test.cpp:10")
        );
    }

    fn create_temp_project() -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("snowagent-trace-test-{unique}"));
        fs::create_dir_all(root.join("Source")).unwrap();
        root
    }

    fn test_project(root: &Path) -> ProjectSession {
        ProjectSession {
            id: "project".to_string(),
            name: "Project".to_string(),
            repo_root: root.to_string_lossy().to_string(),
            solution_path: root.join("Project.sln").to_string_lossy().to_string(),
            uproject_path: None,
            build_command: None,
            vs_process_id: None,
            vs_bridge_endpoint: None,
            created_at: "now".to_string(),
            updated_at: "now".to_string(),
        }
    }
}
