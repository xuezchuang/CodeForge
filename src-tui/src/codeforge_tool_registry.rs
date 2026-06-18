//! CodeForge-owned tool schema, registry, and dispatch surface for the TUI.
//!
//! Phase 2 of the CodeForge TUI backend-and-tooling plan. This module is
//! TUI-owned, not borrowed from the Tauri desktop backend, because the TUI
//! runs as a standalone `codeforge.exe` and must carry its own tool surface.
//!
//! The registry is intentionally schema-first: tool definitions are typed
//! Rust structs, and `codeforge_backend::run_turn` attaches these definitions
//! to the model request, parses `tool_calls` deltas, dispatches them, and
//! feeds the results back to the model.
//!
//! Tools live under namespaced names per the plan (`workspace/read_file`,
//! `goal/get`, `vs.current_solution`, etc.) so the same names map cleanly
//! between the TUI registry, the Tauri tool registry, and the TUI
//! transcript. Read-only tools do not require user approval; tools that
//! mutate the workspace or call external systems are flagged explicitly.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::codeforge_goal_state::GoalState;
use crate::codeforge_tool_trace::ToolCallTrace;
use crate::codeforge_tool_trace::ToolTraceEvent;

// ---------------------------------------------------------------------------
// Tool namespace constants
// ---------------------------------------------------------------------------

pub const WORKSPACE_READ_FILE_TOOL_NAME: &str = "workspace/read_file";
pub const WORKSPACE_LIST_DIR_TOOL_NAME: &str = "workspace/list_dir";
pub const WORKSPACE_SEARCH_TOOL_NAME: &str = "workspace/search";
pub const WORKSPACE_APPLY_PATCH_TOOL_NAME: &str = "workspace/apply_patch";
pub const GOAL_GET_TOOL_NAME: &str = "goal/get";
pub const GOAL_SET_TOOL_NAME: &str = "goal/set";
pub const GOAL_CLEAR_TOOL_NAME: &str = "goal/clear";
pub const VS_CURRENT_SOLUTION_TOOL_NAME: &str = "vs.current_solution";
pub const VS_CURRENT_DOCUMENT_TOOL_NAME: &str = "vs.current_document";
pub const VS_CURRENT_SELECTION_TOOL_NAME: &str = "vs.current_selection";
pub const VS_LIST_PROJECTS_TOOL_NAME: &str = "vs.list_projects";
pub const VS_FIND_DEFINITION_TOOL_NAME: &str = "vs.find_definition";
pub const VS_FIND_REFERENCES_TOOL_NAME: &str = "vs.find_references";
pub const VS_GET_ERROR_LIST_TOOL_NAME: &str = "vs.get_error_list";

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Status of a tool execution result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutputStatus {
    Ok,
    Error,
    Timeout,
    Rejected,
}

/// A fully-typed tool definition: name, description, JSON Schema, and
/// execution policy hints.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinition {
    /// Stable tool name (e.g. "workspace/read_file").
    pub name: String,
    /// Human-readable description shown to the model and in trace UI.
    pub description: String,
    /// JSON Schema for the tool's input parameters.
    pub parameters: Value,
    /// Whether this tool requires explicit user approval before execution.
    pub requires_approval: bool,
    /// Whether this tool is read-only (never modifies files or state).
    pub read_only: bool,
    /// Namespace grouping, e.g. "workspace", "vs", or "goal".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

impl ToolDefinition {
    /// Convert to the OpenAI function-call format sent to the model.
    pub fn to_openai_function(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            }
        })
    }
}

/// A concrete invocation of a tool with JSON arguments.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInvocation {
    /// The tool name being invoked.
    pub tool_name: String,
    /// The input arguments as a JSON value.
    pub arguments: Value,
    /// Optional call ID linking this invocation to a model `tool_call.id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
}

/// A short user-visible summary plus the structured output of a tool.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolOutput {
    /// Execution status.
    pub status: ToolOutputStatus,
    /// Structured output value on success or partial success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    /// Error message when status is Error, Timeout, or Rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Wall-clock execution duration in milliseconds.
    pub elapsed_ms: u64,
    /// Short human-readable summary of the output for trace display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl ToolOutput {
    pub fn ok(output: Value, elapsed_ms: u64) -> Self {
        Self {
            status: ToolOutputStatus::Ok,
            output: Some(output),
            error: None,
            elapsed_ms,
            summary: None,
        }
    }

    pub fn ok_with_summary(output: Value, elapsed_ms: u64, summary: String) -> Self {
        Self {
            status: ToolOutputStatus::Ok,
            output: Some(output),
            error: None,
            elapsed_ms,
            summary: Some(summary),
        }
    }

    pub fn error(message: String, elapsed_ms: u64) -> Self {
        Self {
            status: ToolOutputStatus::Error,
            output: None,
            error: Some(message),
            elapsed_ms,
            summary: None,
        }
    }

    #[cfg(test)]
    pub fn rejected(reason: String) -> Self {
        Self {
            status: ToolOutputStatus::Rejected,
            output: None,
            error: Some(reason),
            elapsed_ms: 0,
            summary: None,
        }
    }

    pub fn is_ok(&self) -> bool {
        self.status == ToolOutputStatus::Ok
    }

    /// Convert to the JSON value format sent back to the model.
    pub fn to_model_value(&self) -> Value {
        json!({
            "status": match self.status {
                ToolOutputStatus::Ok => "ok",
                ToolOutputStatus::Error => "error",
                ToolOutputStatus::Timeout => "timeout",
                ToolOutputStatus::Rejected => "rejected",
            },
            "ok": self.is_ok(),
            "output": self.output,
            "error": self.error,
            "elapsedMs": self.elapsed_ms,
        })
    }
}

/// Per-invocation context passed to a tool handler. Workspace tools use
/// `workspace_root` to confine filesystem access; goal tools use
/// `goal_slot`; VS tools use `vs_bridge_endpoint`.
#[derive(Clone)]
pub struct ToolExecutionContext {
    /// Absolute workspace root for path-confined tools. May be `None` for
    /// tools that do not touch the filesystem (e.g. `goal/get`).
    pub workspace_root: Option<PathBuf>,
    /// CodeForge home directory (typically `~/.codeforge`). Used by
    /// `goal/set` and `goal/clear` to persist `.codeforge/goal.json`.
    pub codeforge_home: Option<PathBuf>,
    /// Stable identifier of the active turn, for trace correlation.
    pub turn_id: Option<String>,
    /// Identifier of the active thread, for trace correlation.
    pub thread_id: Option<String>,
    /// VS Bridge endpoint override. When `None`, the
    /// `codeforge_vs_bridge::endpoint()` resolver falls back to the
    /// `CODEFORGE_VS_BRIDGE_URL` environment variable.
    pub vs_bridge_endpoint: Option<String>,
    /// Shared mutable slot for the current goal. Goal tools read the
    /// current state from this slot and write back updated state. The
    /// slot is shared across the entire TUI session, not per call.
    pub goal_slot: Option<Arc<std::sync::RwLock<Option<GoalState>>>>,
}

