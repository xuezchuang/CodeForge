use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::goal_state::GoalState;
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
pub const GET_FILE_CONTEXT_TOOL_NAME: &str = "get_file_context";
pub const WORKSPACE_GET_FILE_CONTEXT_TOOL_NAME: &str = "workspace/get_file_context";
pub const VS_CURRENT_SOLUTION_TOOL_NAME: &str = "vs.current_solution";
pub const VS_CURRENT_DOCUMENT_TOOL_NAME: &str = "vs.current_document";
pub const VS_CURRENT_SELECTION_TOOL_NAME: &str = "vs.current_selection";
pub const VS_LIST_PROJECTS_TOOL_NAME: &str = "vs.list_projects";
pub const VS_LIST_PROJECT_FILES_TOOL_NAME: &str = "vs.list_project_files";
pub const VS_GET_ERROR_LIST_TOOL_NAME: &str = "vs.get_error_list";
pub const GOAL_GET_TOOL_NAME: &str = "goal/get";
pub const GOAL_SET_TOOL_NAME: &str = "goal/set";
pub const GOAL_CLEAR_TOOL_NAME: &str = "goal/clear";

pub struct ToolExecutionContext<'a> {
    pub workspace_root: &'a str,
    pub vs_bridge_endpoint: Option<&'a str>,
    pub allow_shell: bool,
    pub assume_yes: bool,
    pub cli_mode: bool,
    pub goal: Option<&'a mut Option<GoalState>>,
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
    let mut tools = workspace_tool_definitions();
    tools.extend([
        list_dir_definition(),
        get_file_context_definition(),
        vs_current_solution_definition(),
        vs_current_document_definition(),
        vs_current_selection_definition(),
        vs_list_projects_definition(),
        vs_list_project_files_definition(),
        vs_get_error_list_definition(),
        goal_get_definition(),
        goal_set_definition(),
        goal_clear_definition(),
    ]);
    tools.extend(workspace_namespace_aliases());
    tools
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

