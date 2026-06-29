use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use codeforge_core::office_tools;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::goal_state::GoalState;
use crate::mcp_runtime::{
    is_mcp_management_tool, McpRuntime, MCP_CONNECT_SERVER_TOOL_NAME,
    MCP_DISCONNECT_SERVER_TOOL_NAME, MCP_LIST_SERVERS_TOOL_NAME, MCP_LIST_TOOLS_TOOL_NAME,
};
use crate::tool_interface::ToolOutput;
use crate::vs_bridge_client;
use crate::workspace_tools;

pub const CALCULATOR_ADD_TOOL_NAME: &str = "calculator.add";
pub const LIST_DIR_TOOL_NAME: &str = "list_dir";
pub const WORKSPACE_LIST_DIR_TOOL_NAME: &str = "workspace/list_dir";
pub const READ_FILE_TOOL_NAME: &str = "read_file";
pub const WORKSPACE_READ_FILE_TOOL_NAME: &str = "workspace/read_file";
pub const SEARCH_FILE_TOOL_NAME: &str = "search_file";
pub const WORKSPACE_SEARCH_FILE_TOOL_NAME: &str = "workspace/search_file";
pub const SEARCH_CONTENT_TOOL_NAME: &str = "search_content";
pub const WORKSPACE_SEARCH_CONTENT_TOOL_NAME: &str = "workspace/search";
pub const WORKSPACE_SEARCH_CONTENT_ALIAS_TOOL_NAME: &str = "workspace/search_content";
pub const EDIT_FILE_TOOL_NAME: &str = "edit_file";
pub const WORKSPACE_EDIT_FILE_TOOL_NAME: &str = "workspace/edit_file";
pub const WRITE_FILE_TOOL_NAME: &str = "write_file";
pub const WORKSPACE_WRITE_FILE_TOOL_NAME: &str = "workspace/write_file";
pub const SHELL_COMMAND_TOOL_NAME: &str = "shell_command";
pub const WORKSPACE_SHELL_COMMAND_TOOL_NAME: &str = "workspace/shell_command";
pub const APPLY_PATCH_RAW_TOOL_NAME: &str = "apply_patch_raw";
pub const GIT_STATUS_TOOL_NAME: &str = "git/status";
pub const GIT_DIFF_TOOL_NAME: &str = "git/diff";
pub const GIT_LOG_TOOL_NAME: &str = "git/log";
pub const GIT_SHOW_TOOL_NAME: &str = "git/show";
pub const GIT_ADD_TOOL_NAME: &str = "git/add";
pub const GIT_RESET_TOOL_NAME: &str = "git/reset";
pub const GIT_COMMIT_TOOL_NAME: &str = "git/commit";
pub const GET_FILE_CONTEXT_TOOL_NAME: &str = "get_file_context";
pub const WORKSPACE_GET_FILE_CONTEXT_TOOL_NAME: &str = "workspace/get_file_context";
pub const DOCUMENT_READ_DOCX_TOOL_NAME: &str = "document/read_docx";
pub const PRESENTATION_READ_PPTX_TOOL_NAME: &str = "presentation/read_pptx";
pub const VS_CURRENT_SOLUTION_TOOL_NAME: &str = "vs.current_solution";
pub const VS_CURRENT_DOCUMENT_TOOL_NAME: &str = "vs.current_document";
pub const VS_CURRENT_SELECTION_TOOL_NAME: &str = "vs.current_selection";
pub const VS_LIST_PROJECTS_TOOL_NAME: &str = "vs.list_projects";
pub const VS_LIST_PROJECT_FILES_TOOL_NAME: &str = "vs.list_project_files";
pub const VS_SEARCH_FILE_TOOL_NAME: &str = "vs.search_file";
pub const VS_SEARCH_CONTENT_TOOL_NAME: &str = "vs.search";
pub const VS_FIND_SYMBOL_TOOL_NAME: &str = "vs.find_symbol";
pub const VS_FIND_REFERENCES_TOOL_NAME: &str = "vs.find_references";
pub const VS_GO_TO_DEFINITION_TOOL_NAME: &str = "vs.go_to_definition";
pub const VS_GET_ERROR_LIST_TOOL_NAME: &str = "vs.get_error_list";
pub const GOAL_GET_TOOL_NAME: &str = "goal/get";
pub const GOAL_SET_TOOL_NAME: &str = "goal/set";
pub const GOAL_CLEAR_TOOL_NAME: &str = "goal/clear";
pub const AGENT_SPAWN_TOOL_NAME: &str = "agent/spawn";
pub const AGENT_WAIT_TOOL_NAME: &str = "agent/wait";
pub const AGENT_LIST_TOOL_NAME: &str = "agent/list";
pub const PROGRESS_UPDATE_STEPS_TOOL_NAME: &str = "progress/update_steps";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteScope {
    paths: Vec<String>,
}

impl WriteScope {
    pub fn from_allowed_paths(workspace_root: &str, paths: &[String]) -> Result<Self, String> {
        let mut normalized = paths
            .iter()
            .map(|path| normalize_write_target_path(workspace_root, path))
            .collect::<Result<Vec<_>, _>>()?;
        normalized.sort();
        normalized.dedup();
        if normalized.is_empty() {
            return Err("allowedWritePaths must include at least one workspace path".to_string());
        }
        Ok(Self { paths: normalized })
    }

    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    pub fn allows(&self, workspace_root: &str, raw_path: &str) -> Result<bool, String> {
        let target = normalize_write_target_path(workspace_root, raw_path)?;
        Ok(self
            .paths
            .iter()
            .any(|allowed| path_is_within(&target, allowed)))
    }

    pub fn overlaps(&self, other: &WriteScope) -> bool {
        self.paths.iter().any(|left| {
            other
                .paths
                .iter()
                .any(|right| write_paths_overlap(left, right))
        })
    }
}

pub struct ToolExecutionContext<'a> {
    pub workspace_root: &'a str,
    pub vs_bridge_endpoint: Option<&'a str>,
    pub allow_shell: bool,
    pub assume_yes: bool,
    pub cli_mode: bool,
    pub goal: Option<&'a mut Option<GoalState>>,
    pub mcp_runtime: Option<Arc<Mutex<McpRuntime>>>,
    pub write_scope: Option<&'a WriteScope>,
}

fn workspace_namespace_aliases() -> Vec<Value> {
    vec![
        workspace_alias(WORKSPACE_LIST_DIR_TOOL_NAME, &list_dir_definition()),
        workspace_alias(WORKSPACE_READ_FILE_TOOL_NAME, &read_file_definition()),
        workspace_alias(WORKSPACE_SEARCH_FILE_TOOL_NAME, &search_file_definition()),
        workspace_alias(
            WORKSPACE_SEARCH_CONTENT_TOOL_NAME,
            &search_content_definition(),
        ),
        workspace_alias(WORKSPACE_EDIT_FILE_TOOL_NAME, &edit_file_definition()),
        workspace_alias(WORKSPACE_WRITE_FILE_TOOL_NAME, &write_file_definition()),
        workspace_alias(
            WORKSPACE_GET_FILE_CONTEXT_TOOL_NAME,
            &get_file_context_definition(),
        ),
    ]
}

/// Clone a tool definition and rename its function name to the given namespace
/// alias. The parameters and description are preserved so the model sees the
/// same schema.
fn workspace_alias(name: &str, definition: &Value) -> Value {
    let mut cloned = definition.clone();
    if let Some(function) = cloned.get_mut("function").and_then(|v| v.as_object_mut()) {
        function.insert("name".to_string(), Value::String(name.to_string()));
    }
    cloned
}
pub fn tool_definitions() -> Vec<Value> {
    let mut tools = vs_tool_definitions();
    tools.extend(workspace_tool_definitions());
    tools.extend(git_tool_definitions());
    tools.extend([
        list_dir_definition(),
        get_file_context_definition(),
        document_read_docx_definition(),
        presentation_read_pptx_definition(),
        goal_get_definition(),
        goal_set_definition(),
        goal_clear_definition(),
        progress_update_steps_definition(),
    ]);
    tools.extend(mcp_management_tool_definitions());
    tools.extend(workspace_namespace_aliases());
    tools
}

pub fn read_only_tool_definitions() -> Vec<Value> {
    let mut tools = vs_tool_definitions();
    tools.extend(read_only_workspace_tool_definitions());
    tools.extend(read_only_git_tool_definitions());
    tools.extend([
        list_dir_definition(),
        get_file_context_definition(),
        document_read_docx_definition(),
        presentation_read_pptx_definition(),
        goal_get_definition(),
        progress_update_steps_definition(),
    ]);
    tools.extend(mcp_management_tool_definitions());
    tools.extend(read_only_workspace_namespace_aliases());
    tools
}

pub fn agent_tool_definitions() -> Vec<Value> {
    vec![
        agent_spawn_definition(),
        agent_wait_definition(),
        agent_list_definition(),
    ]
}

pub fn tool_call_test_definitions() -> Vec<Value> {
    vec![calculator_add_definition()]
}

pub fn cli_tool_definitions(
    provider_type: &str,
    model_id: &str,
    shell_enabled: bool,
) -> Vec<Value> {
    let mut tools = workspace_tool_definitions();
    tools.extend(git_tool_definitions());
    tools.extend(workspace_namespace_aliases());
    if shell_enabled {
        tools.push(shell_command_definition());
    }
    if exposes_apply_patch_raw(provider_type, model_id) {
        tools.push(apply_patch_raw_definition());
    }
    tools
}

fn workspace_tool_definitions() -> Vec<Value> {
    vec![
        read_file_definition(),
        search_file_definition(),
        search_content_definition(),
        edit_file_definition(),
        write_file_definition(),
    ]
}

fn read_only_workspace_tool_definitions() -> Vec<Value> {
    vec![
        read_file_definition(),
        search_file_definition(),
        search_content_definition(),
    ]
}

fn vs_tool_definitions() -> Vec<Value> {
    vec![
        vs_current_solution_definition(),
        vs_current_document_definition(),
        vs_current_selection_definition(),
        vs_list_projects_definition(),
        vs_list_project_files_definition(),
        vs_search_file_definition(),
        vs_search_content_definition(),
        vs_find_symbol_definition(),
        vs_find_references_definition(),
        vs_go_to_definition_definition(),
        vs_get_error_list_definition(),
    ]
}

fn read_only_git_tool_definitions() -> Vec<Value> {
    vec![
        git_status_definition(),
        git_diff_definition(),
        git_log_definition(),
        git_show_definition(),
    ]
}

fn git_tool_definitions() -> Vec<Value> {
    let mut tools = read_only_git_tool_definitions();
    tools.extend([
        git_add_definition(),
        git_reset_definition(),
        git_commit_definition(),
    ]);
    tools
}