/// Async handler invoked by [`ToolRegistry::dispatch`]. The handler returns
/// a [`ToolOutput`] that the registry packages into a trace event.
pub type ToolHandler =
    Arc<dyn Fn(ToolInvocation, ToolExecutionContext) -> ToolOutput + Send + Sync>;

// ---------------------------------------------------------------------------
// ToolRegistry
// ---------------------------------------------------------------------------

/// Owns the active set of [`ToolDefinition`]s and their handlers. New
/// definitions are registered at startup; the live set is read-only after
/// `freeze`.
pub struct ToolRegistry {
    definitions: HashMap<String, ToolDefinition>,
    handlers: HashMap<String, ToolHandler>,
    frozen: bool,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            definitions: HashMap::new(),
            handlers: HashMap::new(),
            frozen: false,
        }
    }

    /// Registers a tool with its definition and async handler. Returns
    /// `false` if a tool with the same name is already registered.
    pub fn register(&mut self, definition: ToolDefinition, handler: ToolHandler) -> bool {
        if self.frozen || self.definitions.contains_key(&definition.name) {
            return false;
        }
        self.definitions
            .insert(definition.name.clone(), definition.clone());
        self.handlers.insert(definition.name, handler);
        true
    }

    /// Returns the list of tool definitions in registration order.
    pub fn list_definitions(&self) -> Vec<ToolDefinition> {
        let mut names: Vec<&String> = self.definitions.keys().collect();
        names.sort();
        names
            .into_iter()
            .filter_map(|name| self.definitions.get(name).cloned())
            .collect()
    }

    /// Returns the tool definitions formatted for the OpenAI function-calling
    /// `tools` request field.
    pub fn openai_tools(&self) -> Vec<Value> {
        self.list_definitions()
            .into_iter()
            .map(|definition| definition.to_openai_function())
            .collect()
    }

    /// Locks the registry so further `register` calls are rejected.
    pub fn freeze(&mut self) {
        self.frozen = true;
    }

    /// Dispatches `invocation` through the registered handler. The trace
    /// callback receives a [`ToolTraceEvent`] describing the call lifecycle
    /// and result. Returns the same [`ToolOutput`] that the handler
    /// produced, with `elapsed_ms` populated by the registry.
    pub fn dispatch(
        &self,
        invocation: ToolInvocation,
        context: ToolExecutionContext,
        mut on_trace: impl FnMut(ToolTraceEvent),
    ) -> ToolOutput {
        let tool_name = invocation.tool_name.clone();
        let started = Instant::now();
        on_trace(ToolTraceEvent::Started {
            invocation: invocation.clone(),
        });

        let handler = match self.handlers.get(&tool_name) {
            Some(handler) => handler.clone(),
            None => {
                let output = ToolOutput::error(
                    format!("unknown tool: {tool_name}"),
                    started.elapsed().as_millis() as u64,
                );
                on_trace(ToolTraceEvent::Completed {
                    trace: ToolCallTrace {
                        invocation,
                        output: Some(output.clone()),
                        approval_granted: None,
                    },
                });
                return output;
            }
        };

        let mut output = handler(invocation.clone(), context);
        // Trust the handler's elapsed_ms if it set one, otherwise fill in.
        if output.elapsed_ms == 0 {
            output.elapsed_ms = started.elapsed().as_millis() as u64;
        }
        on_trace(ToolTraceEvent::Completed {
            trace: ToolCallTrace {
                invocation,
                output: Some(output.clone()),
                approval_granted: None,
            },
        });
        output
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Default tool surface (Phase 3 entry point)
// ---------------------------------------------------------------------------

/// Builds the default CodeForge TUI tool registry used by
/// `codeforge_backend` when it advertises tools to the model.
pub fn default_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    register_workspace_read_file(&mut registry);
    register_workspace_list_dir(&mut registry);
    register_workspace_search(&mut registry);
    register_workspace_apply_patch(&mut registry);
    register_goal_get(&mut registry);
    register_goal_set(&mut registry);
    register_goal_clear(&mut registry);
    register_vs_current_solution(&mut registry);
    register_vs_current_document(&mut registry);
    register_vs_current_selection(&mut registry);
    register_vs_list_projects(&mut registry);
    register_vs_find_definition(&mut registry);
    register_vs_find_references(&mut registry);
    register_vs_get_error_list(&mut registry);
    registry.freeze();
    registry
}

fn namespace_of(name: &str) -> Option<String> {
    name.split_once('/')
        .map(|(ns, _)| ns.to_string())
        .or_else(|| name.split_once('.').map(|(ns, _)| ns.to_string()))
}

fn build_definition(
    name: &str,
    description: &str,
    parameters: Value,
    requires_approval: bool,
    read_only: bool,
) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        parameters,
        requires_approval,
        read_only,
        namespace: namespace_of(name),
    }
}

fn register_workspace_read_file(registry: &mut ToolRegistry) {
    let name = WORKSPACE_READ_FILE_TOOL_NAME;
    let definition = build_definition(
        name,
        "Read a text file inside the workspace with line numbers. Defaults to at most 300 lines; use start_line and end_line for large files. Binary files are rejected.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative file path."
                },
                "start_line": { "type": "integer", "minimum": 1 },
                "end_line": { "type": "integer", "minimum": 1 }
            },
            "required": ["path"]
        }),
        false,
        true,
    );
    let handler: ToolHandler =
        Arc::new(|invocation, context| tools::read_file(invocation, context));
    registry.register(definition, handler);
}

fn register_workspace_list_dir(registry: &mut ToolRegistry) {
    let name = WORKSPACE_LIST_DIR_TOOL_NAME;
    let definition = build_definition(
        name,
        "List immediate child directories and files under a workspace-relative path. Paths cannot escape the workspace root. Ignored directories include .git, .vs, bin, obj, build, out, node_modules, and .cache.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative directory path, for example '.' or 'src'."
                }
            },
            "required": ["path"]
        }),
        false,
        true,
    );
    let handler: ToolHandler = Arc::new(|invocation, context| tools::list_dir(invocation, context));
    registry.register(definition, handler);
}

