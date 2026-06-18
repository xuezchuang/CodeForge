//! CodeForge-owned on-disk trace store for the standalone TUI.
//!
//! Phase 6 of the CodeForge TUI backend-and-tooling plan. The trace
//! store is the TUI-owned equivalent of the Tauri `tool_trace::ToolTraceStore`.
//! The TUI does not depend on the Tauri desktop crate, so the TUI ships
//! its own copy. Trace events are appended as JSONL lines under
//! `<codeforge_home>/traces/<turn_id>.jsonl` and a rolling in-memory
//! `recent()` list keeps the last N events for the TUI status surface.
//!
//! The store is intentionally small: writes are append-only and atomic
//! per line, reads are line-delimited JSON, and there is no external
//! database dependency. Phase 6+ can layer indexing or compaction on
//! top without changing the on-disk format.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::codeforge_tool_trace::ToolCallTrace;

/// Status reported on each persisted trace event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceStatus {
    /// The tool was submitted to the handler.
    Started,
    /// The tool finished successfully.
    Ok,
    /// The tool failed.
    Error,
    /// The tool was rejected before execution.
    Rejected,
}

/// Lifecycle of one tool call. Persisted as a single JSONL line
/// per event.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceRecord {
    /// The turn identifier this record belongs to. Used to scope the
    /// per-turn JSONL file.
    pub turn_id: String,
    /// Optional thread identifier (CodeForge thread id from the
    /// app-server protocol) for cross-correlation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// The tool name involved in the event, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// The lifecycle phase.
    pub status: TraceStatus,
    /// Optional structured payload. For `Started` events this carries
    /// the call id and parsed arguments; for terminal events it carries
    /// the tool output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
    /// Short human-readable summary shown in the TUI status surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Wall-clock duration in milliseconds for terminal events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    /// RFC 3339 timestamp when the event was emitted.
    pub created_at: String,
}

impl TraceRecord {
    /// Build a `Started` event for a fresh dispatch.
    pub fn started(
        turn_id: impl Into<String>,
        thread_id: Option<String>,
        tool_name: impl Into<String>,
        call_id: Option<String>,
        arguments: Option<Value>,
    ) -> Self {
        let payload = match (call_id, arguments) {
            (Some(call_id), Some(arguments)) => {
                Some(json!({ "callId": call_id, "arguments": arguments }))
            }
            (Some(call_id), None) => Some(json!({ "callId": call_id })),
            (None, Some(arguments)) => Some(json!({ "arguments": arguments })),
            (None, None) => None,
        };
        Self {
            turn_id: turn_id.into(),
            thread_id,
            tool_name: Some(tool_name.into()),
            status: TraceStatus::Started,
            payload,
            summary: None,
            elapsed_ms: None,
            created_at: Utc::now().to_rfc3339(),
        }
    }

    /// Build a terminal event from a completed [`ToolCallTrace`].
    pub fn completed(
        turn_id: impl Into<String>,
        thread_id: Option<String>,
        trace: &ToolCallTrace,
    ) -> Self {
        let (status, summary, elapsed_ms, payload) = match trace.output.as_ref() {
            Some(output) => {
                let status = match output.status {
                    crate::codeforge_tool_registry::ToolOutputStatus::Ok => TraceStatus::Ok,
                    crate::codeforge_tool_registry::ToolOutputStatus::Error
                    | crate::codeforge_tool_registry::ToolOutputStatus::Timeout => {
                        TraceStatus::Error
                    }
                    crate::codeforge_tool_registry::ToolOutputStatus::Rejected => {
                        TraceStatus::Rejected
                    }
                };
                let payload = json!({
                    "invocation": trace.invocation,
                    "output": output.to_model_value(),
                });
                (
                    status,
                    output.summary.clone(),
                    Some(output.elapsed_ms),
                    Some(payload),
                )
            }
            None => (
                TraceStatus::Error,
                Some("tool call completed without output".to_string()),
                None,
                None,
            ),
        };
        Self {
            turn_id: turn_id.into(),
            thread_id,
            tool_name: Some(trace.invocation.tool_name.clone()),
            status,
            payload,
            summary,
            elapsed_ms,
            created_at: Utc::now().to_rfc3339(),
        }
    }
}