pub fn mcp_management_tool_definitions() -> Vec<Value> {
    vec![
        mcp_list_servers_definition(),
        mcp_connect_server_definition(),
        mcp_disconnect_server_definition(),
        mcp_list_tools_definition(),
    ]
}

fn read_only_workspace_namespace_aliases() -> Vec<Value> {
    vec![
        workspace_alias(WORKSPACE_LIST_DIR_TOOL_NAME, &list_dir_definition()),
        workspace_alias(WORKSPACE_READ_FILE_TOOL_NAME, &read_file_definition()),
        workspace_alias(WORKSPACE_SEARCH_FILE_TOOL_NAME, &search_file_definition()),
        workspace_alias(
            WORKSPACE_SEARCH_CONTENT_TOOL_NAME,
            &search_content_definition(),
        ),
        workspace_alias(
            WORKSPACE_GET_FILE_CONTEXT_TOOL_NAME,
            &get_file_context_definition(),
        ),
    ]
}

pub fn exposes_apply_patch_raw(provider_type: &str, model_id: &str) -> bool {
    let provider = provider_type.to_ascii_lowercase();
    let model = model_id.to_ascii_lowercase();
    (provider.contains("openai") || provider.contains("codex") || model.contains("codex"))
        && !matches!(
            provider.as_str(),
            "minimax" | "deepseek" | "glm" | "codebuddy"
        )
}

fn write_tool_target(arguments: &Value) -> Result<String, String> {
    arguments
        .get("file")
        .or_else(|| arguments.get("path"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "invalid_arguments: write tool requires `file`".to_string())
}

fn ensure_write_scope_allows(
    context: &ToolExecutionContext<'_>,
    name: &str,
    arguments: &Value,
) -> Result<(), String> {
    let Some(scope) = context.write_scope else {
        return Ok(());
    };
    let target = write_tool_target(arguments)?;
    if scope.allows(context.workspace_root, &target)? {
        return Ok(());
    }

    Err(format!(
        "rejected: {name} target `{target}` is outside this subagent write scope: {}",
        scope.paths().join(", ")
    ))
}

fn ensure_git_pathspec_scope_allows(
    context: &ToolExecutionContext<'_>,
    name: &str,
    arguments: &Value,
) -> Result<(), String> {
    let Some(scope) = context.write_scope else {
        return Ok(());
    };
    if arguments
        .get("all")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(format!(
            "rejected: {name} with all=true is outside this subagent write scope: {}",
            scope.paths().join(", ")
        ));
    }
    let pathspecs = git_pathspecs(arguments)?;
    if pathspecs.is_empty() {
        return Err(format!(
            "rejected: {name} requires explicit pathspecs inside this subagent write scope: {}",
            scope.paths().join(", ")
        ));
    }
    for pathspec in pathspecs {
        if git_pathspec_is_dynamic(&pathspec) {
            return Err(format!(
                "rejected: {name} pathspec `{pathspec}` is too broad for this subagent write scope"
            ));
        }
        if !scope.allows(context.workspace_root, &pathspec)? {
            return Err(format!(
                "rejected: {name} pathspec `{pathspec}` is outside this subagent write scope: {}",
                scope.paths().join(", ")
            ));
        }
    }
    Ok(())
}

fn ensure_git_commit_scope_allows(context: &ToolExecutionContext<'_>) -> Result<(), String> {
    let Some(scope) = context.write_scope else {
        return Ok(());
    };
    let staged_paths = workspace_tools::git_staged_paths(context.workspace_root)?;
    for path in staged_paths {
        if !scope.allows(context.workspace_root, &path)? {
            return Err(format!(
                "rejected: git/commit staged path `{path}` is outside this subagent write scope: {}",
                scope.paths().join(", ")
            ));
        }
    }
    Ok(())
}

fn git_pathspecs(arguments: &Value) -> Result<Vec<String>, String> {
    match arguments.get("pathspecs") {
        Some(Value::Null) | None => Ok(Vec::new()),
        Some(Value::String(value)) => Ok(vec![value.trim().to_string()]
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| {
                        "invalid_arguments: `pathspecs` entries must be strings".to_string()
                    })
            })
            .collect(),
        _ => Err("invalid_arguments: `pathspecs` must be a string or array of strings".to_string()),
    }
}

fn git_pathspec_is_dynamic(pathspec: &str) -> bool {
    let pathspec = pathspec.trim();
    pathspec.starts_with(':')
        || pathspec.contains('*')
        || pathspec.contains('?')
        || pathspec.contains('[')
        || pathspec.contains(']')
}

fn normalize_write_target_path(workspace_root: &str, raw_path: &str) -> Result<String, String> {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() {
        return Err("invalid_arguments: write path must not be empty".to_string());
    }

    let workspace = clean_path(&PathBuf::from(workspace_root));
    let raw = Path::new(trimmed);
    let absolute = if raw.is_absolute() {
        clean_path(raw)
    } else {
        clean_path(&workspace.join(raw))
    };
    if !absolute.starts_with(&workspace) {
        return Err(format!(
            "invalid_arguments: write path `{trimmed}` must stay inside the workspace"
        ));
    }
    let relative = absolute.strip_prefix(&workspace).map_err(|_| {
        format!("invalid_arguments: write path `{trimmed}` must stay inside the workspace")
    })?;
    let normalized = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().replace('\\', "/")),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    if normalized.is_empty() {
        return Err("invalid_arguments: write path cannot be the workspace root".to_string());
    }
    Ok(write_path_key(&normalized))
}

fn clean_path(path: &Path) -> PathBuf {
    let mut cleaned = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                cleaned.pop();
            }
            Component::Normal(_) | Component::RootDir | Component::Prefix(_) => {
                cleaned.push(component.as_os_str());
            }
        }
    }
    cleaned
}