fn register_workspace_search(registry: &mut ToolRegistry) {
    let name = WORKSPACE_SEARCH_TOOL_NAME;
    let definition = build_definition(
        name,
        "Search text content inside workspace files with bounded traversal. Returns structured matches with file, line, column, text, before, and after. Narrow root or file_glob for large repositories.",
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Text or regex to search for." },
                "root": { "type": "string", "description": "Optional workspace-relative directory to search under." },
                "file_glob": { "type": "string", "description": "Optional glob such as *.cpp, **/*.h, or *.rs." },
                "max_results": { "type": "integer", "minimum": 1, "default": 100 },
                "context_lines": { "type": "integer", "minimum": 0, "default": 2 },
                "case_sensitive": { "type": "boolean", "default": false },
                "regex": { "type": "boolean", "default": false }
            },
            "required": ["query"]
        }),
        false,
        true,
    );
    let handler: ToolHandler =
        Arc::new(|invocation, context| tools::search_content(invocation, context));
    registry.register(definition, handler);
}

fn register_workspace_apply_patch(registry: &mut ToolRegistry) {
    let name = WORKSPACE_APPLY_PATCH_TOOL_NAME;
    let definition = build_definition(
        name,
        "Edit a text file inside the workspace by replacing one exact text block. Requires explicit user approval before mutation.",
        json!({
            "type": "object",
            "properties": {
                "file": { "type": "string", "description": "Workspace-relative file path." },
                "search": { "type": "string", "description": "Exact text to replace. Must occur exactly once." },
                "replace": { "type": "string", "description": "Replacement text." }
            },
            "required": ["file", "search", "replace"]
        }),
        true,
        false,
    );
    let handler: ToolHandler =
        Arc::new(|invocation, context| tools::edit_file(invocation, context));
    registry.register(definition, handler);
}

fn register_goal_get(registry: &mut ToolRegistry) {
    let name = GOAL_GET_TOOL_NAME;
    let definition = build_definition(
        name,
        "Get the current goal state. Returns the objective, status, token budget, tokens used, and elapsed time.",
        json!({ "type": "object", "properties": {} }),
        false,
        true,
    );
    let handler: ToolHandler = Arc::new(tools::goal_get);
    registry.register(definition, handler);
}

fn register_goal_set(registry: &mut ToolRegistry) {
    let name = GOAL_SET_TOOL_NAME;
    let definition = build_definition(
        name,
        "Set or replace the current goal with a new objective. Persists to .codeforge/goal.json.",
        json!({
            "type": "object",
            "properties": {
                "objective": { "type": "string", "description": "The goal objective text." },
                "tokenBudget": { "type": "integer", "minimum": 1, "description": "Optional token budget." }
            },
            "required": ["objective"]
        }),
        true,
        false,
    );
    let handler: ToolHandler = Arc::new(tools::goal_set);
    registry.register(definition, handler);
}

fn register_goal_clear(registry: &mut ToolRegistry) {
    let name = GOAL_CLEAR_TOOL_NAME;
    let definition = build_definition(
        name,
        "Clear the current goal. Removes .codeforge/goal.json if present.",
        json!({ "type": "object", "properties": {} }),
        true,
        false,
    );
    let handler: ToolHandler = Arc::new(tools::goal_clear);
    registry.register(definition, handler);
}

fn register_vs_current_solution(registry: &mut ToolRegistry) {
    let name = VS_CURRENT_SOLUTION_TOOL_NAME;
    let definition = build_definition(
        name,
        "Read the current Visual Studio solution through the connected VS Bridge. Stub returns bridge_not_connected until the bridge is wired into the TUI.",
        json!({ "type": "object", "properties": {} }),
        false,
        true,
    );
    let handler: ToolHandler = Arc::new(|_invocation, context| tools::vs_current_solution(context));
    registry.register(definition, handler);
}

fn register_vs_current_document(registry: &mut ToolRegistry) {
    let name = VS_CURRENT_DOCUMENT_TOOL_NAME;
    let definition = build_definition(
        name,
        "Read the active Visual Studio text document through the connected VS Bridge. Stub returns bridge_not_connected.",
        json!({ "type": "object", "properties": {} }),
        false,
        true,
    );
    let handler: ToolHandler = Arc::new(|_invocation, context| tools::vs_current_document(context));
    registry.register(definition, handler);
}

fn register_vs_current_selection(registry: &mut ToolRegistry) {
    let name = VS_CURRENT_SELECTION_TOOL_NAME;
    let definition = build_definition(
        name,
        "Read the active Visual Studio text selection through the connected VS Bridge. Stub returns bridge_not_connected.",
        json!({ "type": "object", "properties": {} }),
        false,
        true,
    );
    let handler: ToolHandler =
        Arc::new(|_invocation, context| tools::vs_current_selection(context));
    registry.register(definition, handler);
}

fn register_vs_list_projects(registry: &mut ToolRegistry) {
    let name = VS_LIST_PROJECTS_TOOL_NAME;
    let definition = build_definition(
        name,
        "List projects currently loaded in the active Visual Studio solution. Stub returns bridge_not_connected.",
        json!({ "type": "object", "properties": {} }),
        false,
        true,
    );
    let handler: ToolHandler = Arc::new(|_invocation, context| tools::vs_list_projects(context));
    registry.register(definition, handler);
}

fn register_vs_find_definition(registry: &mut ToolRegistry) {
    let name = VS_FIND_DEFINITION_TOOL_NAME;
    let definition = build_definition(
        name,
        "Find the definition of a symbol through the connected VS Bridge. Stub returns bridge_not_connected.",
        json!({
            "type": "object",
            "properties": {
                "symbol": { "type": "string", "description": "Symbol name to locate." }
            },
            "required": ["symbol"]
        }),
        false,
        true,
    );
    let handler: ToolHandler = Arc::new(tools::vs_find_definition);
    registry.register(definition, handler);
}

fn register_vs_find_references(registry: &mut ToolRegistry) {
    let name = VS_FIND_REFERENCES_TOOL_NAME;
    let definition = build_definition(
        name,
        "Find references to a symbol through the connected VS Bridge. Stub returns bridge_not_connected.",
        json!({
            "type": "object",
            "properties": {
                "symbol": { "type": "string", "description": "Symbol name to find references for." }
            },
            "required": ["symbol"]
        }),
        false,
        true,
    );
    let handler: ToolHandler = Arc::new(tools::vs_find_references);
    registry.register(definition, handler);
}

