use std::sync::Arc;
use std::time::Instant;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::agent_runner::{run_agent, AgentRunInput};
use crate::mcp_runtime::McpRuntime;
use crate::project_registry::ProjectSession;
use crate::tool_interface::ToolOutput;
use crate::tool_registry::{
    WriteScope, AGENT_LIST_TOOL_NAME, AGENT_SPAWN_TOOL_NAME, AGENT_WAIT_TOOL_NAME,
};
use crate::tool_trace::{
    self, MockAgentRun, SubagentTraceRun, ToolTraceEvent, TraceEventType, TraceStatus,
};
use crate::vs_registry::AppSettings;

const MAX_SUBAGENT_THREADS: usize = 6;

pub struct SubagentManager {
    parent_task_id: String,
    project: ProjectSession,
    settings: AppSettings,
    provider_id: Option<String>,
    credential_id: Option<String>,
    model_id: Option<String>,
    reasoning_effort: Option<String>,
    mcp_runtime: Option<Arc<Mutex<McpRuntime>>>,
    allow_write_subagents: bool,
    children: Vec<SubagentSlot>,
}

struct SubagentSlot {
    task_id: String,
    agent_name: String,
    task_name: String,
    read_only: bool,
    write_scope: Option<WriteScope>,
    status: SubagentStatus,
    summary: Option<String>,
    handle: Option<tauri::async_runtime::JoinHandle<Result<MockAgentRun, String>>>,
    trace_run: Option<SubagentTraceRun>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SubagentStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpawnArgs {
    task_name: String,
    message: String,
    #[serde(default)]
    agent_kind: Option<String>,
    #[serde(default)]
    model_id: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default = "default_true")]
    read_only: bool,
    #[serde(default)]
    allowed_write_paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WaitArgs {
    #[serde(default)]
    child_task_ids: Vec<String>,
}

impl SubagentManager {
    pub fn new(
        parent_task_id: String,
        project: ProjectSession,
        settings: AppSettings,
        provider_id: Option<String>,
        credential_id: Option<String>,
        model_id: Option<String>,
        reasoning_effort: Option<String>,
        mcp_runtime: Option<Arc<Mutex<McpRuntime>>>,
        allow_write_subagents: bool,
    ) -> Self {
        Self {
            parent_task_id,
            project,
            settings,
            provider_id,
            credential_id,
            model_id,
            reasoning_effort,
            mcp_runtime,
            allow_write_subagents,
            children: Vec::new(),
        }
    }

    pub async fn execute_tool(&mut self, tool_name: &str, arguments: &Value) -> Option<ToolOutput> {
        match tool_name {
            AGENT_SPAWN_TOOL_NAME => Some(self.spawn(arguments)),
            AGENT_WAIT_TOOL_NAME => Some(self.wait(arguments).await),
            AGENT_LIST_TOOL_NAME => Some(self.list()),
            _ => None,
        }
    }

    pub fn into_trace_runs(self) -> Vec<SubagentTraceRun> {
        self.children
            .into_iter()
            .filter_map(|child| child.trace_run)
            .collect()
    }

    pub async fn finish_all(&mut self) {
        for index in 0..self.children.len() {
            self.await_child(index).await;
        }
    }