fn write_path_key(path: &str) -> String {
    let normalized = path.replace('\\', "/").trim_matches('/').to_string();
    if cfg!(windows) {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

fn write_paths_overlap(left: &str, right: &str) -> bool {
    path_is_within(left, right) || path_is_within(right, left)
}

fn path_is_within(path: &str, scope: &str) -> bool {
    path == scope
        || path
            .strip_prefix(scope)
            .is_some_and(|rest| rest.starts_with('/'))
}

pub async fn execute_tool(
    context: &mut ToolExecutionContext<'_>,
    name: &str,
    arguments: &Value,
) -> Result<Value, String> {
    let result = execute_tool_result(context, name, arguments).await;
    if result.is_ok() {
        Ok(result.output.unwrap_or(Value::Null))
    } else {
        Err(result
            .error
            .unwrap_or_else(|| format!("tool failed: {name}")))
    }
}

pub async fn execute_tool_result(
    context: &mut ToolExecutionContext<'_>,
    name: &str,
    arguments: &Value,
) -> ToolOutput {
    if let Some(runtime) = context.mcp_runtime.clone() {
        let mut runtime = runtime.lock().await;
        if is_mcp_management_tool(name) {
            return runtime.execute_management_tool(name, arguments).await;
        }
        if runtime.is_mcp_tool(name) {
            return runtime.call_tool(name, arguments).await;
        }
    } else if is_mcp_management_tool(name) || name.starts_with("mcp__") {
        return ToolOutput::error(
            "MCP runtime is not available for this agent run".to_string(),
            0,
        );
    }

    let started = Instant::now();
    match execute_tool_inner(context, name, arguments).await {
        Ok(output) => {
            let elapsed_ms = started.elapsed().as_millis() as u64;
            if let Some(summary) = recovered_read_file_summary(name, &output) {
                ToolOutput::ok_with_summary(output, elapsed_ms, summary)
            } else {
                ToolOutput::ok(output, elapsed_ms)
            }
        }
        Err(error) => {
            let elapsed_ms = started.elapsed().as_millis() as u64;
            if error.starts_with("rejected:") {
                ToolOutput::rejected(error)
            } else if error.starts_with("timeout:") {
                ToolOutput::timeout(elapsed_ms)
            } else {
                ToolOutput::error(error, elapsed_ms)
            }
        }
    }
}

fn recovered_read_file_summary(name: &str, output: &Value) -> Option<String> {
    if !matches!(name, READ_FILE_TOOL_NAME | WORKSPACE_READ_FILE_TOOL_NAME) {
        return None;
    }
    if output.get("recovered").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let recovery = output.get("recovery").and_then(Value::as_object)?;
    let requested = recovery
        .get("requestedPath")
        .and_then(Value::as_str)
        .unwrap_or("requested path");
    let resolved = recovery
        .get("resolvedPath")
        .and_then(Value::as_str)
        .or_else(|| output.get("file").and_then(Value::as_str))
        .unwrap_or("matching file");
    Some(format!(
        "recovered file_not_found: read {resolved} after missing {requested}"
    ))
}

fn tool_timeout(name: &str) -> Duration {
    match name {
        READ_FILE_TOOL_NAME => Duration::from_secs(10),
        SEARCH_CONTENT_TOOL_NAME | EDIT_FILE_TOOL_NAME | WRITE_FILE_TOOL_NAME => {
            Duration::from_secs(30)
        }
        SHELL_COMMAND_TOOL_NAME => Duration::from_secs(60),
        GIT_COMMIT_TOOL_NAME => Duration::from_secs(120),
        GIT_ADD_TOOL_NAME | GIT_RESET_TOOL_NAME => Duration::from_secs(60),
        _ => Duration::from_secs(30),
    }
}

async fn execute_tool_inner(
    context: &mut ToolExecutionContext<'_>,
    name: &str,
    arguments: &Value,
) -> Result<Value, String> {
    match name {
        CALCULATOR_ADD_TOOL_NAME => add(arguments),
        LIST_DIR_TOOL_NAME | WORKSPACE_LIST_DIR_TOOL_NAME => workspace_tools::list_dir(context.workspace_root, arguments),
        READ_FILE_TOOL_NAME | WORKSPACE_READ_FILE_TOOL_NAME => workspace_tools::read_file(context.workspace_root, arguments),
        SEARCH_FILE_TOOL_NAME | WORKSPACE_SEARCH_FILE_TOOL_NAME => execute_search_file(context, arguments).await,
        SEARCH_CONTENT_TOOL_NAME | WORKSPACE_SEARCH_CONTENT_TOOL_NAME | WORKSPACE_SEARCH_CONTENT_ALIAS_TOOL_NAME => execute_search_content(context, arguments).await,
        EDIT_FILE_TOOL_NAME | WORKSPACE_EDIT_FILE_TOOL_NAME => {
            ensure_write_scope_allows(context, name, arguments)?;
            workspace_tools::edit_file(context.workspace_root, arguments)
        },
        WRITE_FILE_TOOL_NAME | WORKSPACE_WRITE_FILE_TOOL_NAME => {
            ensure_write_scope_allows(context, name, arguments)?;
            workspace_tools::write_file(context.workspace_root, arguments)
        },
        GIT_STATUS_TOOL_NAME => workspace_tools::git_status(context.workspace_root, arguments),
        GIT_DIFF_TOOL_NAME => workspace_tools::git_diff(context.workspace_root, arguments),
        GIT_LOG_TOOL_NAME => workspace_tools::git_log(context.workspace_root, arguments),
        GIT_SHOW_TOOL_NAME => workspace_tools::git_show(context.workspace_root, arguments),
        GIT_ADD_TOOL_NAME => {
            ensure_git_pathspec_scope_allows(context, name, arguments)?;
            workspace_tools::git_add(context.workspace_root, arguments)
        },
        GIT_RESET_TOOL_NAME => {
            ensure_git_pathspec_scope_allows(context, name, arguments)?;
            workspace_tools::git_reset(context.workspace_root, arguments)
        },
        GIT_COMMIT_TOOL_NAME => {
            ensure_git_commit_scope_allows(context)?;
            workspace_tools::git_commit(context.workspace_root, arguments)
        },
        SHELL_COMMAND_TOOL_NAME | WORKSPACE_SHELL_COMMAND_TOOL_NAME => workspace_tools::shell_command(
            context.workspace_root,
            arguments,
            context.allow_shell,
            context.assume_yes,
        ).await,
        APPLY_PATCH_RAW_TOOL_NAME => Err("rejected: apply_patch_raw is reserved for compatible Codex/OpenAI adapters and is not implemented in the CLI runtime".to_string()),
        GET_FILE_CONTEXT_TOOL_NAME | WORKSPACE_GET_FILE_CONTEXT_TOOL_NAME => workspace_tools::get_file_context(context.workspace_root, arguments),
        DOCUMENT_READ_DOCX_TOOL_NAME => office_tools::read_docx(context.workspace_root, arguments),
        PRESENTATION_READ_PPTX_TOOL_NAME => office_tools::read_pptx(context.workspace_root, arguments),
        VS_CURRENT_SOLUTION_TOOL_NAME => vs_bridge_client::call_vs_current_solution(context.vs_bridge_endpoint).await,
        VS_CURRENT_DOCUMENT_TOOL_NAME => vs_bridge_client::call_vs_current_document(context.vs_bridge_endpoint).await,
        VS_CURRENT_SELECTION_TOOL_NAME => vs_bridge_client::call_vs_current_selection(context.vs_bridge_endpoint).await,
        VS_LIST_PROJECTS_TOOL_NAME => vs_bridge_client::call_vs_list_projects(context.vs_bridge_endpoint).await,
        VS_LIST_PROJECT_FILES_TOOL_NAME => vs_bridge_client::call_vs_list_project_files(context.vs_bridge_endpoint, arguments).await,
        VS_SEARCH_FILE_TOOL_NAME => vs_bridge_client::call_vs_search_file(context.vs_bridge_endpoint, context.workspace_root, arguments).await,
        VS_SEARCH_CONTENT_TOOL_NAME => vs_bridge_client::call_vs_search_content(context.vs_bridge_endpoint, context.workspace_root, arguments).await,
        VS_FIND_SYMBOL_TOOL_NAME | VS_FIND_REFERENCES_TOOL_NAME | VS_GO_TO_DEFINITION_TOOL_NAME => Ok(vs_semantic_tool_not_implemented(context, name)),
        VS_GET_ERROR_LIST_TOOL_NAME => vs_bridge_client::call_vs_get_error_list(context.vs_bridge_endpoint).await,
        GOAL_GET_TOOL_NAME => goal_get(context),
        GOAL_SET_TOOL_NAME => goal_set(context, arguments),
        GOAL_CLEAR_TOOL_NAME => goal_clear(context),
        PROGRESS_UPDATE_STEPS_TOOL_NAME => progress_update_steps(arguments),
        AGENT_SPAWN_TOOL_NAME | AGENT_WAIT_TOOL_NAME | AGENT_LIST_TOOL_NAME => Err("rejected: agent tools are handled by the agent runner".to_string()),
        _ => Err(format!("Unknown tool: {name}")),
    }
}

async fn execute_search_file(
    context: &ToolExecutionContext<'_>,
    arguments: &Value,
) -> Result<Value, String> {
    if bridge_endpoint_available(context.vs_bridge_endpoint) {
        match vs_bridge_client::call_vs_search_file(
            context.vs_bridge_endpoint,
            context.workspace_root,
            arguments,
        )
        .await
        {
            Ok(output) if bridge_output_ok(&output) => return Ok(output),
            Ok(output) => {
                let mut fallback = workspace_tools::search_file(context.workspace_root, arguments)?;
                annotate_search_source(&mut fallback, "workspace_fallback");
                annotate_bridge_fallback(&mut fallback, bridge_failure_summary(&output));
                return Ok(fallback);
            }
            Err(error) => {
                let mut fallback = workspace_tools::search_file(context.workspace_root, arguments)?;
                annotate_search_source(&mut fallback, "workspace_fallback");
                annotate_bridge_fallback(
                    &mut fallback,
                    json!({
                        "ok": false,
                        "status": "client_error",
                        "message": error,
                        "source": "vsix",
                    }),
                );
                return Ok(fallback);
            }
        }
    }

    let mut output = workspace_tools::search_file(context.workspace_root, arguments)?;
    annotate_search_source(&mut output, "workspace");
    Ok(output)
}

async fn execute_search_content(
    context: &ToolExecutionContext<'_>,
    arguments: &Value,
) -> Result<Value, String> {
    if bridge_endpoint_available(context.vs_bridge_endpoint) {
        match vs_bridge_client::call_vs_search_content(
            context.vs_bridge_endpoint,
            context.workspace_root,
            arguments,
        )
        .await
        {
            Ok(output) if bridge_output_ok(&output) => return Ok(output),
            Ok(output) => {
                let mut fallback =
                    workspace_tools::search_content(context.workspace_root, arguments)?;
                annotate_search_source(&mut fallback, "workspace_fallback");
                annotate_bridge_fallback(&mut fallback, bridge_failure_summary(&output));
                return Ok(fallback);
            }
            Err(error) => {
                let mut fallback =
                    workspace_tools::search_content(context.workspace_root, arguments)?;
                annotate_search_source(&mut fallback, "workspace_fallback");
                annotate_bridge_fallback(
                    &mut fallback,
                    json!({
                        "ok": false,
                        "status": "client_error",
                        "message": error,
                        "source": "vsix",
                    }),
                );
                return Ok(fallback);
            }
        }
    }

    let mut output = workspace_tools::search_content(context.workspace_root, arguments)?;
    annotate_search_source(&mut output, "workspace");
    Ok(output)
}

fn vs_semantic_tool_not_implemented(context: &ToolExecutionContext<'_>, name: &str) -> Value {
    json!({
        "ok": false,
        "available": false,
        "status": "not_implemented",
        "source": "vsix",
        "tool": name,
        "bridgeConnected": bridge_endpoint_available(context.vs_bridge_endpoint),
        "message": "This VS semantic tool is defined for the model but the VSIX endpoint is not implemented yet.",
    })
}

fn bridge_endpoint_available(endpoint: Option<&str>) -> bool {
    endpoint
        .map(str::trim)
        .is_some_and(|endpoint| !endpoint.is_empty())
}

fn bridge_output_ok(output: &Value) -> bool {
    output.get("ok").and_then(Value::as_bool) == Some(true)
}

fn annotate_search_source(output: &mut Value, source: &str) {
    if let Some(object) = output.as_object_mut() {
        object.insert("source".to_string(), json!(source));
    }
}

fn annotate_bridge_fallback(output: &mut Value, bridge_summary: Value) {
    if let Some(object) = output.as_object_mut() {
        object.insert("vsFallback".to_string(), json!(true));
        object.insert("vsBridge".to_string(), bridge_summary);
    }
}

fn bridge_failure_summary(output: &Value) -> Value {
    let mut summary = serde_json::Map::new();
    for key in [
        "ok",
        "status",
        "message",
        "source",
        "endpoint",
        "route",
        "httpStatus",
    ] {
        if let Some(value) = output.get(key) {
            summary.insert(key.to_string(), value.clone());
        }
    }

    Value::Object(summary)
}

/// calculator.add is a deliberately trivial demo tool kept to
/// exercise the full tool-calling loop end-to-end. It is NOT a production
/// tool; production code should use the workspace/ and goal/ tools.
/// The handler returns a plain numeric result with no side effects.
fn calculator_add_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": CALCULATOR_ADD_TOOL_NAME,
            "description": "Add two numbers and return the result.",
            "parameters": {
                "type": "object",
                "properties": {
                    "a": { "type": "number" },
                    "b": { "type": "number" }
                },
                "required": ["a", "b"]
            }
        }
    })
}

fn list_dir_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": LIST_DIR_TOOL_NAME,
            "description": "List immediate child directories and files under a directory path. Accepts workspace-relative paths and absolute local paths. Ignored directories include .git, .vs, bin, obj, build, out, node_modules, and .cache.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory path, for example ., src, or C:\\\\Users\\\\name\\\\AppData\\\\Local\\\\Temp."
                    }
                },
                "required": ["path"]
            }
        }
    })
}

fn read_file_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": READ_FILE_TOOL_NAME,
            "description": "Read a local text file from the current filesystem with line numbers. Accepts workspace-relative paths and absolute local paths. Treat a successful result as current disk contents for this turn, not a stale cache or snapshot. If the exact path was not already returned by a current tool result, prefer list_dir/search_file first to confirm path segments before reading; do not guess module directories from filenames or includes. If the requested file is missing, read_file may recover by reading a unique same-name file from a nearby existing ancestor and will mark the output as recovered. Defaults to at most 300 lines; use start_line and end_line for large files. Binary files are rejected.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path, relative to the workspace or absolute."
                    },
                    "start_line": {
                        "type": "integer",
                        "minimum": 1
                    },
                    "end_line": {
                        "type": "integer",
                        "minimum": 1
                    }
                },
                "required": ["path"]
            }
        }
    })
}

fn search_file_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": SEARCH_FILE_TOOL_NAME,
            "description": "Fallback/general file path search. For code in the current Visual Studio solution/project, use vs.search_file first so the trace records an explicit VS search attempt. Use this workspace search after VS search is unavailable/insufficient, for files outside the VS solution, logs, generated artifacts, or non-VS workspaces. Supports fuzzy filename search and simple wildcard patterns such as *.log or wz_render_frame_trace_*.log. Use this to locate filenames or paths, not to search file contents.",
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Filename or path pattern to search for. Use plain text for fuzzy search, or * and ? for wildcard matching."
                    },
                    "root": {
                        "type": "string",
                        "description": "Optional root directory to search under, relative to the workspace or absolute."
                    },
                    "path": {
                        "type": "string",
                        "description": "Compatibility alias for root. Prefer root in new calls."
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "default": 100
                    }
                },
                "required": ["pattern"]
            }
        }
    })
}

