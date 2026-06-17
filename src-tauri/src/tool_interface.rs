//! CodeForge Tool Interface Types
//!
//! Defines the typed interface for tool definitions, schemas, invocations,
//! outputs, errors, and approval requests. These types are the contract
//! between the tool registry, agent runner, trace system, and UI.
//!
//! The goal is to make every tool call fully traceable and the interface
//! shape explicit before adding more tools.

use std::fmt;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// ToolDefinition — describes a tool to the model and to the UI
// ---------------------------------------------------------------------------

/// A fully-defined tool with name, description, and JSON Schema parameters.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinition {
    /// The tool name, e.g. "workspace/search" or "vs/current_solution".
    pub name: String,
    /// Human-readable description shown to the model and in trace UI.
    pub description: String,
    /// JSON Schema for the tool's input parameters.
    pub parameters: Value,
    /// Whether this tool requires explicit user approval before execution.
    pub requires_approval: bool,
    /// Whether this tool is read-only (never modifies files or state).
    pub read_only: bool,
    /// Namespace grouping, e.g. "workspace" or "vs" or "goal".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

impl ToolDefinition {
    /// Convert to the OpenAI function-call format sent to the model.
    pub fn to_openai_function(&self) -> Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            }
        })
    }
}

// ---------------------------------------------------------------------------
// ToolSchema — describes the schema part of a tool definition
// ---------------------------------------------------------------------------

/// The parameter schema for a tool. Wraps a JSON Schema value with helpers.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSchema {
    /// The JSON Schema describing the tool's input parameters.
    pub schema: Value,
}

// ---------------------------------------------------------------------------
// ToolInvocation — a request to execute a tool
// ---------------------------------------------------------------------------

/// A concrete invocation of a tool with arguments.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInvocation {
    /// The tool name being invoked.
    pub tool_name: String,
    /// The input arguments as a JSON value.
    pub arguments: Value,
    /// An optional call ID linking this invocation to the model's tool call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
}

impl fmt::Display for ToolInvocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}({})", self.tool_name, self.arguments)
    }
}

// ---------------------------------------------------------------------------
// ToolOutput — the result of executing a tool
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

impl fmt::Display for ToolOutputStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolOutputStatus::Ok => write!(f, "ok"),
            ToolOutputStatus::Error => write!(f, "error"),
            ToolOutputStatus::Timeout => write!(f, "timeout"),
            ToolOutputStatus::Rejected => write!(f, "rejected"),
        }
    }
}

/// The output of a tool execution.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolOutput {
    /// Execution status.
    pub status: ToolOutputStatus,
    /// The structured output value (on success or partial success).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    /// Error message when status is Error, Timeout, or Rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Execution duration in milliseconds.
    pub elapsed_ms: u64,
    /// A short human-readable summary of the output for trace display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl ToolOutput {
    /// Create a successful output.
    pub fn ok(output: Value, elapsed_ms: u64) -> Self {
        Self {
            status: ToolOutputStatus::Ok,
            output: Some(output),
            error: None,
            elapsed_ms,
            summary: None,
        }
    }

    /// Create a successful output with a summary.
    pub fn ok_with_summary(output: Value, elapsed_ms: u64, summary: String) -> Self {
        Self {
            status: ToolOutputStatus::Ok,
            output: Some(output),
            error: None,
            elapsed_ms,
            summary: Some(summary),
        }
    }

    /// Create an error output.
    pub fn error(message: String, elapsed_ms: u64) -> Self {
        Self {
            status: ToolOutputStatus::Error,
            output: None,
            error: Some(message),
            elapsed_ms,
            summary: None,
        }
    }

    /// Create a timeout output.
    pub fn timeout(elapsed_ms: u64) -> Self {
        Self {
            status: ToolOutputStatus::Timeout,
            output: None,
            error: Some("Tool execution timed out".to_string()),
            elapsed_ms,
            summary: None,
        }
    }

    /// Create a rejected output.
    pub fn rejected(reason: String) -> Self {
        Self {
            status: ToolOutputStatus::Rejected,
            output: None,
            error: Some(reason),
            elapsed_ms: 0,
            summary: None,
        }
    }

    /// Convert to the value format used by the model.
    pub fn to_model_value(&self) -> Value {
        serde_json::json!({
            "status": self.status.to_string(),
            "ok": self.status == ToolOutputStatus::Ok,
            "output": self.output,
            "error": self.error,
            "elapsedMs": self.elapsed_ms,
        })
    }

    /// Whether the execution succeeded.
    pub fn is_ok(&self) -> bool {
        self.status == ToolOutputStatus::Ok
    }
}