    fn spawn(&mut self, arguments: &Value) -> ToolOutput {
        let started = Instant::now();
        let args = match serde_json::from_value::<SpawnArgs>(arguments.clone()) {
            Ok(args) => args,
            Err(error) => {
                return ToolOutput::error(format!("agent/spawn invalid arguments: {error}"), 0)
            }
        };
        if !args.read_only && !self.allow_write_subagents {
            return ToolOutput::rejected(
                "agent/spawn rejected: parent run is read-only, so write-capable subagents are not available"
                    .to_string(),
            );
        }
        if self.children.len() >= MAX_SUBAGENT_THREADS {
            return ToolOutput::rejected(format!(
                "agent/spawn rejected: max subagent count is {MAX_SUBAGENT_THREADS}"
            ));
        }

        let read_only = args.read_only;
        let write_scope = if read_only {
            None
        } else {
            match WriteScope::from_allowed_paths(&self.project.repo_root, &args.allowed_write_paths)
            {
                Ok(scope) => {
                    if let Some(conflict) = self.write_scope_conflict(&scope) {
                        return ToolOutput::rejected(format!(
                            "agent/spawn rejected: write scope conflicts with running subagent `{}` ({}) on {}",
                            conflict.task_name,
                            conflict.task_id,
                            conflict
                                .write_scope
                                .as_ref()
                                .map(|scope| scope.paths().join(", "))
                                .unwrap_or_else(|| "unknown scope".to_string())
                        ));
                    }
                    Some(scope)
                }
                Err(error) => {
                    return ToolOutput::rejected(format!(
                        "agent/spawn rejected: invalid allowedWritePaths: {error}"
                    ));
                }
            }
        };

        let task_name = non_empty_or(args.task_name, "subagent-task");
        let agent_name = non_empty_or(args.agent_kind.unwrap_or_default(), "explorer");
        let child_task_id = Uuid::new_v4().to_string();
        let child_prompt = build_child_prompt(
            &agent_name,
            &task_name,
            &args.message,
            read_only,
            write_scope.as_ref(),
        );
        let child_input = AgentRunInput {
            project_id: self.project.id.clone(),
            session_id: None,
            task_id: Some(child_task_id.clone()),
            user_prompt: child_prompt,
            messages: None,
            provider_id: self.provider_id.clone(),
            credential_id: self.credential_id.clone(),
            model_id: non_empty(args.model_id).or_else(|| self.model_id.clone()),
            reasoning_effort: non_empty(args.reasoning_effort)
                .or_else(|| self.reasoning_effort.clone()),
            allow_shell: false,
            assume_yes: false,
            cli_mode: false,
            goal: None,
            parent_task_id: Some(self.parent_task_id.clone()),
            agent_name: Some(agent_name.clone()),
            task_name: Some(task_name.clone()),
            read_only,
            subagent_depth: 1,
            goal_slot: None,
            mcp_runtime: self.mcp_runtime.clone(),
            write_scope: write_scope.clone(),
        };
        let project = self.project.clone();
        let settings = self.settings.clone();
        let handle = tauri::async_runtime::spawn(async move {
            let mut run = run_agent(&project, &settings, child_input, |_event| {}).await?;
            run.subagent_runs.clear();
            Ok(run)
        });

        self.children.push(SubagentSlot {
            task_id: child_task_id.clone(),
            agent_name: agent_name.clone(),
            task_name: task_name.clone(),
            read_only,
            write_scope: write_scope.clone(),
            status: SubagentStatus::Running,
            summary: None,
            handle: Some(handle),
            trace_run: None,
        });

        ToolOutput::ok_with_summary(
            json!({
                "childTaskId": child_task_id,
                "childRunId": child_task_id,
                "parentTaskId": self.parent_task_id.clone(),
                "agentName": agent_name,
                "taskName": task_name,
                "readOnly": read_only,
                "allowedWritePaths": write_scope
                    .as_ref()
                    .map(|scope| scope.paths().to_vec())
                    .unwrap_or_default(),
                "status": "running"
            }),
            started.elapsed().as_millis() as u64,
            format!(
                "Spawned {} subagent {task_name}",
                if read_only {
                    "read-only"
                } else {
                    "write-capable"
                }
            ),
        )
    }