/// Compute the on-disk directory for trace JSONL files.
pub fn traces_dir(codeforge_home: &Path) -> PathBuf {
    codeforge_home.join("traces")
}

/// Compute the per-turn JSONL path. `turn_id` is sanitized to keep
/// only filesystem-safe characters.
pub fn trace_path(codeforge_home: &Path, turn_id: &str) -> PathBuf {
    let safe = sanitize_turn_id(turn_id);
    traces_dir(codeforge_home).join(format!("{safe}.jsonl"))
}

fn sanitize_turn_id(turn_id: &str) -> String {
    let mut out = String::with_capacity(turn_id.len());
    for ch in turn_id.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push_str("turn");
    }
    out
}

/// Persistent + in-memory trace store. `write` appends to the per-turn
/// JSONL file under `<codeforge_home>/traces/<turn_id>.jsonl` and to a
/// fixed-size in-memory ring of recent events. `recent` returns the
/// most recent events across all turns.
pub struct TraceStore {
    codeforge_home: PathBuf,
    memory: Mutex<Vec<TraceRecord>>,
    memory_capacity: usize,
}

impl TraceStore {
    /// Build a trace store rooted at `codeforge_home`. Creates the
    /// traces directory eagerly so the first write never races with
    /// directory creation.
    pub fn new(codeforge_home: impl Into<PathBuf>) -> io::Result<Self> {
        let codeforge_home = codeforge_home.into();
        fs::create_dir_all(traces_dir(&codeforge_home))?;
        Ok(Self {
            codeforge_home,
            memory: Mutex::new(Vec::new()),
            memory_capacity: 256,
        })
    }

    /// Replace the in-memory ring capacity. The default is 256.
    pub fn with_memory_capacity(mut self, capacity: usize) -> Self {
        self.memory_capacity = capacity.max(1);
        self
    }