fn register_vs_get_error_list(registry: &mut ToolRegistry) {
    let name = VS_GET_ERROR_LIST_TOOL_NAME;
    let definition = build_definition(
        name,
        "Read Visual Studio Error List diagnostics. Stub returns bridge_not_connected.",
        json!({ "type": "object", "properties": {} }),
        false,
        true,
    );
    let handler: ToolHandler = Arc::new(|_invocation, context| tools::vs_get_error_list(context));
    registry.register(definition, handler);
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

mod tools {
    use super::*;

    /// Maximum bytes an `Output` payload is allowed to occupy before we
    /// truncate it. The TUI transcript and the model context both dislike
    /// runaway reads.
    const MAX_OUTPUT_BYTES: usize = 256 * 1024;

    /// Directories that are always skipped by list_dir / search_content.
    const IGNORED_DIRS: &[&str] = &[
        ".git",
        ".vs",
        "bin",
        "obj",
        "build",
        "out",
        "node_modules",
        ".cache",
        "target",
    ];

    pub(super) fn read_file(
        invocation: ToolInvocation,
        context: ToolExecutionContext,
    ) -> ToolOutput {
        let started = Instant::now();
        let path = match read_workspace_path(&invocation.arguments, "path", &context) {
            Ok(path) => path,
            Err(err) => return err.to_output(started.elapsed().as_millis() as u64),
        };
        let start_line = invocation
            .arguments
            .get("start_line")
            .and_then(Value::as_u64)
            .map(|value| value.max(1) as usize);
        let end_line = invocation
            .arguments
            .get("end_line")
            .and_then(Value::as_u64)
            .map(|value| value.max(1) as usize);

        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) => {
                return ToolOutput::error(
                    format!("read_file: failed to read {}: {err}", path.display()),
                    started.elapsed().as_millis() as u64,
                );
            }
        };
        if let Some(kind) = looks_binary(&bytes) {
            return ToolOutput::error(
                format!("read_file: refusing to return binary file ({kind})"),
                started.elapsed().as_millis() as u64,
            );
        }
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        let total = lines.len();
        let start = start_line.unwrap_or(1);
        let end = end_line.unwrap_or(start.saturating_add(299).min(total));
        if start > total {
            return ToolOutput::error(
                format!("read_file: start_line {start} is past end of file ({total} lines)"),
                started.elapsed().as_millis() as u64,
            );
        }
        let mut body = String::new();
        for (idx, line) in lines.iter().enumerate().take(end).skip(start - 1) {
            body.push_str(&format!("{:>5}  {}\n", idx + 1, line));
        }
        truncate_output(&mut body);
        ToolOutput::ok_with_summary(
            json!({
                "path": relative_display(&path, &context),
                "totalLines": total,
                "startLine": start,
                "endLine": end.min(total),
                "text": body,
            }),
            started.elapsed().as_millis() as u64,
            format!(
                "read {} lines from {}",
                end.min(total).saturating_sub(start - 1) + 1,
                path.display()
            ),
        )
    }

    pub(super) fn list_dir(
        invocation: ToolInvocation,
        context: ToolExecutionContext,
    ) -> ToolOutput {
        let started = Instant::now();
        let path = match read_workspace_path(&invocation.arguments, "path", &context) {
            Ok(path) => path,
            Err(err) => return err.to_output(started.elapsed().as_millis() as u64),
        };
        let entries = match std::fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(err) => {
                return ToolOutput::error(
                    format!("list_dir: failed to read {}: {err}", path.display()),
                    started.elapsed().as_millis() as u64,
                );
            }
        };
        let mut dirs: Vec<String> = Vec::new();
        let mut files: Vec<String> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if IGNORED_DIRS.contains(&name.as_str()) {
                continue;
            }
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                dirs.push(name);
            } else if file_type.is_file() {
                files.push(name);
            }
        }
        dirs.sort();
        files.sort();
        ToolOutput::ok_with_summary(
            json!({
                "path": relative_display(&path, &context),
                "dirs": dirs,
                "files": files,
            }),
            started.elapsed().as_millis() as u64,
            format!("{} directories, {} files", dirs.len(), files.len()),
        )
    }

    pub(super) fn search_content(
        invocation: ToolInvocation,
        context: ToolExecutionContext,
    ) -> ToolOutput {
        let started = Instant::now();
        let query = match invocation.arguments.get("query").and_then(Value::as_str) {
            Some(query) => query,
            None => {
                return ToolOutput::error(
                    "search: missing 'query' argument".to_string(),
                    started.elapsed().as_millis() as u64,
                );
            }
        };
        let root = if invocation.arguments.get("root").is_some() {
            match read_workspace_path(&invocation.arguments, "root", &context) {
                Ok(path) => path,
                Err(err) => return err.to_output(started.elapsed().as_millis() as u64),
            }
        } else {
            match context.workspace_root.as_ref() {
                Some(root) => root.clone(),
                None => {
                    return ToolOutput::error(
                        "search: no workspace root configured for this tool".to_string(),
                        started.elapsed().as_millis() as u64,
                    );
                }
            }
        };
        let max_results = invocation
            .arguments
            .get("max_results")
            .and_then(Value::as_u64)
            .unwrap_or(100) as usize;
        let context_lines = invocation
            .arguments
            .get("context_lines")
            .and_then(Value::as_u64)
            .unwrap_or(2) as usize;
        let case_sensitive = invocation
            .arguments
            .get("case_sensitive")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let use_regex = invocation
            .arguments
            .get("regex")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let file_glob = invocation
            .arguments
            .get("file_glob")
            .and_then(Value::as_str)
            .map(str::to_string);

        let matcher = match build_matcher(query, case_sensitive, use_regex) {
            Ok(matcher) => matcher,
            Err(err) => {
                return ToolOutput::error(
                    format!("search: {err}"),
                    started.elapsed().as_millis() as u64,
                );
            }
        };

        let mut matches = Vec::new();
        let mut truncated = false;
        let walk_error = walk_files(&root, &file_glob, &mut |path| {
            if matches.len() >= max_results {
                truncated = true;
                return false;
            }
            let Ok(bytes) = std::fs::read(path) else {
                return true;
            };
            if looks_binary(&bytes).is_some() {
                return true;
            }
            let text = String::from_utf8_lossy(&bytes);
            for (idx, line) in text.lines().enumerate() {
                if let Some(hit) = matcher(line) {
                    let (before, after) = context_lines_for(&text, idx, context_lines);
                    matches.push(json!({
                        "file": relative_display(path, &context),
                        "line": idx + 1,
                        "column": hit + 1,
                        "text": line,
                        "before": before,
                        "after": after,
                    }));
                    if matches.len() >= max_results {
                        truncated = true;
                        return false;
                    }
                }
            }
            true
        });
        if let Err(err) = walk_error {
            return ToolOutput::error(
                format!("search: failed to walk {}: {err}", root.display()),
                started.elapsed().as_millis() as u64,
            );
        }
        let count = matches.len();
        ToolOutput::ok_with_summary(
            json!({
                "root": relative_display(&root, &context),
                "query": query,
                "matches": matches,
                "truncated": truncated,
            }),
            started.elapsed().as_millis() as u64,
            format!("{count} match{}", if count == 1 { "" } else { "es" }),
        )
    }

    pub(super) fn edit_file(
        invocation: ToolInvocation,
        context: ToolExecutionContext,
    ) -> ToolOutput {
        let started = Instant::now();
        let path = match read_workspace_path(&invocation.arguments, "file", &context) {
            Ok(path) => path,
            Err(err) => return err.to_output(started.elapsed().as_millis() as u64),
        };
        let search = match invocation.arguments.get("search").and_then(Value::as_str) {
            Some(search) => search,
            None => {
                return ToolOutput::error(
                    "edit_file: missing 'search' argument".to_string(),
                    started.elapsed().as_millis() as u64,
                );
            }
        };
        let replace = match invocation.arguments.get("replace").and_then(Value::as_str) {
            Some(replace) => replace,
            None => {
                return ToolOutput::error(
                    "edit_file: missing 'replace' argument".to_string(),
                    started.elapsed().as_millis() as u64,
                );
            }
        };
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) => {
                return ToolOutput::error(
                    format!("edit_file: failed to read {}: {err}", path.display()),
                    started.elapsed().as_millis() as u64,
                );
            }
        };
        if looks_binary(&bytes).is_some() {
            return ToolOutput::error(
                "edit_file: refusing to edit binary file".to_string(),
                started.elapsed().as_millis() as u64,
            );
        }
        let text = String::from_utf8_lossy(&bytes);
        let occurrences = text.matches(search).count();
        if occurrences == 0 {
            return ToolOutput::error(
                format!("edit_file: search text not found in {}", path.display()),
                started.elapsed().as_millis() as u64,
            );
        }
        if occurrences > 1 {
            return ToolOutput::error(
                format!(
                    "edit_file: search text occurs {occurrences} times; expected exactly 1. Narrow the search block."
                ),
                started.elapsed().as_millis() as u64,
            );
        }
        let updated = text.replacen(search, replace, 1).to_string();
        if let Err(err) = std::fs::write(&path, updated.as_bytes()) {
            return ToolOutput::error(
                format!("edit_file: failed to write {}: {err}", path.display()),
                started.elapsed().as_millis() as u64,
            );
        }
        ToolOutput::ok_with_summary(
            json!({
                "file": relative_display(&path, &context),
                "replacements": 1,
            }),
            started.elapsed().as_millis() as u64,
            format!("updated {}", path.display()),
        )
    }

    pub(super) fn goal_get(
        _invocation: ToolInvocation,
        context: ToolExecutionContext,
    ) -> ToolOutput {
        let started = Instant::now();
        let current = current_goal(&context);
        match current {
            Some(goal) => {
                let elapsed_seconds = compute_elapsed_seconds(&goal);
                ToolOutput::ok_with_summary(
                    json!({
                        "active": true,
                        "objective": goal.objective,
                        "status": goal.status.label(),
                        "tokenBudget": goal.token_budget,
                        "tokensUsed": goal.tokens_used,
                        "timeUsedSeconds": elapsed_seconds,
                        "createdAt": goal.created_at,
                        "updatedAt": goal.updated_at,
                    }),
                    started.elapsed().as_millis() as u64,
                    format!(
                        "goal: {} ({})",
                        truncate_for_summary(&goal.objective, 80),
                        goal.status.label()
                    ),
                )
            }
            None => ToolOutput::ok_with_summary(
                json!({ "active": false, "message": "no active goal" }),
                started.elapsed().as_millis() as u64,
                "no active goal".to_string(),
            ),
        }
    }

    pub(super) fn goal_set(
        invocation: ToolInvocation,
        context: ToolExecutionContext,
    ) -> ToolOutput {
        let started = Instant::now();
        let objective = match invocation
            .arguments
            .get("objective")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(value) => value.to_string(),
            None => {
                return ToolOutput::error(
                    "goal/set requires a non-empty 'objective' string argument".to_string(),
                    started.elapsed().as_millis() as u64,
                );
            }
        };
        let token_budget = invocation
            .arguments
            .get("tokenBudget")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0);

        let mut goal = GoalState::new(objective.clone());
        goal.token_budget = token_budget;

        if let Some(home) = context.codeforge_home.as_ref() {
            if let Err(err) = crate::codeforge_goal_state::save(home, &goal) {
                return ToolOutput::error(
                    format!("goal/set: failed to persist goal: {err}"),
                    started.elapsed().as_millis() as u64,
                );
            }
        }

        if let Some(slot) = context.goal_slot.as_ref() {
            if let Ok(mut guard) = slot.write() {
                *guard = Some(goal.clone());
            }
        }

        ToolOutput::ok_with_summary(
            json!({
                "set": true,
                "objective": goal.objective,
                "status": goal.status.label(),
                "tokenBudget": goal.token_budget,
                "tokensUsed": goal.tokens_used,
                "timeUsedSeconds": goal.time_used_seconds,
                "createdAt": goal.created_at,
                "updatedAt": goal.updated_at,
            }),
            started.elapsed().as_millis() as u64,
            format!("set goal: {}", truncate_for_summary(&goal.objective, 80)),
        )
    }

    pub(super) fn goal_clear(
        _invocation: ToolInvocation,
        context: ToolExecutionContext,
    ) -> ToolOutput {
        let started = Instant::now();
        let previous = current_goal(&context);
        let mut removed = false;
        if let Some(home) = context.codeforge_home.as_ref() {
            match crate::codeforge_goal_state::clear(home) {
                Ok(did_remove) => removed = did_remove,
                Err(err) => {
                    return ToolOutput::error(
                        format!("goal/clear: failed to remove goal: {err}"),
                        started.elapsed().as_millis() as u64,
                    );
                }
            }
        }
        if let Some(slot) = context.goal_slot.as_ref() {
            if let Ok(mut guard) = slot.write() {
                *guard = None;
            }
        }
        let payload = match previous {
            Some(ref goal) => json!({
                "cleared": true,
                "removedFromDisk": removed,
                "previous": {
                    "objective": goal.objective,
                    "status": goal.status.label(),
                    "tokenBudget": goal.token_budget,
                },
            }),
            None => json!({ "cleared": true, "removedFromDisk": removed }),
        };
        let summary = match previous.as_ref() {
            Some(goal) => format!(
                "cleared goal: {}",
                truncate_for_summary(&goal.objective, 80)
            ),
            None => "cleared goal (none was active)".to_string(),
        };
        ToolOutput::ok_with_summary(payload, started.elapsed().as_millis() as u64, summary)
    }

    pub(super) fn vs_current_solution(context: ToolExecutionContext) -> ToolOutput {
        let started = Instant::now();
        let endpoint = context.vs_bridge_endpoint.clone();
        match block_on(async move {
            crate::codeforge_vs_bridge::call_current_solution(endpoint.as_deref()).await
        }) {
            Ok(value) => ToolOutput::ok(value, started.elapsed().as_millis() as u64),
            Err(err) => ToolOutput::error(err, started.elapsed().as_millis() as u64),
        }
    }

    pub(super) fn vs_current_document(context: ToolExecutionContext) -> ToolOutput {
        let started = Instant::now();
        let endpoint = context.vs_bridge_endpoint.clone();
        match block_on(async move {
            crate::codeforge_vs_bridge::call_current_document(endpoint.as_deref()).await
        }) {
            Ok(value) => ToolOutput::ok(value, started.elapsed().as_millis() as u64),
            Err(err) => ToolOutput::error(err, started.elapsed().as_millis() as u64),
        }
    }

    pub(super) fn vs_current_selection(context: ToolExecutionContext) -> ToolOutput {
        let started = Instant::now();
        let endpoint = context.vs_bridge_endpoint.clone();
        match block_on(async move {
            crate::codeforge_vs_bridge::call_current_selection(endpoint.as_deref()).await
        }) {
            Ok(value) => ToolOutput::ok(value, started.elapsed().as_millis() as u64),
            Err(err) => ToolOutput::error(err, started.elapsed().as_millis() as u64),
        }
    }

    pub(super) fn vs_list_projects(context: ToolExecutionContext) -> ToolOutput {
        let started = Instant::now();
        let endpoint = context.vs_bridge_endpoint.clone();
        match block_on(async move {
            crate::codeforge_vs_bridge::call_list_projects(endpoint.as_deref()).await
        }) {
            Ok(value) => ToolOutput::ok(value, started.elapsed().as_millis() as u64),
            Err(err) => ToolOutput::error(err, started.elapsed().as_millis() as u64),
        }
    }

    pub(super) fn vs_find_definition(
        invocation: ToolInvocation,
        context: ToolExecutionContext,
    ) -> ToolOutput {
        let started = Instant::now();
        let endpoint = context.vs_bridge_endpoint.clone();
        let arguments = invocation.arguments.clone();
        match block_on(async move {
            crate::codeforge_vs_bridge::call_find_definition(endpoint.as_deref(), &arguments).await
        }) {
            Ok(value) => ToolOutput::ok(value, started.elapsed().as_millis() as u64),
            Err(err) => ToolOutput::error(err, started.elapsed().as_millis() as u64),
        }
    }

    pub(super) fn vs_find_references(
        invocation: ToolInvocation,
        context: ToolExecutionContext,
    ) -> ToolOutput {
        let started = Instant::now();
        let endpoint = context.vs_bridge_endpoint.clone();
        let arguments = invocation.arguments.clone();
        match block_on(async move {
            crate::codeforge_vs_bridge::call_find_references(endpoint.as_deref(), &arguments).await
        }) {
            Ok(value) => ToolOutput::ok(value, started.elapsed().as_millis() as u64),
            Err(err) => ToolOutput::error(err, started.elapsed().as_millis() as u64),
        }
    }

    pub(super) fn vs_get_error_list(context: ToolExecutionContext) -> ToolOutput {
        let started = Instant::now();
        let endpoint = context.vs_bridge_endpoint.clone();
        match block_on(async move {
            crate::codeforge_vs_bridge::call_get_error_list(endpoint.as_deref()).await
        }) {
            Ok(value) => ToolOutput::ok(value, started.elapsed().as_millis() as u64),
            Err(err) => ToolOutput::error(err, started.elapsed().as_millis() as u64),
        }
    }

    /// Drive a short async bridge call to completion from a sync handler.
    /// Current-thread Tokio runtimes cannot use `block_in_place`, so those
    /// calls run on a temporary runtime in a helper thread.
    fn block_on<F>(future: F) -> F::Output
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => match handle.runtime_flavor() {
                tokio::runtime::RuntimeFlavor::MultiThread => {
                    tokio::task::block_in_place(|| handle.block_on(future))
                }
                _ => block_on_new_thread(future),
            },
            Err(_) => block_on_new_thread(future),
        }
    }

    fn block_on_new_thread<F>(future: F) -> F::Output
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("vs bridge: failed to build fallback tokio runtime");
            runtime.block_on(future)
        })
        .join()
        .expect("vs bridge: fallback runtime thread panicked")
    }
    /// Snapshot the current goal from the live slot, falling back to the
    /// on-disk file under `.codeforge/goal.json` when the slot is empty
    /// or absent.
    fn current_goal(context: &ToolExecutionContext) -> Option<GoalState> {
        if let Some(slot) = context.goal_slot.as_ref()
            && let Ok(guard) = slot.read()
            && let Some(goal) = guard.as_ref()
        {
            return Some(goal.clone());
        }
        context
            .codeforge_home
            .as_ref()
            .and_then(|home| crate::codeforge_goal_state::load(home).ok().flatten())
    }

    /// Re-derive elapsed seconds from `created_at` so the value shown by
    /// `goal/get` reflects wall-clock time, not the persisted snapshot.
    fn compute_elapsed_seconds(goal: &GoalState) -> i64 {
        let started_at = chrono::DateTime::parse_from_rfc3339(&goal.created_at)
            .ok()
            .map(|value| value.with_timezone(&chrono::Utc));
        match started_at {
            Some(started) => (chrono::Utc::now() - started).num_seconds().max(0),
            None => goal.time_used_seconds,
        }
    }

    /// Trim a string to `max_chars` for short tool summaries.
    fn truncate_for_summary(text: &str, max_chars: usize) -> String {
        let chars: Vec<char> = text.chars().collect();
        if chars.len() <= max_chars {
            return text.to_string();
        }
        if max_chars <= 1 {
            return chars.into_iter().take(max_chars).collect();
        }
        let mut summary: String = chars.into_iter().take(max_chars - 1).collect();
        summary.push('\u{2026}');
        summary
    }

    // -----------------------------------------------------------------------
    // Shared helpers
    // -----------------------------------------------------------------------

    fn truncate_output(body: &mut String) {
        if body.len() > MAX_OUTPUT_BYTES {
            body.truncate(MAX_OUTPUT_BYTES);
            body.push_str("\n... [truncated]");
        }
    }

    fn looks_binary(bytes: &[u8]) -> Option<&'static str> {
        if bytes.contains(&0) {
            return Some("contains NUL byte");
        }
        let sample_len = bytes.len().min(4096);
        let mut suspicious = 0usize;
        for &byte in &bytes[..sample_len] {
            if byte < 0x09 || (byte > 0x0D && byte < 0x20 && byte != 0x1B) {
                suspicious += 1;
            }
        }
        if sample_len > 0 && suspicious * 10 > sample_len {
            return Some("non-printable content");
        }
        None
    }

    fn context_lines_for(
        text: &str,
        idx: usize,
        context_lines: usize,
    ) -> (Vec<String>, Vec<String>) {
        let lines: Vec<&str> = text.lines().collect();
        let start = idx.saturating_sub(context_lines);
        let end = (idx + context_lines + 1).min(lines.len());
        let before = lines[start..idx]
            .iter()
            .map(|line| line.to_string())
            .collect();
        let after = lines[(idx + 1)..end]
            .iter()
            .map(|line| line.to_string())
            .collect();
        (before, after)
    }

    type WalkFn<'a> = dyn FnMut(&Path) -> bool + 'a;

    fn walk_files(
        root: &Path,
        file_glob: &Option<String>,
        visit: &mut WalkFn<'_>,
    ) -> std::io::Result<()> {
        for entry in std::fs::read_dir(root)? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let name = entry.file_name().to_string_lossy().to_string();
            if IGNORED_DIRS.contains(&name.as_str()) {
                continue;
            }
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                walk_files(&path, file_glob, visit)?;
            } else if file_type.is_file() {
                if let Some(glob) = file_glob {
                    if !glob_matches(glob, &name) {
                        continue;
                    }
                }
                if !visit(&path) {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn glob_matches(glob: &str, name: &str) -> bool {
        if glob == "*" || glob == "*.*" {
            return true;
        }
        if !glob.contains('*') {
            return name == glob;
        }
        let mut pattern = String::from("^");
        for ch in glob.chars() {
            match ch {
                '*' => pattern.push_str(".*"),
                '.' | '(' | ')' | '+' | '|' | '^' | '$' | '{' | '}' | '[' | ']' | '\\' => {
                    pattern.push('\\');
                    pattern.push(ch);
                }
                _ => pattern.push(ch),
            }
        }
        pattern.push('$');
        regex_lite::Regex::new(&pattern)
            .map(|regex| regex.is_match(name))
            .unwrap_or(false)
    }

    fn build_matcher(
        query: &str,
        case_sensitive: bool,
        use_regex: bool,
    ) -> Result<Box<dyn Fn(&str) -> Option<usize> + '_>, String> {
        if use_regex {
            let regex = regex_lite::RegexBuilder::new(query)
                .case_insensitive(!case_sensitive)
                .build()
                .map_err(|err| format!("invalid regex: {err}"))?;
            Ok(Box::new(move |line| regex.find(line).map(|m| m.start())))
        } else if case_sensitive {
            Ok(Box::new(move |line| line.find(query)))
        } else {
            let needle = query.to_lowercase();
            Ok(Box::new(move |line| line.to_lowercase().find(&needle)))
        }
    }

    struct PathError {
        message: String,
    }

    impl PathError {
        fn to_output(self, elapsed_ms: u64) -> ToolOutput {
            ToolOutput::error(self.message, elapsed_ms)
        }
    }

    fn read_workspace_path(
        arguments: &Value,
        key: &str,
        context: &ToolExecutionContext,
    ) -> Result<PathBuf, PathError> {
        let raw = arguments
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| PathError {
                message: format!("missing '{key}' argument"),
            })?;
        let workspace_root = context.workspace_root.as_ref().ok_or_else(|| PathError {
            message: "no workspace root configured for this tool".to_string(),
        })?;
        let candidate = PathBuf::from(raw);
        let absolute = if candidate.is_absolute() {
            candidate
        } else {
            workspace_root.join(candidate)
        };
        let normalized = absolute.canonicalize().map_err(|err| PathError {
            message: format!("failed to resolve '{raw}': {err}"),
        })?;
        if !normalized.starts_with(workspace_root) {
            return Err(PathError {
                message: format!("path '{raw}' escapes workspace root"),
            });
        }
        Ok(normalized)
    }

    fn relative_display(path: &Path, context: &ToolExecutionContext) -> String {
        match context.workspace_root.as_ref() {
            Some(root) => match path.strip_prefix(root) {
                Ok(relative) => relative.to_string_lossy().to_string(),
                Err(_) => path.to_string_lossy().to_string(),
            },
            None => path.to_string_lossy().to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    fn ctx(root: &Path) -> ToolExecutionContext {
        ToolExecutionContext {
            workspace_root: Some(root.to_path_buf()),
            codeforge_home: None,
            turn_id: None,
            thread_id: None,
            vs_bridge_endpoint: None,
            goal_slot: None,
        }
    }

    #[test]
    fn register_lists_definitions_in_sorted_order() {
        let mut registry = ToolRegistry::new();
        let handler: ToolHandler = Arc::new(|_, _| ToolOutput::ok(json!({}), 0));
        for name in ["workspace/read_file", "goal/get", "workspace/list_dir"] {
            registry.register(
                build_definition(name, "test", json!({"type": "object"}), false, true),
                handler.clone(),
            );
        }
        let names: Vec<String> = registry
            .list_definitions()
            .into_iter()
            .map(|def| def.name)
            .collect();
        assert_eq!(
            names,
            vec![
                "goal/get".to_string(),
                "workspace/list_dir".to_string(),
                "workspace/read_file".to_string(),
            ]
        );
    }

    #[test]
    fn register_rejects_duplicates_and_freeze() {
        let mut registry = ToolRegistry::new();
        let handler: ToolHandler = Arc::new(|_, _| ToolOutput::ok(json!({}), 0));
        assert!(registry.register(
            build_definition("a", "test", json!({}), false, true),
            handler.clone(),
        ));
        assert!(!registry.register(
            build_definition("a", "test", json!({}), false, true),
            handler.clone(),
        ));
        registry.freeze();
        assert!(!registry.register(
            build_definition("b", "test", json!({}), false, true),
            handler.clone(),
        ));
    }

    #[test]
    fn dispatch_emits_started_and_completed_events() {
        let mut registry = ToolRegistry::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();
        let handler: ToolHandler = Arc::new(move |_, _| {
            counter_clone.fetch_add(1, Ordering::SeqCst);
            ToolOutput::ok(json!({"hello": "world"}), 0)
        });
        registry.register(
            build_definition("demo/echo", "test", json!({}), false, true),
            handler,
        );
        let mut events = Vec::new();
        let output = registry.dispatch(
            ToolInvocation {
                tool_name: "demo/echo".to_string(),
                arguments: json!({}),
                call_id: Some("call-1".to_string()),
            },
            ctx(Path::new(".")),
            |event| events.push(event),
        );
        assert!(output.is_ok());
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], ToolTraceEvent::Started { .. }));
        assert!(matches!(events[1], ToolTraceEvent::Completed { .. }));
    }

    #[test]
    fn dispatch_unknown_tool_returns_error_event() {
        let registry = ToolRegistry::new();
        let mut events = Vec::new();
        let output = registry.dispatch(
            ToolInvocation {
                tool_name: "missing".to_string(),
                arguments: json!({}),
                call_id: None,
            },
            ctx(Path::new(".")),
            |event| events.push(event),
        );
        assert!(!output.is_ok());
        assert!(output.error.unwrap().contains("unknown tool"));
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn openai_tools_format_includes_function_wrapper() {
        let mut registry = ToolRegistry::new();
        let handler: ToolHandler = Arc::new(|_, _| ToolOutput::ok(json!({}), 0));
        registry.register(
            build_definition(
                "workspace/read_file",
                "Read a file",
                json!({"type": "object", "properties": {"path": {"type": "string"}}}),
                false,
                true,
            ),
            handler,
        );
        let formatted = registry.openai_tools();
        assert_eq!(formatted[0]["type"], "function");
        assert_eq!(formatted[0]["function"]["name"], "workspace/read_file");
    }

    #[test]
    fn read_file_returns_line_numbered_output() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("hello.txt");
        std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();
        let output = tools::read_file(
            ToolInvocation {
                tool_name: WORKSPACE_READ_FILE_TOOL_NAME.to_string(),
                arguments: json!({ "path": "hello.txt" }),
                call_id: None,
            },
            ctx(dir.path()),
        );
        assert!(output.is_ok(), "{:?}", output);
        let value = output.output.unwrap();
        assert_eq!(value["totalLines"], 3);
        assert!(value["text"].as_str().unwrap().contains("1  alpha"));
        assert!(value["text"].as_str().unwrap().contains("3  gamma"));
    }

    #[test]
    fn read_file_rejects_path_outside_workspace() {
        let dir = TempDir::new().unwrap();
        let output = tools::read_file(
            ToolInvocation {
                tool_name: WORKSPACE_READ_FILE_TOOL_NAME.to_string(),
                arguments: json!({ "path": "../etc/passwd" }),
                call_id: None,
            },
            ctx(dir.path()),
        );
        assert!(!output.is_ok());
        assert!(output.error.unwrap().contains("escapes workspace"));
    }

    #[test]
    fn read_file_rejects_binary() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("blob.bin");
        std::fs::write(&path, [0u8, 1, 2, 3, 0]).unwrap();
        let output = tools::read_file(
            ToolInvocation {
                tool_name: WORKSPACE_READ_FILE_TOOL_NAME.to_string(),
                arguments: json!({ "path": "blob.bin" }),
                call_id: None,
            },
            ctx(dir.path()),
        );
        assert!(!output.is_ok());
    }

    #[test]
    fn list_dir_skips_ignored_directories() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("README.md"), "hello").unwrap();
        let output = tools::list_dir(
            ToolInvocation {
                tool_name: WORKSPACE_LIST_DIR_TOOL_NAME.to_string(),
                arguments: json!({ "path": "." }),
                call_id: None,
            },
            ctx(dir.path()),
        );
        assert!(output.is_ok());
        let value = output.output.unwrap();
        let dirs: Vec<String> = value["dirs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        let files: Vec<String> = value["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(dirs.contains(&"src".to_string()));
        assert!(!dirs.contains(&".git".to_string()));
        assert!(files.contains(&"README.md".to_string()));
    }

    #[test]
    fn search_content_returns_structured_matches() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("a.txt"),
            "first line\nsecond alpha\nthird alpha\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("b.txt"), "no match here\n").unwrap();
        let output = tools::search_content(
            ToolInvocation {
                tool_name: WORKSPACE_SEARCH_TOOL_NAME.to_string(),
                arguments: json!({ "query": "alpha", "root": "." }),
                call_id: None,
            },
            ctx(dir.path()),
        );
        assert!(output.is_ok());
        let value = output.output.unwrap();
        let matches = value["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0]["file"], "a.txt");
        assert_eq!(matches[0]["line"], 2);
    }

    #[test]
    fn edit_file_replaces_exactly_one_occurrence() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sample.txt");
        std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();
        let output = tools::edit_file(
            ToolInvocation {
                tool_name: WORKSPACE_APPLY_PATCH_TOOL_NAME.to_string(),
                arguments: json!({
                    "file": "sample.txt",
                    "search": "beta",
                    "replace": "BETA",
                }),
                call_id: None,
            },
            ctx(dir.path()),
        );
        assert!(output.is_ok(), "{:?}", output);
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body, "alpha\nBETA\ngamma\n");
    }

    #[test]
    fn edit_file_rejects_ambiguous_match() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sample.txt");
        std::fs::write(&path, "alpha\nalpha\n").unwrap();
        let output = tools::edit_file(
            ToolInvocation {
                tool_name: WORKSPACE_APPLY_PATCH_TOOL_NAME.to_string(),
                arguments: json!({
                    "file": "sample.txt",
                    "search": "alpha",
                    "replace": "beta",
                }),
                call_id: None,
            },
            ctx(dir.path()),
        );
        assert!(!output.is_ok());
        let err = output.error.unwrap();
        assert!(err.contains("2 times"));
    }

    #[test]
    fn default_registry_exposes_plan_tool_names() {
        let registry = default_registry();
        let names: HashSet<String> = registry
            .list_definitions()
            .into_iter()
            .map(|def| def.name)
            .collect();
        for required in [
            WORKSPACE_READ_FILE_TOOL_NAME,
            WORKSPACE_LIST_DIR_TOOL_NAME,
            WORKSPACE_SEARCH_TOOL_NAME,
            WORKSPACE_APPLY_PATCH_TOOL_NAME,
            GOAL_GET_TOOL_NAME,
            GOAL_SET_TOOL_NAME,
            GOAL_CLEAR_TOOL_NAME,
            VS_CURRENT_SOLUTION_TOOL_NAME,
            VS_CURRENT_DOCUMENT_TOOL_NAME,
            VS_CURRENT_SELECTION_TOOL_NAME,
            VS_LIST_PROJECTS_TOOL_NAME,
            VS_FIND_DEFINITION_TOOL_NAME,
            VS_FIND_REFERENCES_TOOL_NAME,
            VS_GET_ERROR_LIST_TOOL_NAME,
        ] {
            assert!(
                names.contains(required),
                "default registry missing {required}"
            );
        }
    }
}