    async fn wait(&mut self, arguments: &Value) -> ToolOutput {
        let started = Instant::now();
        let args = match serde_json::from_value::<WaitArgs>(arguments.clone()) {
            Ok(args) => args,
            Err(error) => {
                return ToolOutput::error(format!("agent/wait invalid arguments: {error}"), 0)
            }
        };
        let indices = self.selected_child_indices(&args.child_task_ids);
        if !args.child_task_ids.is_empty() && indices.len() != args.child_task_ids.len() {
            return ToolOutput::error(
                "agent/wait referenced an unknown childTaskId".to_string(),
                0,
            );
        }

        for index in indices.iter().copied() {
            self.await_child(index).await;
        }

        let subagents = indices
            .iter()
            .map(|index| self.child_output(*index))
            .collect::<Vec<_>>();
        let completed_count = subagents
            .iter()
            .filter(|value| value.get("status").and_then(Value::as_str) == Some("completed"))
            .count();
        let failed_count = subagents
            .iter()
            .filter(|value| value.get("status").and_then(Value::as_str) == Some("failed"))
            .count();

        ToolOutput::ok_with_summary(
            json!({
                "parentTaskId": self.parent_task_id.clone(),
                "subagents": subagents,
                "completedCount": completed_count,
                "failedCount": failed_count
            }),
            started.elapsed().as_millis() as u64,
            format!("Collected {completed_count} subagent summary(s), {failed_count} failed"),
        )
    }

    fn list(&self) -> ToolOutput {
        ToolOutput::ok(
            json!({
                "parentTaskId": self.parent_task_id.clone(),
                "maxSubagents": MAX_SUBAGENT_THREADS,
                "subagents": self.children
                    .iter()
                    .enumerate()
                    .map(|(index, _)| self.child_output(index))
                    .collect::<Vec<_>>()
            }),
            0,
        )
    }

    fn write_scope_conflict(&self, requested_scope: &WriteScope) -> Option<&SubagentSlot> {
        self.children.iter().find(|child| {
            matches!(child.status, SubagentStatus::Running)
                && child
                    .write_scope
                    .as_ref()
                    .is_some_and(|scope| scope.overlaps(requested_scope))
        })
    }

    fn selected_child_indices(&self, requested: &[String]) -> Vec<usize> {
        if requested.is_empty() {
            return (0..self.children.len()).collect();
        }
        requested
            .iter()
            .filter_map(|task_id| {
                self.children
                    .iter()
                    .position(|child| child.task_id == *task_id)
            })
            .collect()
    }

    async fn await_child(&mut self, index: usize) {
        let Some(handle) = self.children[index].handle.take() else {
            return;
        };
        let outcome = match handle.await {
            Ok(result) => result,
            Err(error) => Err(format!("subagent join failed: {error}")),
        };
        self.finish_child(index, outcome);
    }

    fn finish_child(&mut self, index: usize, outcome: Result<MockAgentRun, String>) {
        match outcome {
            Ok(mut run) => {
                annotate_child_traces(
                    &mut run.traces,
                    &self.parent_task_id,
                    &self.children[index].agent_name,
                    &self.children[index].task_name,
                    self.children[index].read_only,
                );
                let status = infer_child_status(&run.traces);
                let summary = child_summary(&run.traces);
                self.children[index].status = status;
                self.children[index].summary = summary.clone();
                self.children[index].trace_run = Some(SubagentTraceRun {
                    task_id: run.task_id,
                    parent_task_id: self.parent_task_id.clone(),
                    agent_name: self.children[index].agent_name.clone(),
                    task_name: self.children[index].task_name.clone(),
                    read_only: self.children[index].read_only,
                    subagent_depth: 1,
                    status: status.as_str().to_string(),
                    summary,
                    traces: run.traces,
                });
            }
            Err(error) => {
                let mut traces = vec![tool_trace::tool_event(
                    &self.children[index].task_id,
                    1,
                    TraceEventType::Error,
                    Some(AGENT_WAIT_TOOL_NAME.to_string()),
                    "subagent_failed".to_string(),
                    Some(json!({
                        "parentTaskId": self.parent_task_id.clone(),
                        "taskName": self.children[index].task_name.clone(),
                    })),
                    Some(json!({ "error": error.clone() })),
                    Some(error.clone()),
                    TraceStatus::Failed,
                    0,
                )];
                annotate_child_traces(
                    &mut traces,
                    &self.parent_task_id,
                    &self.children[index].agent_name,
                    &self.children[index].task_name,
                    self.children[index].read_only,
                );
                self.children[index].status = SubagentStatus::Failed;
                self.children[index].summary = Some(error.clone());
                self.children[index].trace_run = Some(SubagentTraceRun {
                    task_id: self.children[index].task_id.clone(),
                    parent_task_id: self.parent_task_id.clone(),
                    agent_name: self.children[index].agent_name.clone(),
                    task_name: self.children[index].task_name.clone(),
                    read_only: self.children[index].read_only,
                    subagent_depth: 1,
                    status: "failed".to_string(),
                    summary: Some(error),
                    traces,
                });
            }
        }
    }