    /// Append `record` to both the per-turn JSONL file and the
    /// in-memory ring. I/O failures are returned to the caller so the
    /// turn runtime can surface them.
    pub fn write(&self, record: TraceRecord) -> io::Result<()> {
        let path = trace_path(&self.codeforge_home, &record.turn_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let line = serde_json::to_string(&record).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("failed to serialize trace record: {err}"),
            )
        })?;
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_data()?;

        let mut guard = self
            .memory
            .lock()
            .map_err(|err| io::Error::other(format!("trace memory mutex poisoned: {err}")))?;
        guard.push(record);
        if guard.len() > self.memory_capacity {
            let excess = guard.len() - self.memory_capacity;
            guard.drain(0..excess);
        }
        Ok(())
    }

    /// Snapshot the most recent in-memory events, newest first.
    pub fn recent(&self) -> Vec<TraceRecord> {
        match self.memory.lock() {
            Ok(guard) => {
                let mut snapshot = guard.clone();
                snapshot.reverse();
                snapshot
            }
            Err(err) => {
                tracing::warn!(error = %err, "trace memory mutex poisoned during read");
                Vec::new()
            }
        }
    }

    /// Read all persisted events for a single turn, oldest first.
    /// Missing file is not an error and returns an empty vector.
    pub fn read_turn(&self, turn_id: &str) -> io::Result<Vec<TraceRecord>> {
        let path = trace_path(&self.codeforge_home, turn_id);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => {
                return Err(io::Error::new(
                    err.kind(),
                    format!("failed to read turn trace {}: {err}", path.display()),
                ));
            }
        };
        let mut records = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<TraceRecord>(line) {
                Ok(record) => records.push(record),
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        path = %path.display(),
                        "skipping malformed trace line"
                    );
                }
            }
        }
        Ok(records)
    }

    /// Remove the on-disk trace file for a turn. Missing file is not
    /// an error.
    pub fn clear_turn(&self, turn_id: &str) -> io::Result<bool> {
        let path = trace_path(&self.codeforge_home, turn_id);
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codeforge_tool_registry::{ToolInvocation, ToolOutput, ToolOutputStatus};
    use crate::codeforge_tool_trace::ToolCallTrace;
    use tempfile::TempDir;

    #[test]
    fn sanitize_turn_id_keeps_safe_chars() {
        assert_eq!(sanitize_turn_id("turn-123_abc"), "turn-123_abc");
        assert_eq!(sanitize_turn_id("a/b c?d"), "a_b_c_d");
        assert_eq!(sanitize_turn_id(""), "turn");
    }

    #[test]
    fn trace_path_is_inside_traces_dir() {
        let dir = TempDir::new().unwrap();
        let path = trace_path(dir.path(), "turn-1");
        assert!(path.starts_with(traces_dir(dir.path())));
        assert_eq!(path.file_name().unwrap(), "turn-1.jsonl");
    }

    #[test]
    fn write_and_recent_roundtrip() {
        let dir = TempDir::new().unwrap();
        let store = TraceStore::new(dir.path()).unwrap();
        store
            .write(TraceRecord::started(
                "turn-1",
                Some("thread-1".to_string()),
                "workspace/read_file",
                Some("call-1".to_string()),
                Some(json!({ "path": "README.md" })),
            ))
            .unwrap();
        let tool_trace = ToolCallTrace {
            invocation: ToolInvocation {
                tool_name: "workspace/read_file".to_string(),
                arguments: json!({ "path": "README.md" }),
                call_id: Some("call-1".to_string()),
            },
            output: Some(ToolOutput::ok_with_summary(
                json!({ "lines": 42 }),
                7,
                "read 42 lines".to_string(),
            )),
            approval_granted: None,
        };
        store
            .write(TraceRecord::completed(
                "turn-1",
                Some("thread-1".to_string()),
                &tool_trace,
            ))
            .unwrap();

        let recent = store.recent();
        assert_eq!(recent.len(), 2);
        // Newest first
        assert_eq!(recent[0].status, TraceStatus::Ok);
        assert_eq!(recent[0].tool_name.as_deref(), Some("workspace/read_file"));
        assert_eq!(recent[0].elapsed_ms, Some(7));
        assert_eq!(recent[1].status, TraceStatus::Started);

        let on_disk = store.read_turn("turn-1").unwrap();
        assert_eq!(on_disk.len(), 2);
        assert_eq!(on_disk[0].status, TraceStatus::Started);
        assert_eq!(on_disk[1].status, TraceStatus::Ok);
        assert_eq!(on_disk[1].summary.as_deref(), Some("read 42 lines"));
    }

    #[test]
    fn recent_caps_to_memory_capacity() {
        let dir = TempDir::new().unwrap();
        let store = TraceStore::new(dir.path()).unwrap().with_memory_capacity(3);
        for idx in 0..5 {
            store
                .write(TraceRecord::started(
                    format!("turn-{idx}"),
                    None,
                    "tool",
                    None,
                    None,
                ))
                .unwrap();
        }
        let recent = store.recent();
        assert_eq!(recent.len(), 3);
        // Newest first, so the last three writes are visible.
        assert_eq!(recent[0].turn_id, "turn-4");
        assert_eq!(recent[1].turn_id, "turn-3");
        assert_eq!(recent[2].turn_id, "turn-2");
    }

    #[test]
    fn clear_turn_removes_file() {
        let dir = TempDir::new().unwrap();
        let store = TraceStore::new(dir.path()).unwrap();
        store
            .write(TraceRecord::started("turn-x", None, "tool", None, None))
            .unwrap();
        assert!(trace_path(dir.path(), "turn-x").exists());
        assert!(store.clear_turn("turn-x").unwrap());
        assert!(!trace_path(dir.path(), "turn-x").exists());
        // Missing file is not an error.
        assert!(!store.clear_turn("turn-x").unwrap());
    }

    #[test]
    fn read_turn_returns_empty_for_missing_file() {
        let dir = TempDir::new().unwrap();
        let store = TraceStore::new(dir.path()).unwrap();
        assert!(store.read_turn("never").unwrap().is_empty());
    }

    #[test]
    fn completed_event_marks_rejected_status() {
        let tool_trace = ToolCallTrace {
            invocation: ToolInvocation {
                tool_name: "workspace/apply_patch".to_string(),
                arguments: json!({ "file": "x.txt" }),
                call_id: Some("call-2".to_string()),
            },
            output: Some(ToolOutput::rejected("user denied".to_string())),
            approval_granted: Some(false),
        };
        let record = TraceRecord::completed("turn-9", None, &tool_trace);
        assert_eq!(record.status, TraceStatus::Rejected);
        assert_eq!(record.tool_name.as_deref(), Some("workspace/apply_patch"));
    }
}