// ---------------------------------------------------------------------------
// ToolError — structured tool execution error
// ---------------------------------------------------------------------------

/// A structured error from tool execution.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolError {
    /// The tool name that produced the error.
    pub tool_name: String,
    /// Error category.
    pub kind: ToolErrorKind,
    /// Human-readable error message.
    pub message: String,
}

/// Category of tool error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorKind {
    /// The tool name is not recognized.
    UnknownTool,
    /// The input arguments failed validation.
    InvalidArguments,
    /// The tool execution failed.
    ExecutionFailed,
    /// The tool execution timed out.
    Timeout,
    /// The tool execution was rejected by policy or user.
    Rejected,
    /// The tool requires approval that was not granted.
    ApprovalRequired,
}

// ---------------------------------------------------------------------------
// ToolApprovalRequest — when a tool needs user approval
// ---------------------------------------------------------------------------

/// A request for user approval before executing a tool.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolApprovalRequest {
    /// The tool invocation that needs approval.
    pub invocation: ToolInvocation,
    /// Why approval is needed.
    pub reason: String,
    /// The tool definition for reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_definition: Option<ToolDefinition>,
}

// ---------------------------------------------------------------------------
// ToolTraceEvent — a trace event for tool execution
// ---------------------------------------------------------------------------

/// A trace event representing a tool call, its execution, and its result.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallTrace {
    /// The tool invocation.
    pub invocation: ToolInvocation,
    /// The tool output (set after execution completes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<ToolOutput>,
    /// The approval request, if one was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<ToolApprovalRequest>,
    /// Whether the approval was granted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_granted: Option<bool>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_definition_to_openai_format() {
        let def = ToolDefinition {
            name: "workspace/search".to_string(),
            description: "Search workspace files.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" }
                },
                "required": ["query"]
            }),
            requires_approval: false,
            read_only: true,
            namespace: Some("workspace".to_string()),
        };
        let value = def.to_openai_function();
        assert_eq!(value["type"], "function");
        assert_eq!(value["function"]["name"], "workspace/search");
    }

    #[test]
    fn tool_output_ok_builds_correctly() {
        let output = ToolOutput::ok(json!({"result": 42}), 10);
        assert!(output.is_ok());
        assert_eq!(output.status, ToolOutputStatus::Ok);
        assert_eq!(output.elapsed_ms, 10);
    }

    #[test]
    fn tool_output_error_builds_correctly() {
        let output = ToolOutput::error("file not found".to_string(), 5);
        assert!(!output.is_ok());
        assert_eq!(output.status, ToolOutputStatus::Error);
    }

    #[test]
    fn tool_output_rejected_builds_correctly() {
        let output = ToolOutput::rejected("policy denied".to_string());
        assert_eq!(output.status, ToolOutputStatus::Rejected);
        assert_eq!(output.elapsed_ms, 0);
    }

    #[test]
    fn tool_output_to_model_value_roundtrip() {
        let output =
            ToolOutput::ok_with_summary(json!({"files": 3}), 25, "Found 3 files".to_string());
        let value = output.to_model_value();
        assert_eq!(value["ok"], true);
        assert_eq!(value["status"], "ok");
        assert_eq!(value["elapsedMs"], 25);
    }

    #[test]
    fn tool_invocation_display() {
        let inv = ToolInvocation {
            tool_name: "workspace/search".to_string(),
            arguments: json!({"query": "main"}),
            call_id: Some("call_123".to_string()),
        };
        let display = format!("{inv}");
        assert!(display.contains("workspace/search"));
    }
}