    fn child_output(&self, index: usize) -> Value {
        let child = &self.children[index];
        json!({
            "childTaskId": child.task_id.clone(),
            "childRunId": child.task_id.clone(),
            "parentTaskId": self.parent_task_id.clone(),
            "agentName": child.agent_name.clone(),
            "taskName": child.task_name.clone(),
            "readOnly": child.read_only,
            "allowedWritePaths": child
                .write_scope
                .as_ref()
                .map(|scope| scope.paths().to_vec())
                .unwrap_or_default(),
            "status": child.status.as_str(),
            "summary": child.summary.clone(),
            "traceCount": child.trace_run.as_ref().map(|run| run.traces.len()).unwrap_or(0)
        })
    }
}

impl SubagentStatus {
    fn as_str(self) -> &'static str {
        match self {
            SubagentStatus::Running => "running",
            SubagentStatus::Completed => "completed",
            SubagentStatus::Failed => "failed",
        }
    }
}

fn default_true() -> bool {
    true
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn non_empty_or(value: String, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

fn build_child_prompt(
    agent_name: &str,
    task_name: &str,
    message: &str,
    read_only: bool,
    write_scope: Option<&WriteScope>,
) -> String {
    let capability = if read_only {
        "read-only"
    } else {
        "write-capable"
    };
    let write_scope_text = write_scope
        .map(|scope| scope.paths().join(", "))
        .unwrap_or_else(|| "(none)".to_string());
    let rules = if read_only {
        "- Only inspect the workspace and Visual Studio context.\n\
         - Do not edit files, apply patches, run shell commands, install packages, or spawn subagents."
            .to_string()
    } else {
        format!(
            "- You may edit workspace files when needed for the assigned task.\n\
         - You may write only inside this assigned write scope: {write_scope_text}.\n\
         - Do not change paths outside that assigned write scope.\n\
         - Keep edits strictly inside the assigned scope and avoid unrelated cleanup.\n\
         - Do not run shell commands, install packages, or spawn subagents.\n\
         - Report changed files and verification evidence in the final summary."
        )
    };
    format!(
        "You are a CodeForge {capability} subagent.\n\
         Agent: {agent_name}\n\
         Task: {task_name}\n\n\
         Rules:\n\
         {rules}\n\
         - Keep intermediate noise out of the final answer.\n\
         - Return a concise summary with concrete file references when available.\n\n\
         Assigned task:\n{message}"
    )
}

fn annotate_child_traces(
    traces: &mut [ToolTraceEvent],
    parent_task_id: &str,
    agent_name: &str,
    task_name: &str,
    read_only: bool,
) {
    for event in traces {
        event.parent_task_id = Some(parent_task_id.to_string());
        event.agent_name = Some(agent_name.to_string());
        event.task_name = Some(task_name.to_string());
        event.read_only = Some(read_only);
        event.subagent_depth = Some(1);
    }
}

fn infer_child_status(traces: &[ToolTraceEvent]) -> SubagentStatus {
    if traces
        .iter()
        .any(|event| matches!(event.status, TraceStatus::Failed))
    {
        return SubagentStatus::Failed;
    }
    if traces
        .iter()
        .any(|event| matches!(event.event_type, TraceEventType::FinalResponse))
    {
        return SubagentStatus::Completed;
    }
    SubagentStatus::Completed
}

fn child_summary(traces: &[ToolTraceEvent]) -> Option<String> {
    traces
        .iter()
        .rev()
        .find(|event| matches!(event.event_type, TraceEventType::FinalResponse))
        .and_then(|event| event.output_summary.clone())
        .or_else(|| {
            traces
                .iter()
                .rev()
                .find_map(|event| event.output_summary.clone())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_interface::ToolOutputStatus;
    use std::fs;

    fn test_project(root: &str) -> ProjectSession {
        ProjectSession {
            id: "project".to_string(),
            name: "Project".to_string(),
            repo_root: root.to_string(),
            solution_path: None,
            uproject_path: None,
            build_command: None,
            vs_process_id: None,
            vs_bridge_endpoint: None,
            created_at: "now".to_string(),
            updated_at: "now".to_string(),
        }
    }

    fn test_workspace() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("codeforge-subagent-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn test_manager(root: &str) -> SubagentManager {
        SubagentManager::new(
            "parent".to_string(),
            test_project(root),
            AppSettings::default(),
            None,
            None,
            None,
            None,
            None,
            true,
        )
    }

    #[test]
    fn write_capable_child_prompt_allows_workspace_edits() {
        let allowed = vec!["src/App.css".to_string()];
        let scope = WriteScope::from_allowed_paths("D:\\code\\snowAgents", &allowed).unwrap();
        let prompt = build_child_prompt(
            "worker",
            "fix-ui",
            "Update the composer.",
            false,
            Some(&scope),
        );

        assert!(prompt.contains("CodeForge write-capable subagent"));
        assert!(prompt.contains("You may edit workspace files"));
        assert!(prompt.contains("src/app.css"));
        assert!(prompt.contains("Do not change paths outside that assigned write scope"));
        assert!(!prompt.contains("Do not edit files"));
    }

    #[test]
    fn read_only_child_prompt_rejects_file_edits() {
        let prompt =
            build_child_prompt("reviewer", "review-ui", "Inspect the composer.", true, None);

        assert!(prompt.contains("CodeForge read-only subagent"));
        assert!(prompt.contains("Only inspect the workspace"));
        assert!(prompt.contains("Do not edit files"));
    }

    #[test]
    fn write_capable_spawn_requires_allowed_write_paths() {
        let root = test_workspace();
        let mut manager = test_manager(root.to_str().unwrap());

        let output = manager.spawn(&json!({
            "taskName": "write-ui",
            "message": "Update UI.",
            "readOnly": false
        }));

        assert_eq!(output.status, ToolOutputStatus::Rejected);
        assert!(output
            .error
            .unwrap()
            .contains("allowedWritePaths must include at least one workspace path"));
    }

    #[test]
    fn write_capable_spawn_rejects_running_scope_conflict() {
        let root = test_workspace();
        let root_text = root.to_string_lossy().to_string();
        let mut manager = test_manager(&root_text);
        let existing_paths = vec!["src/App.css".to_string()];
        let existing_scope = WriteScope::from_allowed_paths(&root_text, &existing_paths).unwrap();
        manager.children.push(SubagentSlot {
            task_id: "child-1".to_string(),
            agent_name: "worker".to_string(),
            task_name: "first-ui".to_string(),
            read_only: false,
            write_scope: Some(existing_scope),
            status: SubagentStatus::Running,
            summary: None,
            handle: None,
            trace_run: None,
        });

        let output = manager.spawn(&json!({
            "taskName": "second-ui",
            "message": "Update UI too.",
            "readOnly": false,
            "allowedWritePaths": ["src/App.css"]
        }));

        assert_eq!(output.status, ToolOutputStatus::Rejected);
        assert!(output.error.unwrap().contains("write scope conflicts"));
    }
}
