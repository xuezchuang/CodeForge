//! CodeForge-owned tool trace surface for the TUI.
//!
//! Phase 2 of the CodeForge TUI backend-and-tooling plan. Trace events are
//! the TUI-owned equivalent of the Tauri `tool_trace` module. The TUI does
//! not depend on the Tauri desktop crate, so the TUI carries its own
//! trace shapes keyed by the TUI tool registry types
//! ([`crate::codeforge_tool_registry::ToolInvocation`],
//! [`crate::codeforge_tool_registry::ToolOutput`]).
//!
//! Phase 6 of the plan extends this module with persistence and richer
//! run-time statistics. For Phase 2 the surface is intentionally small:
//! in-memory event callbacks and JSON-serializable records that can be
//! surfaced through the TUI status surface and a future `.codeforge/traces/`
//! writer.

use serde::{Deserialize, Serialize};

use crate::codeforge_tool_registry::{ToolInvocation, ToolOutput};

/// A trace record for a single tool call lifecycle.
///
/// Pairs a tool [`ToolInvocation`] with the resulting [`ToolOutput`] and
/// optional approval data so the trace surface can render a complete
/// picture of the call.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallTrace {
    /// The tool invocation that was dispatched.
    pub invocation: ToolInvocation,
    /// The tool output. Populated after the call completes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<ToolOutput>,
    /// Whether the call required approval, and whether it was granted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_granted: Option<bool>,
}

/// Lifecycle events emitted by the tool registry while a call is in
/// flight. The registry pushes one `Started` event at dispatch time and one
/// `Completed` event after the handler returns.
#[derive(Clone, Debug)]
pub enum ToolTraceEvent {
    /// A tool call was submitted to the registry and is about to run.
    Started { invocation: ToolInvocation },
    /// A tool call finished (with or without success).
    Completed { trace: ToolCallTrace },
}