fn search_content_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": SEARCH_CONTENT_TOOL_NAME,
            "description": "Fallback/general text content search. For code in the current Visual Studio solution/project, use vs.search first so the trace records an explicit VS search attempt. Use this workspace search after VS search is unavailable/insufficient, for files outside the VS solution, logs, generated artifacts, or non-VS workspaces. Accepts workspace-relative and absolute roots. Returns structured matches with file, line, column, text, before, and after. Narrow root or file_glob for large repositories. If the result says search_limited, do not repeat the same broad search; retry with a narrower root/path or file_glob.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Text or regex to search for."
                    },
                    "root": {
                        "type": "string",
                        "description": "Optional root directory or file to search under, relative to the workspace or absolute."
                    },
                    "path": {
                        "type": "string",
                        "description": "Compatibility alias for root. Prefer root in new calls."
                    },
                    "file_glob": {
                        "type": "string",
                        "description": "Optional glob such as *.cpp, **/*.h, or *.rs."
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "default": 100
                    },
                    "context_lines": {
                        "type": "integer",
                        "minimum": 0,
                        "default": 2
                    },
                    "case_sensitive": {
                        "type": "boolean",
                        "default": false
                    },
                    "regex": {
                        "type": "boolean",
                        "default": false
                    }
                },
                "required": ["query"]
            }
        }
    })
}

fn edit_file_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": EDIT_FILE_TOOL_NAME,
            "description": "Edit an existing text file inside the workspace by replacing one exact text block. Prefer this for existing-file changes. The match is CRLF/LF tolerant and preserves the target file's existing line endings.",
            "parameters": {
                "type": "object",
                "properties": {
                    "file": { "type": "string", "description": "Workspace-relative file path." },
                    "search": { "type": "string", "description": "Exact text to replace. Must occur exactly once." },
                    "replace": { "type": "string", "description": "Replacement text." }
                },
                "required": ["file", "search", "replace"]
            }
        }
    })
}

fn write_file_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": WRITE_FILE_TOOL_NAME,
            "description": "Write UTF-8 text to a workspace-relative file. Use this mainly for new files or intentional full-file writes; use edit_file for existing-file modifications. Existing-file overwrites are protected against placeholder/test content and large destructive shrinkage. Line endings use the existing file style, or .vscode/settings.json files.eol, or CRLF by default.",
            "parameters": {
                "type": "object",
                "properties": {
                    "file": { "type": "string", "description": "Workspace-relative file path." },
                    "content": { "type": "string", "description": "Full new file contents." }
                },
                "required": ["file", "content"]
            }
        }
    })
}

fn shell_command_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": SHELL_COMMAND_TOOL_NAME,
            "description": "Run a bounded command in the workspace. Dangerous commands are rejected; install commands require explicit confirmation.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Command line to execute." },
                    "timeout_ms": { "type": "integer", "minimum": 1, "default": 60000 }
                },
                "required": ["command"]
            }
        }
    })
}

fn apply_patch_raw_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": APPLY_PATCH_RAW_TOOL_NAME,
            "description": "Compatibility-only raw patch tool for Codex/OpenAI-style models. Other providers should use edit_file.",
            "parameters": {
                "type": "object",
                "properties": {
                    "patch": { "type": "string" }
                },
                "required": ["patch"]
            }
        }
    })
}

fn git_status_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": GIT_STATUS_TOOL_NAME,
            "description": "Run git status in the workspace and return stdout/stderr. Use this before summarizing or committing changes. Defaults to `git status --short --branch`.",
            "parameters": {
                "type": "object",
                "properties": {
                    "porcelain": { "type": "boolean", "default": true, "description": "Use short porcelain-style output." },
                    "branch": { "type": "boolean", "default": true, "description": "Include branch information." },
                    "pathspecs": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional workspace-relative pathspecs to limit status."
                    },
                    "max_bytes": { "type": "integer", "minimum": 1, "default": 200000 }
                }
            }
        }
    })
}

fn git_diff_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": GIT_DIFF_TOOL_NAME,
            "description": "Run git diff in the workspace and return stdout/stderr. Use cached=true for staged changes, stat=true for a summary, and pathspecs to limit large diffs.",
            "parameters": {
                "type": "object",
                "properties": {
                    "cached": { "type": "boolean", "default": false, "description": "Diff staged changes with --cached." },
                    "stat": { "type": "boolean", "default": false, "description": "Return --stat summary output." },
                    "name_only": { "type": "boolean", "default": false, "description": "Return changed path names only." },
                    "unified": { "type": "integer", "minimum": 0, "default": 3, "description": "Number of context lines for patch output." },
                    "pathspecs": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional workspace-relative pathspecs to limit the diff."
                    },
                    "max_bytes": { "type": "integer", "minimum": 1, "default": 200000 }
                }
            }
        }
    })
}

fn git_log_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": GIT_LOG_TOOL_NAME,
            "description": "Run git log in the workspace. Defaults to the latest 10 commits in one-line format.",
            "parameters": {
                "type": "object",
                "properties": {
                    "max_count": { "type": "integer", "minimum": 1, "maximum": 100, "default": 10 },
                    "oneline": { "type": "boolean", "default": true },
                    "max_bytes": { "type": "integer", "minimum": 1, "default": 200000 }
                }
            }
        }
    })
}

fn git_show_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": GIT_SHOW_TOOL_NAME,
            "description": "Run git show for a revision in the workspace. Defaults to HEAD with --stat.",
            "parameters": {
                "type": "object",
                "properties": {
                    "revision": { "type": "string", "default": "HEAD", "description": "Revision, commit, tag, or ref to show." },
                    "stat": { "type": "boolean", "default": true },
                    "name_only": { "type": "boolean", "default": false },
                    "pathspecs": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional workspace-relative pathspecs to limit output."
                    },
                    "max_bytes": { "type": "integer", "minimum": 1, "default": 200000 }
                }
            }
        }
    })
}

fn git_add_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": GIT_ADD_TOOL_NAME,
            "description": "Stage workspace changes with git add. Use explicit pathspecs for intended files; use all=true only when the user clearly asked to stage all relevant dirty work.",
            "parameters": {
                "type": "object",
                "properties": {
                    "pathspecs": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Workspace-relative files or directories to stage."
                    },
                    "all": { "type": "boolean", "default": false, "description": "Run git add --all." },
                    "max_bytes": { "type": "integer", "minimum": 1, "default": 200000 }
                }
            }
        }
    })
}

fn git_reset_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": GIT_RESET_TOOL_NAME,
            "description": "Unstage paths with git reset. This does not discard working tree changes. Use all=true to unstage everything.",
            "parameters": {
                "type": "object",
                "properties": {
                    "pathspecs": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Workspace-relative files or directories to unstage."
                    },
                    "all": { "type": "boolean", "default": false, "description": "Run git reset with no pathspecs to unstage all." },
                    "max_bytes": { "type": "integer", "minimum": 1, "default": 200000 }
                }
            }
        }
    })
}

fn git_commit_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": GIT_COMMIT_TOOL_NAME,
            "description": "Create a git commit from the currently staged changes. Always inspect git/status and staged git/diff evidence first, and only commit when the user requested a commit.",
            "parameters": {
                "type": "object",
                "properties": {
                    "message": { "type": "string", "description": "Commit summary line." },
                    "body": { "type": "string", "description": "Optional commit body paragraph." },
                    "body_paragraphs": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional additional body paragraphs, each passed as a separate -m argument."
                    },
                    "allow_empty": { "type": "boolean", "default": false },
                    "max_bytes": { "type": "integer", "minimum": 1, "default": 200000 }
                },
                "required": ["message"]
            }
        }
    })
}

fn get_file_context_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": GET_FILE_CONTEXT_TOOL_NAME,
            "description": "Read line-numbered context around one line in a local text file. Accepts workspace-relative paths and absolute local paths. Defaults to 30 lines before and after.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path, relative to the workspace or absolute."
                    },
                    "line": {
                        "type": "integer",
                        "minimum": 1
                    },
                    "before": {
                        "type": "integer",
                        "minimum": 0,
                        "default": 30
                    },
                    "after": {
                        "type": "integer",
                        "minimum": 0,
                        "default": 30
                    }
                },
                "required": ["path", "line"]
            }
        }
    })
}

fn document_read_docx_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": DOCUMENT_READ_DOCX_TOOL_NAME,
            "description": "Read a local .docx Word document. Accepts workspace-relative paths and absolute local paths. Extracts paragraphs, headings, tables, comments, headers, footers, footnotes, endnotes, images, and plain text. Read-only; does not preserve exact visual layout.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": ".docx file path, relative to the workspace or absolute."
                    },
                    "max_items": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 5000,
                        "description": "Maximum extracted paragraphs/tables/comments/slides per category. Defaults to 1200."
                    }
                },
                "required": ["path"]
            }
        }
    })
}

fn presentation_read_pptx_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": PRESENTATION_READ_PPTX_TOOL_NAME,
            "description": "Read a local .pptx PowerPoint deck. Accepts workspace-relative paths and absolute local paths. Extracts slide order, titles, text boxes, speaker notes, and image relationship targets. Read-only; does not preserve exact visual layout.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": ".pptx file path, relative to the workspace or absolute."
                    },
                    "max_items": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 5000,
                        "description": "Maximum extracted slides/text items per category. Defaults to 1200."
                    }
                },
                "required": ["path"]
            }
        }
    })
}

fn vs_current_solution_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": VS_CURRENT_SOLUTION_TOOL_NAME,
            "description": "Read the current Visual Studio solution through the connected VS Bridge. Requires Bridge Connected; returns bridge_not_connected when Visual Studio is not connected.",
            "parameters": {
                "type": "object",
                "properties": {}
            }
        }
    })
}

fn vs_current_document_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": VS_CURRENT_DOCUMENT_TOOL_NAME,
            "description": "Read the active Visual Studio text document through the connected VS Bridge. Returns path, cursor line/column, language, text, totalLines, and textTruncated. Requires Bridge Connected; returned text may be truncated.",
            "parameters": {
                "type": "object",
                "properties": {}
            }
        }
    })
}

fn vs_current_selection_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": VS_CURRENT_SELECTION_TOOL_NAME,
            "description": "Read the active Visual Studio text selection through the connected VS Bridge. Returns current selection text and start/end line/column, or isEmpty=true for an empty caret selection. Requires Bridge Connected.",
            "parameters": {
                "type": "object",
                "properties": {}
            }
        }
    })
}