pub fn exposes_apply_patch_raw(provider_type: &str, model_id: &str) -> bool {
    let provider = provider_type.to_ascii_lowercase();
    let model = model_id.to_ascii_lowercase();
    (provider.contains("openai") || provider.contains("codex") || model.contains("codex"))
        && !matches!(
            provider.as_str(),
            "minimax" | "deepseek" | "glm" | "codebuddy"
        )
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
    let started = Instant::now();
    match execute_tool_inner(context, name, arguments).await {
        Ok(output) => ToolOutput::ok(output, started.elapsed().as_millis() as u64),
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

fn tool_timeout(name: &str) -> Duration {
    match name {
        READ_FILE_TOOL_NAME => Duration::from_secs(10),
        SEARCH_CONTENT_TOOL_NAME | EDIT_FILE_TOOL_NAME | WRITE_FILE_TOOL_NAME => {
            Duration::from_secs(30)
        }
        SHELL_COMMAND_TOOL_NAME => Duration::from_secs(60),
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
        EDIT_FILE_TOOL_NAME | WORKSPACE_EDIT_FILE_TOOL_NAME => workspace_tools::edit_file(context.workspace_root, arguments),
        WRITE_FILE_TOOL_NAME | WORKSPACE_WRITE_FILE_TOOL_NAME => workspace_tools::write_file(context.workspace_root, arguments),
        SHELL_COMMAND_TOOL_NAME | WORKSPACE_SHELL_COMMAND_TOOL_NAME => workspace_tools::shell_command(
            context.workspace_root,
            arguments,
            context.allow_shell,
            context.assume_yes,
        ).await,
        APPLY_PATCH_RAW_TOOL_NAME => Err("rejected: apply_patch_raw is reserved for compatible Codex/OpenAI adapters and is not implemented in the CLI runtime".to_string()),
        GET_FILE_CONTEXT_TOOL_NAME | WORKSPACE_GET_FILE_CONTEXT_TOOL_NAME => workspace_tools::get_file_context(context.workspace_root, arguments),
        VS_CURRENT_SOLUTION_TOOL_NAME => vs_bridge_client::call_vs_current_solution(context.vs_bridge_endpoint).await,
        VS_CURRENT_DOCUMENT_TOOL_NAME => vs_bridge_client::call_vs_current_document(context.vs_bridge_endpoint).await,
        VS_CURRENT_SELECTION_TOOL_NAME => vs_bridge_client::call_vs_current_selection(context.vs_bridge_endpoint).await,
        VS_LIST_PROJECTS_TOOL_NAME => vs_bridge_client::call_vs_list_projects(context.vs_bridge_endpoint).await,
        VS_LIST_PROJECT_FILES_TOOL_NAME => vs_bridge_client::call_vs_list_project_files(context.vs_bridge_endpoint, arguments).await,
        VS_GET_ERROR_LIST_TOOL_NAME => vs_bridge_client::call_vs_get_error_list(context.vs_bridge_endpoint).await,
        GOAL_GET_TOOL_NAME => goal_get(context),
        GOAL_SET_TOOL_NAME => goal_set(context, arguments),
        GOAL_CLEAR_TOOL_NAME => goal_clear(context),
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
            "description": "List immediate child directories and files under a workspace-relative path. Paths cannot escape the workspace root. Ignored directories include .git, .vs, bin, obj, build, out, node_modules, and .cache.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Workspace-relative directory path, for example . or src."
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
            "description": "Read a text file inside the workspace with line numbers. Defaults to at most 300 lines; use start_line and end_line for large files. Binary files are rejected.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Workspace-relative file path."
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
            "description": "Search for file paths. When Visual Studio Bridge is connected, searches the active Visual Studio solution first and falls back to workspace fuzzy search. Use this to locate filenames or paths, not to search file contents.",
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Filename or path pattern to search for."
                    },
                    "root": {
                        "type": "string",
                        "description": "Optional workspace-relative directory to search under."
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
            "description": "Search text content. When Visual Studio Bridge is connected, searches active Visual Studio solution files first and falls back to workspace content search. Returns structured matches with file, line, column, text, before, and after. Narrow root or file_glob for large repositories.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Text or regex to search for."
                    },
                    "root": {
                        "type": "string",
                        "description": "Optional workspace-relative directory to search under."
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
            "description": "Edit a text file inside the workspace by replacing one exact text block. Prefer this over raw patch tools for third-party models.",
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
            "description": "Write UTF-8 text to a workspace-relative file. Creates parent directories inside the workspace.",
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

fn get_file_context_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": GET_FILE_CONTEXT_TOOL_NAME,
            "description": "Read line-numbered context around one line in a workspace file. Defaults to 30 lines before and after.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Workspace-relative file path."
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
        assert!(names.contains(&WORKSPACE_EDIT_FILE_TOOL_NAME.to_string()));
        assert!(names.contains(&WORKSPACE_WRITE_FILE_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_CURRENT_SOLUTION_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_CURRENT_DOCUMENT_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_CURRENT_SELECTION_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_LIST_PROJECTS_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_LIST_PROJECT_FILES_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_GET_ERROR_LIST_TOOL_NAME.to_string()));
        assert!(!names.contains(&CALCULATOR_ADD_TOOL_NAME.to_string()));
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
        };

        let result = tauri::async_runtime::block_on(execute_tool_result(
            &mut context,
            SEARCH_CONTENT_TOOL_NAME,
            &json!({ "query": "target", "file_glob": "*.cpp", "max_results": 10 }),
        ));
        let request = handle.join().unwrap();
        assert_eq!(result.status, ToolOutputStatus::Ok);
        let output = result.output.unwrap();
        assert_eq!(output["source"], json!("vsix"));
        assert_eq!(output["engine"], json!("stub-vsix-content-search"));
        assert!(request.starts_with("POST /searchContent "));
        assert!(request.contains("\"query\":\"target\""));
        assert!(request.contains("\"fileGlob\":\"*.cpp\""));
        assert!(request.contains("\"workspaceRoot\""));
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
}