fn vs_list_projects_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": VS_LIST_PROJECTS_TOOL_NAME,
            "description": "List projects currently loaded in the active Visual Studio solution through the connected VS Bridge. Handles solution folders best-effort. Requires Bridge Connected.",
            "parameters": {
                "type": "object",
                "properties": {}
            }
        }
    })
}

fn vs_list_project_files_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": VS_LIST_PROJECT_FILES_TOOL_NAME,
            "description": "Lightweight DTE ProjectItems file enumeration through the connected VS Bridge. This is not a full code graph or semantic index. Requires Bridge Connected and returns truncated=true if the file limit is hit.",
            "parameters": {
                "type": "object",
                "properties": {
                    "projectName": {
                        "type": "string",
                        "description": "Optional Visual Studio project display name to enumerate. If omitted, all loaded projects are scanned."
                    },
                    "projectUniqueName": {
                        "type": "string",
                        "description": "Optional Visual Studio project UniqueName to enumerate."
                    },
                    "maxFiles": {
                        "type": "integer",
                        "minimum": 1,
                        "default": 2000,
                        "description": "Maximum files to return before truncating."
                    }
                }
            }
        }
    })
}

fn vs_search_file_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": VS_SEARCH_FILE_TOOL_NAME,
            "description": "Search file paths through the connected Visual Studio Bridge only. Searches the active VS solution/project file list and does not fall back to workspace search. Use this when the user is asking about the currently open Visual Studio project and trace attribution to VS search matters.",
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Filename or path pattern to search for. Use plain text for fuzzy search, or * and ? for wildcard matching."
                    },
                    "root": {
                        "type": "string",
                        "description": "Optional root directory or file to search under, relative to the workspace or absolute."
                    },
                    "path": {
                        "type": "string",
                        "description": "Compatibility alias for root. Prefer root in new calls."
                    },
                    "projectName": {
                        "type": "string",
                        "description": "Optional Visual Studio project display name to search."
                    },
                    "projectUniqueName": {
                        "type": "string",
                        "description": "Optional Visual Studio project UniqueName to search."
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "default": 100
                    }
                },
                "required": ["pattern"]
            }
        }
    })
}

fn vs_search_content_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": VS_SEARCH_CONTENT_TOOL_NAME,
            "description": "Search text content through the connected Visual Studio Bridge only. This is VS solution/project-scoped text search, not semantic symbol analysis, and it does not fall back to workspace search. Use this first when Visual Studio is connected and the user is asking about the currently open solution/project.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Text or regex to search for."
                    },
                    "root": {
                        "type": "string",
                        "description": "Optional root directory or file to search under, relative to the workspace or absolute."
                    },
                    "path": {
                        "type": "string",
                        "description": "Compatibility alias for root. Prefer root in new calls."
                    },
                    "file_glob": {
                        "type": "string",
                        "description": "Optional glob such as *.cpp, **/*.h, or *.rs."
                    },
                    "projectName": {
                        "type": "string",
                        "description": "Optional Visual Studio project display name to search."
                    },
                    "projectUniqueName": {
                        "type": "string",
                        "description": "Optional Visual Studio project UniqueName to search."
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "default": 100
                    },
                    "context_lines": {
                        "type": "integer",
                        "minimum": 0,
                        "default": 2
                    },
                    "case_sensitive": {
                        "type": "boolean",
                        "default": false
                    },
                    "regex": {
                        "type": "boolean",
                        "default": false
                    }
                },
                "required": ["query"]
            }
        }
    })
}

fn vs_find_symbol_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": VS_FIND_SYMBOL_TOOL_NAME,
            "description": "Planned VS semantic symbol lookup for the active solution/project. The current VSIX endpoint is not implemented yet; calls return available=false until the bridge adds semantic support.",
            "parameters": {
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Symbol name to find."
                    },
                    "kind": {
                        "type": "string",
                        "description": "Optional symbol kind hint such as function, class, method, enum, or variable."
                    },
                    "projectName": {
                        "type": "string",
                        "description": "Optional Visual Studio project display name to search."
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "default": 50
                    }
                },
                "required": ["symbol"]
            }
        }
    })
}

fn vs_find_references_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": VS_FIND_REFERENCES_TOOL_NAME,
            "description": "Planned VS semantic reference lookup for the active solution/project. The current VSIX endpoint is not implemented yet; calls return available=false until the bridge adds semantic support.",
            "parameters": {
                "type": "object",
                "properties": {
                    "file": {
                        "type": "string",
                        "description": "Optional workspace-relative or absolute file path containing the symbol."
                    },
                    "line": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional 1-based line number for cursor-based lookup."
                    },
                    "column": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional 1-based column number for cursor-based lookup."
                    },
                    "symbol": {
                        "type": "string",
                        "description": "Optional symbol name when cursor location is not available."
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "default": 100
                    }
                }
            }
        }
    })
}

fn vs_go_to_definition_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": VS_GO_TO_DEFINITION_TOOL_NAME,
            "description": "Planned VS go-to-definition lookup for the active solution/project. The current VSIX endpoint is not implemented yet; calls return available=false until the bridge adds semantic support.",
            "parameters": {
                "type": "object",
                "properties": {
                    "file": {
                        "type": "string",
                        "description": "Optional workspace-relative or absolute file path containing the symbol."
                    },
                    "line": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional 1-based line number for cursor-based lookup."
                    },
                    "column": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional 1-based column number for cursor-based lookup."
                    },
                    "symbol": {
                        "type": "string",
                        "description": "Optional symbol name when cursor location is not available."
                    }
                }
            }
        }
    })
}

fn vs_get_error_list_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": VS_GET_ERROR_LIST_TOOL_NAME,
            "description": "Read Visual Studio Error List diagnostics through the connected VS Bridge when available. The current VSIX may return available=false and message=not_available; do not treat this as clangd, LSP, or full code graph analysis.",
            "parameters": {
                "type": "object",
                "properties": {}
            }
        }
    })
}

fn mcp_list_servers_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": MCP_LIST_SERVERS_TOOL_NAME,
            "description": "List configured MCP servers and their current connection state without connecting to them.",
            "parameters": {
                "type": "object",
                "properties": {}
            }
        }
    })
}

fn mcp_connect_server_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": MCP_CONNECT_SERVER_TOOL_NAME,
            "description": "Connect a configured MCP server, read tools/list, and register its tools for subsequent model requests. Newly connected tools become available from the next model request, not the current response.",
            "parameters": {
                "type": "object",
                "properties": {
                    "server_id": {
                        "type": "string",
                        "description": "Configured MCP server id, for example ue."
                    }
                },
                "required": ["server_id"]
            }
        }
    })
}

fn mcp_disconnect_server_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": MCP_DISCONNECT_SERVER_TOOL_NAME,
            "description": "Disconnect a connected MCP server and remove its tools from future model requests.",
            "parameters": {
                "type": "object",
                "properties": {
                    "server_id": {
                        "type": "string",
                        "description": "Configured MCP server id, for example ue."
                    }
                },
                "required": ["server_id"]
            }
        }
    })
}

fn mcp_list_tools_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": MCP_LIST_TOOLS_TOOL_NAME,
            "description": "List tools currently registered for one MCP server. If the server is not connected, this returns an empty tool list with its current status.",
            "parameters": {
                "type": "object",
                "properties": {
                    "server_id": {
                        "type": "string",
                        "description": "Configured MCP server id, for example ue."
                    }
                },
                "required": ["server_id"]
            }
        }
    })
}

fn agent_spawn_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": AGENT_SPAWN_TOOL_NAME,
            "description": "Spawn a CodeForge subagent for a bounded code task. Use read-only subagents proactively for broad, multi-file, review, architecture, debugging, performance, security, or test-gap investigations. In a write-capable parent run, set readOnly=false only for a narrow implementation task and assign non-overlapping allowedWritePaths. Avoid subagents for simple single-file edits or direct answers. The child agent returns a concise summary; raw child trace is stored separately.",
            "parameters": {
                "type": "object",
                "properties": {
                    "taskName": {
                        "type": "string",
                        "description": "Short stable name for the child task, such as architecture-review or test-gap-scan."
                    },
                    "message": {
                        "type": "string",
                        "description": "Exact task for the child agent. Include scope, files or systems to inspect, and required summary format."
                    },
                    "agentKind": {
                        "type": "string",
                        "enum": ["explorer", "reviewer", "worker"],
                        "default": "explorer",
                        "description": "Subagent profile. Use explorer or reviewer for read-only work, and worker for write-capable implementation subtasks when readOnly is false."
                    },
                    "modelId": {
                        "type": "string",
                        "description": "Optional model override. Omit to inherit the parent model."
                    },
                    "reasoningEffort": {
                        "type": "string",
                        "enum": ["low", "medium", "high"],
                        "description": "Optional reasoning effort override."
                    },
                    "readOnly": {
                        "type": "boolean",
                        "default": true,
                        "description": "Defaults to true. Keep true for investigations. Set false only in a write-capable parent run when the child should be allowed to edit workspace files; read-only parent runs reject false."
                    },
                    "allowedWritePaths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "default": [],
                        "description": "Workspace-relative files or directories this child may edit. Required and must be non-empty when readOnly=false. Use non-overlapping allowedWritePaths across running write-capable subagents."
                    }
                },
                "required": ["taskName", "message"]
            }
        }
    })
}

fn agent_wait_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": AGENT_WAIT_TOOL_NAME,
            "description": "Wait for spawned subagents to finish and collect their concise summaries. If childTaskIds is omitted, waits for all active child agents for this parent run.",
            "parameters": {
                "type": "object",
                "properties": {
                    "childTaskIds": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional child task IDs returned by agent/spawn."
                    }
                }
            }
        }
    })
}

fn agent_list_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": AGENT_LIST_TOOL_NAME,
            "description": "List subagents spawned by the current parent run and their status.",
            "parameters": {
                "type": "object",
                "properties": {}
            }
        }
    })
}

fn progress_update_steps_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": PROGRESS_UPDATE_STEPS_TOOL_NAME,
            "description": "Update the visible task-specific Steps panel for the current run. Use this for non-trivial research, review, debug, verify, or implement work. The steps must be generated for the current user request; do not use a fixed template.",
            "parameters": {
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "default": "Steps",
                        "description": "Short panel title. Prefer Steps unless a mode-specific title is clearer."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["research", "debug", "implement", "review", "verify", "default"],
                        "description": "Current internal mode when useful for display."
                    },
                    "summary": {
                        "type": "string",
                        "description": "Optional one-line progress summary or current focus."
                    },
                    "steps": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 12,
                        "description": "Task-specific steps for this run. Send the full current list on every update.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {
                                    "type": "string",
                                    "description": "Stable short id for this step within the current run."
                                },
                                "title": {
                                    "type": "string",
                                    "description": "Human-readable step title."
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed", "blocked", "skipped"],
                                    "description": "Current step state."
                                },
                                "detail": {
                                    "type": "string",
                                    "description": "Optional short detail, evidence target, or blocker."
                                }
                            },
                            "required": ["id", "title", "status"]
                        }
                    }
                },
                "required": ["steps"]
            }
        }
    })
}

fn progress_update_steps(arguments: &Value) -> Result<Value, String> {
    let steps = arguments
        .get("steps")
        .and_then(Value::as_array)
        .ok_or_else(|| "invalid_arguments: steps must be a non-empty array".to_string())?;
    if steps.is_empty() {
        return Err("invalid_arguments: steps must not be empty".to_string());
    }

    let normalized_steps = steps
        .iter()
        .take(12)
        .enumerate()
        .map(|(index, step)| normalize_progress_step(step, index))
        .collect::<Result<Vec<_>, _>>()?;
    let completed_count = normalized_steps
        .iter()
        .filter(|step| step.get("status").and_then(Value::as_str) == Some("completed"))
        .count();
    let active_step = normalized_steps
        .iter()
        .find(|step| step.get("status").and_then(Value::as_str) == Some("in_progress"))
        .and_then(|step| step.get("title"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let total_count = normalized_steps.len();
    let title =
        optional_non_empty_string(arguments, "title").unwrap_or_else(|| "Steps".to_string());
    let mode = optional_non_empty_string(arguments, "mode");
    let summary = optional_non_empty_string(arguments, "summary");

    Ok(json!({
        "title": title,
        "mode": mode,
        "summary": summary,
        "steps": normalized_steps,
        "completedCount": completed_count,
        "totalCount": total_count,
        "activeStep": active_step,
    }))
}

fn normalize_progress_step(step: &Value, index: usize) -> Result<Value, String> {
    let object = step
        .as_object()
        .ok_or_else(|| "invalid_arguments: each step must be an object".to_string())?;
    let title = object
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "invalid_arguments: each step requires a non-empty title".to_string())?;
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("step-{}", index + 1));
    let status = object
        .get("status")
        .and_then(Value::as_str)
        .map(normalize_progress_status)
        .unwrap_or("pending");
    let detail = object
        .get("detail")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    Ok(json!({
        "id": id,
        "title": title,
        "status": status,
        "detail": detail,
    }))
}

fn normalize_progress_status(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "in_progress" | "in-progress" | "running" | "active" => "in_progress",
        "completed" | "complete" | "done" => "completed",
        "blocked" | "failed" => "blocked",
        "skipped" | "skip" => "skipped",
        _ => "pending",
    }
}

fn optional_non_empty_string(arguments: &Value, key: &str) -> Option<String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn add(arguments: &Value) -> Result<Value, String> {
    let a = read_number(arguments, "a")?;
    let b = read_number(arguments, "b")?;
    Ok(json!({ "result": number_value(a + b) }))
}

fn read_number(arguments: &Value, key: &str) -> Result<f64, String> {
    arguments
        .get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("calculator.add requires numeric field `{key}`"))
}

fn number_value(number: f64) -> Value {
    if number.is_finite()
        && number.fract() == 0.0
        && number <= i64::MAX as f64
        && number >= i64::MIN as f64
    {
        json!(number as i64)
    } else {
        json!(number)
    }
}

// ---------------------------------------------------------------------------
// Goal tools
// ---------------------------------------------------------------------------

fn goal_get_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": GOAL_GET_TOOL_NAME,
            "description": "Get the current goal state. Returns the objective, status, token budget, tokens used, and elapsed time. Returns no_active_goal when no goal is set.",
            "parameters": {
                "type": "object",
                "properties": {}
            }
        }
    })
}

fn goal_set_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": GOAL_SET_TOOL_NAME,
            "description": "Set or replace the current goal with a new objective. Optionally set a token budget. Writes back to the active session's goal state.",
            "parameters": {
                "type": "object",
                "properties": {
                    "objective": {
                        "type": "string",
                        "description": "The goal objective text."
                    },
                    "tokenBudget": {
                        "type": "integer",
                        "description": "Optional token budget for the goal.",
                        "minimum": 1
                    }
                },
                "required": ["objective"]
            }
        }
    })
}

fn goal_clear_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": GOAL_CLEAR_TOOL_NAME,
            "description": "Clear the current goal. Returns the previous goal if one was active.",
            "parameters": {
                "type": "object",
                "properties": {}
            }
        }
    })
}

fn goal_get(context: &ToolExecutionContext<'_>) -> Result<Value, String> {
    let current = context.goal.as_ref().and_then(|opt| opt.as_ref());
    match current {
        Some(goal) => Ok(json!({
            "active": true,
            "objective": goal.objective,
            "status": goal.status.label(),
            "tokenBudget": goal.token_budget,
            "tokensUsed": goal.tokens_used,
            "timeUsedSeconds": goal.time_used_seconds,
        })),
        None => Ok(json!({
            "active": false,
            "message": "No active goal.",
        })),
    }
}

fn goal_set(context: &mut ToolExecutionContext<'_>, arguments: &Value) -> Result<Value, String> {
    let objective = arguments
        .get("objective")
        .and_then(Value::as_str)
        .ok_or_else(|| "goal/set requires an 'objective' string field".to_string())?;
    let token_budget = arguments.get("tokenBudget").and_then(Value::as_i64);
    let mut goal = GoalState::new(objective.to_string());
    goal.token_budget = token_budget;
    // Write back through the mutable reference if the caller provided one.
    // This lets the tool call propagate to the owning session instead of
    // leaving a stale snapshot in the session's goal field.
    let response = json!({
        "set": true,
        "objective": goal.objective,
        "status": goal.status.label(),
        "tokenBudget": goal.token_budget,
        "tokensUsed": goal.tokens_used,
        "timeUsedSeconds": goal.time_used_seconds,
    });
    if let Some(slot) = context.goal.as_deref_mut() {
        *slot = Some(goal);
    }
    Ok(response)
}

fn goal_clear(context: &ToolExecutionContext<'_>) -> Result<Value, String> {
    let current = context.goal.as_ref().and_then(|opt| opt.as_ref());
    match current {
        Some(goal) => Ok(json!({
            "cleared": true,
            "previousObjective": goal.objective,
            "previousStatus": goal.status.label(),
        })),
        None => Ok(json!({
            "cleared": false,
            "message": "No active goal to clear.",
        })),
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn test_context() -> ToolExecutionContext<'static> {
        ToolExecutionContext {
            workspace_root: ".",
            vs_bridge_endpoint: None,
            allow_shell: false,
            assume_yes: false,
            cli_mode: false,
            goal: None,
            mcp_runtime: None,
            write_scope: None,
        }
    }

    #[test]
    fn calculator_add_returns_sum() {
        let result = tauri::async_runtime::block_on(execute_tool(
            &mut test_context(),
            CALCULATOR_ADD_TOOL_NAME,
            &json!({ "a": 1, "b": 1 }),
        ))
        .unwrap();

        assert_eq!(result, json!({ "result": 2 }));
    }

    #[test]
    fn calculator_add_requires_numbers() {
        let error = tauri::async_runtime::block_on(execute_tool(
            &mut test_context(),
            CALCULATOR_ADD_TOOL_NAME,
            &json!({ "a": "1", "b": 1 }),
        ))
        .unwrap_err();

        assert!(error.contains("numeric field `a`"));
    }

    #[test]
    fn unknown_tool_returns_error() {
        let error = tauri::async_runtime::block_on(execute_tool(
            &mut test_context(),
            "missing.tool",
            &json!({}),
        ))
        .unwrap_err();

        assert!(error.contains("Unknown tool: missing.tool"));
    }

    #[test]
    fn tool_definitions_expose_codex_style_workspace_tools() {
        let names = tool_definitions()
            .into_iter()
            .filter_map(|tool| {
                tool.get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect::<Vec<_>>();

        assert!(names.contains(&LIST_DIR_TOOL_NAME.to_string()));
        assert!(names.contains(&WORKSPACE_LIST_DIR_TOOL_NAME.to_string()));
        assert!(names.contains(&READ_FILE_TOOL_NAME.to_string()));
        assert!(names.contains(&WORKSPACE_READ_FILE_TOOL_NAME.to_string()));
        assert!(names.contains(&SEARCH_FILE_TOOL_NAME.to_string()));
        assert!(names.contains(&WORKSPACE_SEARCH_FILE_TOOL_NAME.to_string()));
        assert!(names.contains(&SEARCH_CONTENT_TOOL_NAME.to_string()));
        assert!(names.contains(&WORKSPACE_SEARCH_CONTENT_TOOL_NAME.to_string()));
        assert!(names.contains(&GET_FILE_CONTEXT_TOOL_NAME.to_string()));
        assert!(names.contains(&WORKSPACE_GET_FILE_CONTEXT_TOOL_NAME.to_string()));
        assert!(names.contains(&DOCUMENT_READ_DOCX_TOOL_NAME.to_string()));
        assert!(names.contains(&PRESENTATION_READ_PPTX_TOOL_NAME.to_string()));
        assert!(names.contains(&GIT_STATUS_TOOL_NAME.to_string()));
        assert!(names.contains(&GIT_DIFF_TOOL_NAME.to_string()));
        assert!(names.contains(&GIT_LOG_TOOL_NAME.to_string()));
        assert!(names.contains(&GIT_SHOW_TOOL_NAME.to_string()));
        assert!(names.contains(&GIT_ADD_TOOL_NAME.to_string()));
        assert!(names.contains(&GIT_RESET_TOOL_NAME.to_string()));
        assert!(names.contains(&GIT_COMMIT_TOOL_NAME.to_string()));
        assert!(names.contains(&WORKSPACE_EDIT_FILE_TOOL_NAME.to_string()));
        assert!(names.contains(&WORKSPACE_WRITE_FILE_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_CURRENT_SOLUTION_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_CURRENT_DOCUMENT_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_CURRENT_SELECTION_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_LIST_PROJECTS_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_LIST_PROJECT_FILES_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_SEARCH_FILE_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_SEARCH_CONTENT_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_FIND_SYMBOL_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_FIND_REFERENCES_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_GO_TO_DEFINITION_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_GET_ERROR_LIST_TOOL_NAME.to_string()));
        assert!(names.contains(&PROGRESS_UPDATE_STEPS_TOOL_NAME.to_string()));
        assert!(!names.contains(&CALCULATOR_ADD_TOOL_NAME.to_string()));
    }

    #[test]
    fn tool_definitions_list_vs_search_before_workspace_search() {
        let names = tool_definitions()
            .into_iter()
            .filter_map(|tool| {
                tool.get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect::<Vec<_>>();

        let vs_search = names
            .iter()
            .position(|name| name == VS_SEARCH_CONTENT_TOOL_NAME)
            .unwrap();
        let workspace_search = names
            .iter()
            .position(|name| name == WORKSPACE_SEARCH_CONTENT_TOOL_NAME)
            .unwrap();
        let legacy_search = names
            .iter()
            .position(|name| name == SEARCH_CONTENT_TOOL_NAME)
            .unwrap();

        assert!(vs_search < workspace_search);
        assert!(vs_search < legacy_search);
    }

    #[test]
    fn tool_call_test_definitions_expose_calculator_demo_only() {
        let names = tool_call_test_definitions()
            .into_iter()
            .filter_map(|tool| {
                tool.get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect::<Vec<_>>();

        assert_eq!(names, vec![CALCULATOR_ADD_TOOL_NAME.to_string()]);
    }

    #[test]
    fn read_only_tool_definitions_exclude_write_and_shell_tools() {
        let names = read_only_tool_definitions()
            .into_iter()
            .filter_map(|tool| {
                tool.get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect::<Vec<_>>();

        assert!(names.contains(&WORKSPACE_READ_FILE_TOOL_NAME.to_string()));
        assert!(names.contains(&WORKSPACE_SEARCH_CONTENT_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_SEARCH_FILE_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_SEARCH_CONTENT_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_FIND_SYMBOL_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_FIND_REFERENCES_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_GO_TO_DEFINITION_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_GET_ERROR_LIST_TOOL_NAME.to_string()));
        assert!(names.contains(&PROGRESS_UPDATE_STEPS_TOOL_NAME.to_string()));
        assert!(names.contains(&GIT_STATUS_TOOL_NAME.to_string()));
        assert!(names.contains(&GIT_DIFF_TOOL_NAME.to_string()));
        assert!(names.contains(&GIT_LOG_TOOL_NAME.to_string()));
        assert!(names.contains(&GIT_SHOW_TOOL_NAME.to_string()));
        assert!(!names.contains(&EDIT_FILE_TOOL_NAME.to_string()));
        assert!(!names.contains(&WORKSPACE_EDIT_FILE_TOOL_NAME.to_string()));
        assert!(!names.contains(&WRITE_FILE_TOOL_NAME.to_string()));
        assert!(!names.contains(&WORKSPACE_WRITE_FILE_TOOL_NAME.to_string()));
        assert!(!names.contains(&GIT_ADD_TOOL_NAME.to_string()));
        assert!(!names.contains(&GIT_RESET_TOOL_NAME.to_string()));
        assert!(!names.contains(&GIT_COMMIT_TOOL_NAME.to_string()));
        assert!(!names.contains(&SHELL_COMMAND_TOOL_NAME.to_string()));
    }

    #[test]
    fn read_file_definition_says_results_are_current_disk_contents() {
        let tool = read_file_definition();
        let description = tool["function"]["description"].as_str().unwrap();

        assert!(description.contains("current filesystem"));
        assert!(description.contains("current disk contents"));
        assert!(description.contains("not a stale cache or snapshot"));
        assert!(description.contains("prefer list_dir/search_file first"));
        assert!(description.contains("mark the output as recovered"));
    }

    #[test]
    fn agent_tool_definitions_expose_spawn_wait_and_list() {
        let names = agent_tool_definitions()
            .into_iter()
            .filter_map(|tool| {
                tool.get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            vec![
                AGENT_SPAWN_TOOL_NAME.to_string(),
                AGENT_WAIT_TOOL_NAME.to_string(),
                AGENT_LIST_TOOL_NAME.to_string(),
            ]
        );
    }

    #[test]
    fn agent_spawn_definition_allows_proactive_readonly_delegation() {
        let tools = agent_tool_definitions();
        let spawn = tools
            .iter()
            .find(|tool| {
                tool.get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)
                    == Some(AGENT_SPAWN_TOOL_NAME)
            })
            .unwrap();
        let description = spawn["function"]["description"].as_str().unwrap();
        let read_only_description = spawn["function"]["parameters"]["properties"]["readOnly"]
            ["description"]
            .as_str()
            .unwrap();
        let agent_kind_description = spawn["function"]["parameters"]["properties"]["agentKind"]
            ["description"]
            .as_str()
            .unwrap();
        let allowed_write_paths_description = spawn["function"]["parameters"]["properties"]
            ["allowedWritePaths"]["description"]
            .as_str()
            .unwrap();

        assert!(description.contains("proactively"));
        assert!(description.contains("read-only subagents"));
        assert!(description.contains("readOnly=false"));
        assert!(description.contains("allowedWritePaths"));
        assert!(read_only_description.contains("write-capable parent run"));
        assert!(allowed_write_paths_description.contains("Required"));
        assert!(allowed_write_paths_description.contains("non-overlapping"));
        assert!(agent_kind_description.contains("worker"));
        assert!(!description.contains("explicitly asks"));
        assert!(!read_only_description.contains("Must be true"));
    }

    #[test]
    fn progress_update_steps_normalizes_dynamic_steps() {
        let output = progress_update_steps(&json!({
            "title": "Investigation",
            "mode": "research",
            "summary": "Checking tool wiring",
            "steps": [
                { "id": "entry", "title": "Find entry point", "status": "done" },
                { "id": "tools", "title": "Inspect tools", "status": "running", "detail": "tool_registry.rs" },
                { "id": "answer", "title": "Summarize result", "status": "pending" }
            ]
        }))
        .unwrap();

        assert_eq!(output["title"], json!("Investigation"));
        assert_eq!(output["mode"], json!("research"));
        assert_eq!(output["completedCount"], json!(1));
        assert_eq!(output["totalCount"], json!(3));
        assert_eq!(output["activeStep"], json!("Inspect tools"));
        assert_eq!(output["steps"][0]["status"], json!("completed"));
        assert_eq!(output["steps"][1]["status"], json!("in_progress"));
        assert_eq!(output["steps"][1]["detail"], json!("tool_registry.rs"));
    }

    #[test]
    fn vs_tool_returns_bridge_not_connected_when_endpoint_missing() {
        let result = tauri::async_runtime::block_on(execute_tool(
            &mut test_context(),
            VS_CURRENT_DOCUMENT_TOOL_NAME,
            &json!({}),
        ))
        .unwrap();

        assert_eq!(result["ok"], json!(false));
        assert_eq!(result["status"], json!("bridge_not_connected"));
        assert_eq!(result["source"], json!("vsix"));
    }
}

#[cfg(test)]
mod cli_runtime_tests {
    use super::*;
    use crate::tool_interface::ToolOutputStatus;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::process::Command;
    use std::thread;

    fn workspace() -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("codeforge-tool-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn context(root: &str, allow_shell: bool) -> ToolExecutionContext<'_> {
        ToolExecutionContext {
            workspace_root: root,
            vs_bridge_endpoint: None,
            allow_shell,
            assume_yes: true,
            cli_mode: true,
            goal: None,
            mcp_runtime: None,
            write_scope: None,
        }
    }

    fn stub_bridge(response_body: &'static str) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0u8; 4096];
            let read = stream.read(&mut buffer).unwrap();
            let request = String::from_utf8_lossy(&buffer[..read]).to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream.write_all(response.as_bytes()).unwrap();
            request
        });

        (endpoint, handle)
    }

    #[test]
    fn unknown_tool_returns_error_result() {
        let root = workspace();
        let result = tauri::async_runtime::block_on(execute_tool_result(
            &mut context(root.to_str().unwrap(), false),
            "missing.tool",
            &json!({}),
        ));
        assert_eq!(result.status, ToolOutputStatus::Error);
        assert!(result.error.unwrap().contains("Unknown tool"));
    }

    #[test]
    fn minimax_profile_does_not_expose_apply_patch_raw() {
        let tools = cli_tool_definitions("minimax", "MiniMax-M2.7", false);
        let names = tools
            .iter()
            .filter_map(|tool| tool.get("function")?.get("name")?.as_str())
            .collect::<Vec<_>>();
        assert!(!names.contains(&APPLY_PATCH_RAW_TOOL_NAME));
        assert!(names.contains(&EDIT_FILE_TOOL_NAME));
        assert!(names.contains(&WORKSPACE_EDIT_FILE_TOOL_NAME));
        assert!(names.contains(&GIT_STATUS_TOOL_NAME));
        assert!(names.contains(&GIT_DIFF_TOOL_NAME));
        assert!(names.contains(&GIT_ADD_TOOL_NAME));
        assert!(names.contains(&GIT_COMMIT_TOOL_NAME));
        assert!(!names.contains(&CALCULATOR_ADD_TOOL_NAME));
    }

    #[test]
    fn workspace_get_file_context_alias_executes() {
        let root = workspace();
        fs::write(root.join("sample.txt"), "one\ntwo\nthree\n").unwrap();
        let result = tauri::async_runtime::block_on(execute_tool_result(
            &mut context(root.to_str().unwrap(), false),
            WORKSPACE_GET_FILE_CONTEXT_TOOL_NAME,
            &json!({ "path": "sample.txt", "line": 2, "before": 1, "after": 1 }),
        ));
        assert_eq!(result.status, ToolOutputStatus::Ok);
        let output = result.output.unwrap();
        assert_eq!(output["file"], json!("sample.txt"));
        assert_eq!(output["line"], json!(2));
        assert_eq!(output["lines"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn search_file_without_vs_bridge_uses_workspace_source() {
        let root = workspace();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("sample.cpp"), "int main() {}\n").unwrap();
        let result = tauri::async_runtime::block_on(execute_tool_result(
            &mut context(root.to_str().unwrap(), false),
            SEARCH_FILE_TOOL_NAME,
            &json!({ "pattern": "sample.cpp", "max_results": 10 }),
        ));
        assert_eq!(result.status, ToolOutputStatus::Ok);
        let output = result.output.unwrap();
        assert_eq!(output["source"], json!("workspace"));
        assert_eq!(output["engine"], json!("codex-file-search"));
        assert_eq!(output["paths"][0], json!("src/sample.cpp"));
    }

    #[test]
    fn search_content_without_vs_bridge_uses_workspace_source() {
        let root = workspace();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("sample.cpp"), "int target = 1;\n").unwrap();
        let result = tauri::async_runtime::block_on(execute_tool_result(
            &mut context(root.to_str().unwrap(), false),
            SEARCH_CONTENT_TOOL_NAME,
            &json!({ "query": "target", "file_glob": "*.cpp", "max_results": 10 }),
        ));
        assert_eq!(result.status, ToolOutputStatus::Ok);
        let output = result.output.unwrap();
        assert_eq!(output["source"], json!("workspace"));
        assert_eq!(output["matches"][0]["file"], json!("src/sample.cpp"));
    }

    #[test]
    fn search_content_accepts_path_alias_without_vs_bridge() {
        let root = workspace();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("App.css"), ".toggle-row {}\n").unwrap();
        fs::write(root.join("src").join("Other.css"), ".toggle-row {}\n").unwrap();
        let result = tauri::async_runtime::block_on(execute_tool_result(
            &mut context(root.to_str().unwrap(), false),
            SEARCH_CONTENT_TOOL_NAME,
            &json!({ "query": "toggle-row", "path": "src/App.css", "max_results": 10 }),
        ));
        assert_eq!(result.status, ToolOutputStatus::Ok);
        let output = result.output.unwrap();
        assert_eq!(output["source"], json!("workspace"));
        assert_eq!(output["root"], json!("src/App.css"));
        assert_eq!(output["matches"].as_array().unwrap().len(), 1);
        assert_eq!(output["matches"][0]["file"], json!("src/App.css"));
    }

    #[test]
    fn search_file_prefers_vs_bridge_when_connected() {
        let root = workspace();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("sample.cpp"), "int main() {}\n").unwrap();
        let (endpoint, handle) = stub_bridge(
            r#"{"ok":true,"source":"vsix","engine":"stub-vsix-file-search","matches":[],"paths":[],"count":0}"#,
        );
        let root_text = root.to_string_lossy().to_string();
        let mut context = ToolExecutionContext {
            workspace_root: &root_text,
            vs_bridge_endpoint: Some(&endpoint),
            allow_shell: false,
            assume_yes: true,
            cli_mode: true,
            goal: None,
            mcp_runtime: None,
            write_scope: None,
        };

        let result = tauri::async_runtime::block_on(execute_tool_result(
            &mut context,
            SEARCH_FILE_TOOL_NAME,
            &json!({ "pattern": "sample.cpp", "max_results": 10 }),
        ));
        let request = handle.join().unwrap();
        assert_eq!(result.status, ToolOutputStatus::Ok);
        let output = result.output.unwrap();
        assert_eq!(output["source"], json!("vsix"));
        assert_eq!(output["engine"], json!("stub-vsix-file-search"));
        assert!(request.starts_with("POST /searchFiles "));
        assert!(request.contains("\"pattern\":\"sample.cpp\""));
        assert!(request.contains("\"workspaceRoot\""));
    }

    #[test]
    fn search_content_prefers_vs_bridge_when_connected() {
        let root = workspace();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("sample.cpp"), "int target = 1;\n").unwrap();
        let (endpoint, handle) = stub_bridge(
            r#"{"ok":true,"source":"vsix","engine":"stub-vsix-content-search","matches":[],"count":0}"#,
        );
        let root_text = root.to_string_lossy().to_string();
        let mut context = ToolExecutionContext {
            workspace_root: &root_text,
            vs_bridge_endpoint: Some(&endpoint),
            allow_shell: false,
            assume_yes: true,
            cli_mode: true,
            goal: None,
            mcp_runtime: None,
            write_scope: None,
        };

        let result = tauri::async_runtime::block_on(execute_tool_result(
            &mut context,
            SEARCH_CONTENT_TOOL_NAME,
            &json!({ "query": "target", "path": "src/sample.cpp", "file_glob": "*.cpp", "max_results": 10 }),
        ));
        let request = handle.join().unwrap();
        assert_eq!(result.status, ToolOutputStatus::Ok);
        let output = result.output.unwrap();
        assert_eq!(output["source"], json!("vsix"));
        assert_eq!(output["engine"], json!("stub-vsix-content-search"));
        assert!(request.starts_with("POST /searchContent "));
        assert!(request.contains("\"query\":\"target\""));
        assert!(request.contains("\"root\":\"src/sample.cpp\""));
        assert!(request.contains("\"fileGlob\":\"*.cpp\""));
        assert!(request.contains("\"workspaceRoot\""));
    }

    #[test]
    fn explicit_vs_search_content_uses_bridge_tool_without_workspace_fallback() {
        let root = workspace();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("sample.cpp"), "int target = 1;\n").unwrap();
        let root_text = root.to_string_lossy().to_string();

        let output = tauri::async_runtime::block_on(execute_tool(
            &mut context(&root_text, false),
            VS_SEARCH_CONTENT_TOOL_NAME,
            &json!({ "query": "target", "path": "src/sample.cpp", "max_results": 10 }),
        ))
        .unwrap();

        assert_eq!(output["source"], json!("vsix"));
        assert_eq!(output["status"], json!("bridge_not_connected"));
        assert!(output.get("matches").is_none());
    }

    #[test]
    fn explicit_vs_search_file_calls_search_files_endpoint() {
        let root = workspace();
        let (endpoint, handle) = stub_bridge(
            r#"{"ok":true,"source":"vsix","engine":"stub-vsix-file-search","matches":[],"paths":[],"count":0}"#,
        );
        let root_text = root.to_string_lossy().to_string();
        let mut context = ToolExecutionContext {
            workspace_root: &root_text,
            vs_bridge_endpoint: Some(&endpoint),
            allow_shell: false,
            assume_yes: true,
            cli_mode: true,
            goal: None,
            mcp_runtime: None,
            write_scope: None,
        };

        let result = tauri::async_runtime::block_on(execute_tool_result(
            &mut context,
            VS_SEARCH_FILE_TOOL_NAME,
            &json!({ "pattern": "sample.cpp", "max_results": 10 }),
        ));
        let request = handle.join().unwrap();

        assert_eq!(result.status, ToolOutputStatus::Ok);
        let output = result.output.unwrap();
        assert_eq!(output["source"], json!("vsix"));
        assert_eq!(output["engine"], json!("stub-vsix-file-search"));
        assert!(request.starts_with("POST /searchFiles "));
        assert!(request.contains("\"pattern\":\"sample.cpp\""));
    }

    #[test]
    fn vs_semantic_tools_report_not_implemented() {
        let output = tauri::async_runtime::block_on(execute_tool(
            &mut context(".", false),
            VS_FIND_SYMBOL_TOOL_NAME,
            &json!({ "symbol": "CalcTransparencyFaceState" }),
        ))
        .unwrap();

        assert_eq!(output["ok"], json!(false));
        assert_eq!(output["available"], json!(false));
        assert_eq!(output["status"], json!("not_implemented"));
        assert_eq!(output["source"], json!("vsix"));
    }

    #[test]
    fn edit_file_search_replace_modifies_file() {
        let root = workspace();
        fs::write(root.join("sample.txt"), "alpha\nbeta\n").unwrap();
        let result = tauri::async_runtime::block_on(execute_tool_result(
            &mut context(root.to_str().unwrap(), false),
            EDIT_FILE_TOOL_NAME,
            &json!({ "file": "sample.txt", "search": "beta", "replace": "gamma" }),
        ));
        assert_eq!(result.status, ToolOutputStatus::Ok);
        assert_eq!(
            fs::read_to_string(root.join("sample.txt")).unwrap(),
            "alpha\ngamma\n"
        );
    }

    #[test]
    fn write_scope_rejects_writes_outside_allowed_paths() {
        let root = workspace();
        fs::write(root.join("allowed.txt"), "alpha\nbeta\n").unwrap();
        fs::write(root.join("blocked.txt"), "alpha\nbeta\n").unwrap();
        let root_text = root.to_string_lossy().to_string();
        let allowed = vec!["allowed.txt".to_string()];
        let scope = WriteScope::from_allowed_paths(&root_text, &allowed).unwrap();
        let mut context = ToolExecutionContext {
            workspace_root: &root_text,
            vs_bridge_endpoint: None,
            allow_shell: false,
            assume_yes: true,
            cli_mode: true,
            goal: None,
            mcp_runtime: None,
            write_scope: Some(&scope),
        };

        let allowed_result = tauri::async_runtime::block_on(execute_tool_result(
            &mut context,
            EDIT_FILE_TOOL_NAME,
            &json!({ "file": "allowed.txt", "search": "beta", "replace": "gamma" }),
        ));
        assert_eq!(allowed_result.status, ToolOutputStatus::Ok);

        let blocked_result = tauri::async_runtime::block_on(execute_tool_result(
            &mut context,
            WRITE_FILE_TOOL_NAME,
            &json!({ "file": "blocked.txt", "content": "nope\n" }),
        ));
        assert_eq!(blocked_result.status, ToolOutputStatus::Rejected);
        assert!(blocked_result
            .error
            .unwrap()
            .contains("outside this subagent write scope"));
        assert_eq!(
            fs::read_to_string(root.join("blocked.txt")).unwrap(),
            "alpha\nbeta\n"
        );
    }

    #[test]
    fn shell_command_timeout_returns_timeout_result() {
        let root = workspace();
        let command = if cfg!(windows) {
            "ping 127.0.0.1 -n 3 > nul"
        } else {
            "sleep 2"
        };
        let result = tauri::async_runtime::block_on(execute_tool_result(
            &mut context(root.to_str().unwrap(), true),
            SHELL_COMMAND_TOOL_NAME,
            &json!({ "command": command, "timeout_ms": 1 }),
        ));
        assert_eq!(result.status, ToolOutputStatus::Timeout);
    }

    #[test]
    fn git_add_and_commit_tools_run_in_workspace() {
        if !git_available() {
            return;
        }
        let root = workspace();
        run_git(&root, &["init"]);
        run_git(&root, &["config", "user.email", "codeforge@example.test"]);
        run_git(&root, &["config", "user.name", "CodeForge Test"]);
        fs::write(root.join("sample.txt"), "hello\n").unwrap();
        let root_text = root.to_string_lossy().to_string();

        let status = tauri::async_runtime::block_on(execute_tool_result(
            &mut context(&root_text, false),
            GIT_STATUS_TOOL_NAME,
            &json!({}),
        ));
        assert_eq!(status.status, ToolOutputStatus::Ok);
        assert!(status.output.unwrap()["stdout"]
            .as_str()
            .unwrap()
            .contains("sample.txt"));

        let added = tauri::async_runtime::block_on(execute_tool_result(
            &mut context(&root_text, false),
            GIT_ADD_TOOL_NAME,
            &json!({ "pathspecs": ["sample.txt"] }),
        ));
        assert_eq!(added.status, ToolOutputStatus::Ok);
        assert_eq!(added.output.unwrap()["success"], json!(true));

        let committed = tauri::async_runtime::block_on(execute_tool_result(
            &mut context(&root_text, false),
            GIT_COMMIT_TOOL_NAME,
            &json!({ "message": "test: commit sample" }),
        ));
        assert_eq!(committed.status, ToolOutputStatus::Ok);
        let output = committed.output.unwrap();
        assert_eq!(output["success"], json!(true));
        assert!(output["commit"].as_str().unwrap_or_default().len() >= 7);
    }

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn run_git(root: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
