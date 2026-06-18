//! CodeForge-owned TUI backend adapter.

//!

//! Replaces the legacy stub replay shortcut: this module owns the small turn

//! lifecycle (TurnStarted -> AgentMessageDelta... -> ItemCompleted -> TurnCompleted)

//! and pushes [`AppServerEvent`] values into a tokio mpsc channel that the

//! stub [`AppServerSession`] drains via `next_event`.

//!

//! Phase 1 scope is intentionally narrow:

//!

//! * single model call with no tools and no system prompt injection

//! * no Codex sandbox / OpenAI auth / cloud config / plugin runtime

//! * surfaces streamed assistant text through `AgentMessageDelta`

//! * surfaces transport or model failures through `Error` + `TurnCompleted(Failed)`

use std::sync::Arc;

use chrono::Utc;

use codex_app_server_client::AppServerEvent;

use codex_app_server_protocol::AgentMessageDeltaNotification;

use codex_app_server_protocol::ErrorNotification;

use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::ItemStartedNotification;
use codex_app_server_protocol::McpToolCallError;
use codex_app_server_protocol::McpToolCallResult;
use codex_app_server_protocol::McpToolCallStatus;

use codex_app_server_protocol::ServerNotification;

use codex_app_server_protocol::ThreadItem;

use codex_app_server_protocol::Turn;

use codex_app_server_protocol::TurnCompletedNotification;

use codex_app_server_protocol::TurnError;

use codex_app_server_protocol::TurnItemsView;

use codex_app_server_protocol::TurnStartedNotification;

use codex_app_server_protocol::TurnStatus;

use codex_protocol::ThreadId;
use serde_json::Value;
use serde_json::json;
use std::path::PathBuf;
use std::time::Instant;

use tokio::sync::mpsc;

use uuid::Uuid;

use crate::codeforge_goal_state::GoalState;
use crate::codeforge_tool_registry::{
    ToolExecutionContext, ToolInvocation, ToolOutput, ToolRegistry,
};
use crate::codeforge_tool_trace::ToolTraceEvent;
use crate::codeforge_trace_store::{TraceRecord, TraceStatus, TraceStore};
use crate::legacy_core::config::Config;

/// Creates a fresh bounded event channel pair (sender, receiver) for a new

/// CodeForge backend session. The sender half is moved into a

/// [CodeForgeBackend]; the receiver half is owned by the

/// [AppServerSession](crate::app_server_session::AppServerSession) stub
/// and drained via `next_event`.
pub(crate) fn event_channel() -> (mpsc::Sender<AppServerEvent>, mpsc::Receiver<AppServerEvent>) {
    mpsc::channel(EVENT_CHANNEL_CAPACITY)
}

fn assistant_tool_call_message(round: &crate::codeforge_direct_chat::DirectChatRound) -> Value {
    let tool_calls: Vec<Value> = round
        .tool_calls
        .iter()
        .map(|tool_call| {
            json!({
                "id": tool_call.id,
                "type": "function",
                "function": {
                    "name": tool_call.name,
                    "arguments": tool_call.arguments,
                },
            })
        })
        .collect();
    json!({
        "role": "assistant",
        "content": if round.text.is_empty() {
            Value::Null
        } else {
            Value::String(round.text.clone())
        },
        "tool_calls": tool_calls,
    })
}

fn tool_result_message(
    tool_call: &crate::codeforge_direct_chat::DirectToolCall,
    result: &Value,
) -> Value {
    json!({
        "role": "tool",
        "tool_call_id": tool_call.id,
        "name": tool_call.name,
        "content": result.to_string(),
    })
}

fn parse_tool_arguments(arguments: &str) -> Result<Value, String> {
    let arguments = arguments.trim();
    if arguments.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str::<Value>(arguments)
        .map_err(|err| format!("tool arguments JSON parse failed: {err}; arguments={arguments}"))
}

fn mcp_tool_result(
    output: &ToolOutput,
) -> (
    McpToolCallStatus,
    Option<Box<McpToolCallResult>>,
    Option<McpToolCallError>,
) {
    if output.is_ok() {
        let model_value = output.to_model_value();
        let text = output
            .summary
            .clone()
            .unwrap_or_else(|| compact_json(&model_value));
        return (
            McpToolCallStatus::Completed,
            Some(Box::new(McpToolCallResult {
                content: vec![json!({
                    "type": "text",
                    "text": text,
                })],
                structured_content: output.output.clone(),
                meta: None,
            })),
            None,
        );
    }

    (
        McpToolCallStatus::Failed,
        None,
        Some(McpToolCallError {
            message: output
                .error
                .clone()
                .unwrap_or_else(|| "tool failed without an error message".to_string()),
        }),
    )
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unserializable>".to_string())
}

/// Capacity for the in-process event queue feeding the TUI event loop.

///

/// This matches the in-process app-server client default so the TUI treats

/// CodeForge-owned turns and remote turns the same way.

pub(crate) const EVENT_CHANNEL_CAPACITY: usize = 256;

/// Identifier for one streaming assistant turn.

#[derive(Debug, Clone)]

pub(crate) struct TurnHandle {
    pub thread_id: ThreadId,

    pub turn_id: String,

    pub item_id: String,

    pub started_at_ms: i64,
}

/// Owns a [`mpsc::Sender`] used to publish turn events to the TUI event loop.
#[derive(Clone)]
pub(crate) struct CodeForgeBackend {
    event_tx: mpsc::Sender<AppServerEvent>,
    registry: Arc<ToolRegistry>,
    /// Optional on-disk trace store. When set, every tool dispatch and
    /// model call writes a JSONL line to
    /// `<codeforge_home>/traces/<turn_id>.jsonl`. The store is shared
    /// (cloned) across backend clones so multiple TUI tasks can record
    /// trace events concurrently.
    trace_store: Option<Arc<TraceStore>>,
    /// CodeForge home directory (typically `~/.codeforge`).
    codeforge_home: Option<PathBuf>,
    /// Shared mutable slot for the current goal. Goal tools read the
    /// current state from this slot and write back updated state. The
    /// slot is shared across the entire TUI session.
    goal_slot: Option<Arc<std::sync::RwLock<Option<GoalState>>>>,
}

impl CodeForgeBackend {
    pub(crate) fn new(event_tx: mpsc::Sender<AppServerEvent>) -> Self {
        Self {
            event_tx,
            registry: Arc::new(crate::codeforge_tool_registry::default_registry()),
            trace_store: None,
            codeforge_home: None,
            goal_slot: None,
        }
    }

    /// Configure the on-disk trace store. Persisted under
    /// `<codeforge_home>/traces/<turn_id>.jsonl`. Returns the backend
    /// by value so the call reads as a builder step.
    pub(crate) fn with_trace_store(mut self, store: Arc<TraceStore>) -> Self {
        self.trace_store = Some(store);
        self
    }

    /// Set the CodeForge home directory used for `.codeforge/goal.json`
    /// and the traces directory. Optional; goal tools degrade to
    /// in-memory-only when absent.
    pub(crate) fn with_codeforge_home(mut self, home: impl Into<PathBuf>) -> Self {
        self.codeforge_home = Some(home.into());
        self
    }

    /// Wire a shared goal slot. Goal tools read the current state
    /// from this slot and write back updated state.
    pub(crate) fn with_goal_slot(
        mut self,
        slot: Arc<std::sync::RwLock<Option<GoalState>>>,
    ) -> Self {
        self.goal_slot = Some(slot);
        self
    }

    /// Returns the OpenAI function-calling tool definitions the model should
    /// see. Used by the Phase 3 model loop.
    pub(crate) fn openai_tools(&self) -> Vec<serde_json::Value> {
        self.registry.openai_tools()
    }

    /// Build a [`ToolExecutionContext`] for a given turn, populating
    /// workspace, codeforge home, VS bridge endpoint, and the goal
    /// slot from the backend's stored configuration.
    pub(crate) fn build_tool_context(
        &self,
        workspace_root: Option<PathBuf>,
        thread_id: Option<String>,
        turn_id: Option<String>,
    ) -> ToolExecutionContext {
        let vs_bridge_endpoint = crate::codeforge_vs_bridge::endpoint();
        ToolExecutionContext {
            workspace_root,
            codeforge_home: self.codeforge_home.clone(),
            turn_id,
            thread_id,
            vs_bridge_endpoint,
            goal_slot: self.goal_slot.clone(),
        }
    }

    /// Dispatch a single tool invocation through the live registry,
    /// persist the lifecycle to the on-disk trace store when one is
    /// configured, and return the resulting `ToolOutput`.
    pub(crate) fn dispatch_tool(
        &self,
        invocation: ToolInvocation,
        context: ToolExecutionContext,
    ) -> ToolOutput {
        let trace_store = self.trace_store.clone();
        let turn_id = context
            .turn_id
            .clone()
            .unwrap_or_else(|| "unknown-turn".to_string());
        let thread_id = context.thread_id.clone();
        let output = self
            .registry
            .dispatch(invocation.clone(), context, move |event| {
                if let (Some(store), ToolTraceEvent::Completed { trace }) =
                    (trace_store.as_ref(), &event)
                {
                    let record = TraceRecord::completed(turn_id.clone(), thread_id.clone(), trace);
                    if let Err(err) = store.write(record) {
                        tracing::warn!(error = %err, "trace store write failed");
                    }
                }
            });
        output
    }

    /// Persist a `Started` event for a tool invocation.
    pub(crate) fn trace_tool_started(
        &self,
        turn_id: &str,
        thread_id: Option<String>,
        tool_name: &str,
        call_id: Option<String>,
        arguments: Option<Value>,
    ) {
        let Some(store) = self.trace_store.as_ref() else {
            return;
        };
        let record = TraceRecord::started(turn_id, thread_id, tool_name, call_id, arguments);
        if let Err(err) = store.write(record) {
            tracing::warn!(error = %err, "trace store started write failed");
        }
    }

    fn trace_model_started(
        &self,
        turn_id: &str,
        thread_id: Option<String>,
        model: &str,
        messages_len: usize,
        tools_len: usize,
    ) {
        let record = TraceRecord {
            turn_id: turn_id.to_string(),
            thread_id,
            tool_name: None,
            status: TraceStatus::Started,
            payload: Some(json!({
                "kind": "model_request",
                "model": model,
                "messages": messages_len,
                "tools": tools_len,
            })),
            summary: Some(format!(
                "model request: model={model}, messages={messages_len}, tools={tools_len}"
            )),
            elapsed_ms: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        self.write_trace_record(record, "trace store model-started write failed");
    }

    fn trace_model_completed_round(
        &self,
        turn_id: &str,
        thread_id: Option<String>,
        round: &crate::codeforge_direct_chat::DirectChatRound,
        elapsed_ms: u64,
    ) {
        let content_chars = round.text.chars().count();
        let tool_calls = round.tool_calls.len();
        let mut payload = json!({
            "kind": "model_response",
            "provider": round.provider_name,
            "model": round.model_id,
            "contentChars": content_chars,
            "toolCalls": tool_calls,
        });
        if let Some(usage) = round.usage.clone() {
            payload["usage"] = usage;
        }
        let record = TraceRecord {
            turn_id: turn_id.to_string(),
            thread_id,
            tool_name: None,
            status: TraceStatus::Ok,
            payload: Some(payload),
            summary: Some(format!(
                "model response: {}/{}, tool_calls={tool_calls}, chars={content_chars}",
                round.provider_name, round.model_id
            )),
            elapsed_ms: Some(elapsed_ms),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        self.write_trace_record(record, "trace store model-completed write failed");
    }

    fn trace_model_completed_text(
        &self,
        turn_id: &str,
        thread_id: Option<String>,
        model: &str,
        text: &str,
        elapsed_ms: u64,
    ) {
        let content_chars = text.chars().count();
        let record = TraceRecord {
            turn_id: turn_id.to_string(),
            thread_id,
            tool_name: None,
            status: TraceStatus::Ok,
            payload: Some(json!({
                "kind": "model_response",
                "model": model,
                "contentChars": content_chars,
                "toolCalls": 0,
            })),
            summary: Some(format!(
                "model response: model={model}, chars={content_chars}"
            )),
            elapsed_ms: Some(elapsed_ms),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        self.write_trace_record(record, "trace store model-completed write failed");
    }

    fn trace_model_error(
        &self,
        turn_id: &str,
        thread_id: Option<String>,
        model: &str,
        error: &str,
        elapsed_ms: u64,
    ) {
        let record = TraceRecord {
            turn_id: turn_id.to_string(),
            thread_id,
            tool_name: None,
            status: TraceStatus::Error,
            payload: Some(json!({
                "kind": "model_error",
                "model": model,
                "error": error,
            })),
            summary: Some(format!("model error: {error}")),
            elapsed_ms: Some(elapsed_ms),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        self.write_trace_record(record, "trace store model-error write failed");
    }

    fn write_trace_record(&self, record: TraceRecord, warning: &str) {
        let Some(store) = self.trace_store.as_ref() else {
            return;
        };
        if let Err(err) = store.write(record) {
            tracing::warn!(error = %err, "{warning}");
        }
    }
    /// Spawns a turn on the Tokio runtime and returns the [`TurnHandle`] used

    /// by the caller (typically `AppServerSession::turn_start`) to build the

    /// `TurnStartResponse` while the turn is still running.

    pub(crate) fn spawn_turn(
        &self,

        config: Arc<Config>,

        thread_id: ThreadId,

        model: String,

        prompt: String,

        workspace_root: PathBuf,
    ) -> TurnHandle {
        let handle = TurnHandle {
            thread_id,

            turn_id: format!("codeforge-turn-{}", Uuid::new_v4()),

            item_id: format!("codeforge-agent-{}", Uuid::new_v4()),

            started_at_ms: Utc::now().timestamp_millis(),
        };

        let backend = self.clone();

        let turn_handle = handle.clone();

        tokio::spawn(async move {
            backend
                .run_turn(config, turn_handle, model, prompt, workspace_root)
                .await;
        });

        handle
    }

    async fn run_turn(
        self,

        config: Arc<Config>,

        handle: TurnHandle,

        model: String,

        prompt: String,

        workspace_root: PathBuf,
    ) {
        let thread_id_string = handle.thread_id.to_string();

        if let Err(err) = self.send_started(&thread_id_string, &handle).await {
            tracing::warn!(error = %err, "failed to publish TurnStarted");

            return;
        }

        let stream_result = match self
            .run_tool_enabled_turn(
                config.clone(),
                &handle,
                &model,
                &prompt,
                workspace_root.clone(),
            )
            .await
        {
            Ok(text) => Ok(text),
            Err(err) => {
                tracing::warn!(error = %err, "CodeForge tool-enabled turn failed; falling back to plain streaming");
                self.stream_plain_turn(config, &handle, &model, &prompt)
                    .await
            }
        };

        match stream_result {
            Ok(text) => {
                self.finish_turn(&handle, text, None).await;
            }

            Err(err) => {
                self.finish_turn(&handle, String::new(), Some(err)).await;
            }
        }
    }

    async fn run_tool_enabled_turn(
        &self,
        config: Arc<Config>,
        handle: &TurnHandle,
        model: &str,
        prompt: &str,
        workspace_root: PathBuf,
    ) -> Result<String, String> {
        const MAX_TOOL_ROUNDS: usize = 4;

        let tools = self.openai_tools();
        if tools.is_empty() {
            return self.stream_plain_turn(config, handle, model, prompt).await;
        }

        let thread_id = handle.thread_id.to_string();
        let mut messages = vec![json!({
            "role": "user",
            "content": prompt,
        })];

        for round_index in 0..=MAX_TOOL_ROUNDS {
            let round = self
                .request_chat_round(
                    config.as_ref(),
                    handle,
                    model,
                    messages.clone(),
                    Some(tools.clone()),
                )
                .await?;

            if round.tool_calls.is_empty() {
                return Ok(round.text);
            }

            messages.push(assistant_tool_call_message(&round));

            if round_index >= MAX_TOOL_ROUNDS {
                messages.push(json!({
                    "role": "system",
                    "content": "Tool round budget reached. Answer now using the available tool results. If evidence is incomplete, state what is missing.",
                }));
                let final_round = self
                    .request_chat_round(config.as_ref(), handle, model, messages, None)
                    .await?;
                return Ok(final_round.text);
            }

            for tool_call in round.tool_calls {
                let arguments = parse_tool_arguments(&tool_call.arguments);
                let tool_item_id = format!("codeforge-tool-{}", Uuid::new_v4());
                self.publish_tool_started(
                    &thread_id,
                    &handle.turn_id,
                    &tool_item_id,
                    &tool_call.name,
                    &arguments,
                )
                .await?;
                self.trace_tool_started(
                    &handle.turn_id,
                    Some(thread_id.clone()),
                    &tool_call.name,
                    Some(tool_call.id.clone()),
                    arguments.as_ref().ok().cloned(),
                );

                let output = match arguments {
                    Ok(arguments) => {
                        let invocation = ToolInvocation {
                            tool_name: tool_call.name.clone(),
                            arguments,
                            call_id: Some(tool_call.id.clone()),
                        };
                        let context = self.build_tool_context(
                            Some(workspace_root.clone()),
                            Some(thread_id.clone()),
                            Some(handle.turn_id.clone()),
                        );
                        self.dispatch_tool(invocation, context)
                    }
                    Err(err) => ToolOutput::error(err, 0),
                };

                self.publish_tool_completed(
                    &thread_id,
                    &handle.turn_id,
                    &tool_item_id,
                    &tool_call.name,
                    output.clone(),
                )
                .await?;
                messages.push(tool_result_message(&tool_call, &output.to_model_value()));
            }
        }

        Err("tool loop exited unexpectedly".to_string())
    }

    async fn request_chat_round(
        &self,
        config: &Config,
        handle: &TurnHandle,
        model: &str,
        messages: Vec<Value>,
        tools: Option<Vec<Value>>,
    ) -> Result<crate::codeforge_direct_chat::DirectChatRound, String> {
        let messages_len = messages.len();
        let tools_len = tools.as_ref().map_or(0, Vec::len);
        let trace_thread_id = Some(handle.thread_id.to_string());
        self.trace_model_started(
            &handle.turn_id,
            trace_thread_id.clone(),
            model,
            messages_len,
            tools_len,
        );
        let started = Instant::now();
        let (delta_tx, mut delta_rx) = mpsc::unbounded_channel::<String>();
        let event_tx = self.event_tx.clone();
        let thread_id = handle.thread_id.to_string();
        let turn_id = handle.turn_id.clone();
        let item_id = handle.item_id.clone();
        let drain_handle = tokio::spawn(async move {
            while let Some(delta) = delta_rx.recv().await {
                let notification =
                    ServerNotification::AgentMessageDelta(AgentMessageDeltaNotification {
                        thread_id: thread_id.clone(),
                        turn_id: turn_id.clone(),
                        item_id: item_id.clone(),
                        delta,
                    });
                if event_tx
                    .send(AppServerEvent::ServerNotification(notification))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        let result =
            crate::codeforge_direct_chat::chat_completion_round(config, model, messages, tools, {
                move |delta| {
                    let _ = delta_tx.send(delta.to_string());
                }
            })
            .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;

        match &result {
            Ok(round) => self.trace_model_completed_round(
                &handle.turn_id,
                trace_thread_id,
                round,
                elapsed_ms,
            ),
            Err(err) => {
                self.trace_model_error(&handle.turn_id, trace_thread_id, model, err, elapsed_ms)
            }
        }

        if let Err(err) = drain_handle.await {
            tracing::warn!(error = %err, "codeforge backend round drain task failed");
        }
        result
    }

    async fn stream_plain_turn(
        &self,
        config: Arc<Config>,
        handle: &TurnHandle,
        model: &str,
        prompt: &str,
    ) -> Result<String, String> {
        let (delta_tx, mut delta_rx) = mpsc::unbounded_channel::<String>();
        let event_tx = self.event_tx.clone();
        let thread_id = handle.thread_id.to_string();
        let turn_id = handle.turn_id.clone();
        let item_id = handle.item_id.clone();
        let trace_thread_id = Some(thread_id.clone());
        self.trace_model_started(&handle.turn_id, trace_thread_id.clone(), model, 1, 0);
        let started = Instant::now();
        let drain_handle = tokio::spawn(async move {
            while let Some(delta) = delta_rx.recv().await {
                let notification =
                    ServerNotification::AgentMessageDelta(AgentMessageDeltaNotification {
                        thread_id: thread_id.clone(),
                        turn_id: turn_id.clone(),
                        item_id: item_id.clone(),
                        delta,
                    });
                if event_tx
                    .send(AppServerEvent::ServerNotification(notification))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        let stream_result =
            crate::codeforge_direct_chat::stream(config.as_ref(), model, prompt, move |delta| {
                let _ = delta_tx.send(delta.to_string());
            })
            .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        match &stream_result {
            Ok(text) => self.trace_model_completed_text(
                &handle.turn_id,
                trace_thread_id,
                model,
                text,
                elapsed_ms,
            ),
            Err(err) => {
                self.trace_model_error(&handle.turn_id, trace_thread_id, model, err, elapsed_ms)
            }
        }

        if let Err(err) = drain_handle.await {
            tracing::warn!(error = %err, "codeforge backend plain drain task failed");
        }
        stream_result
    }

    async fn publish_tool_started(
        &self,
        thread_id: &str,
        turn_id: &str,
        item_id: &str,
        tool_name: &str,
        arguments: &Result<Value, String>,
    ) -> Result<(), String> {
        let item = ThreadItem::McpToolCall {
            id: item_id.to_string(),
            server: "codeforge".to_string(),
            tool: tool_name.to_string(),
            status: McpToolCallStatus::InProgress,
            arguments: arguments
                .as_ref()
                .cloned()
                .unwrap_or_else(|err| json!({ "argumentParseError": err })),
            mcp_app_resource_uri: None,
            plugin_id: None,
            result: None,
            error: None,
            duration_ms: None,
        };
        let notification = ServerNotification::ItemStarted(ItemStartedNotification {
            item,
            thread_id: thread_id.to_string(),
            turn_id: turn_id.to_string(),
            started_at_ms: Utc::now().timestamp_millis(),
        });
        self.event_tx
            .send(AppServerEvent::ServerNotification(notification))
            .await
            .map_err(|err| format!("channel closed: {err}"))
    }

    async fn publish_tool_completed(
        &self,
        thread_id: &str,
        turn_id: &str,
        item_id: &str,
        tool_name: &str,
        output: ToolOutput,
    ) -> Result<(), String> {
        let (status, result, error) = mcp_tool_result(&output);
        let item = ThreadItem::McpToolCall {
            id: item_id.to_string(),
            server: "codeforge".to_string(),
            tool: tool_name.to_string(),
            status,
            arguments: Value::Null,
            mcp_app_resource_uri: None,
            plugin_id: None,
            result,
            error,
            duration_ms: Some(output.elapsed_ms as i64),
        };
        let notification = ServerNotification::ItemCompleted(ItemCompletedNotification {
            item,
            thread_id: thread_id.to_string(),
            turn_id: turn_id.to_string(),
            completed_at_ms: Utc::now().timestamp_millis(),
        });
        self.event_tx
            .send(AppServerEvent::ServerNotification(notification))
            .await
            .map_err(|err| format!("channel closed: {err}"))
    }

    async fn send_started(&self, thread_id: &str, handle: &TurnHandle) -> Result<(), String> {
        let turn = Turn {
            id: handle.turn_id.clone(),

            items: Vec::new(),

            items_view: TurnItemsView::Full,

            status: TurnStatus::InProgress,

            error: None,

            started_at: Some(handle.started_at_ms / 1000),

            completed_at: None,

            duration_ms: None,
        };

        let notification = ServerNotification::TurnStarted(TurnStartedNotification {
            thread_id: thread_id.to_string(),

            turn,
        });

        self.event_tx
            .send(AppServerEvent::ServerNotification(notification))
            .await
            .map_err(|err| format!("channel closed: {err}"))
    }

    async fn finish_turn(&self, handle: &TurnHandle, text: String, error_message: Option<String>) {
        let thread_id = handle.thread_id.to_string();

        let completed_at_ms = Utc::now().timestamp_millis();

        let started_at_ms = handle.started_at_ms;

        let duration_ms = (completed_at_ms - started_at_ms).max(0);

        let (status, turn_error) = match &error_message {
            Some(message) => (
                TurnStatus::Failed,
                Some(TurnError {
                    message: message.clone(),

                    codex_error_info: None,

                    additional_details: None,
                }),
            ),

            None => (TurnStatus::Completed, None),
        };

        if let Some(message) = error_message {
            let notification = ServerNotification::Error(ErrorNotification {
                error: TurnError {
                    message,

                    codex_error_info: None,

                    additional_details: None,
                },

                will_retry: false,

                thread_id: thread_id.clone(),

                turn_id: handle.turn_id.clone(),
            });

            if let Err(err) = self
                .event_tx
                .send(AppServerEvent::ServerNotification(notification))
                .await
            {
                tracing::warn!(error = %err, "failed to publish Error");

                return;
            }
        } else {
            let item = ThreadItem::AgentMessage {
                id: handle.item_id.clone(),

                text,

                phase: None,

                memory_citation: None,
            };

            let item_notification = ServerNotification::ItemCompleted(ItemCompletedNotification {
                item,

                thread_id: thread_id.clone(),

                turn_id: handle.turn_id.clone(),

                completed_at_ms,
            });

            if let Err(err) = self
                .event_tx
                .send(AppServerEvent::ServerNotification(item_notification))
                .await
            {
                tracing::warn!(error = %err, "failed to publish ItemCompleted");

                return;
            }
        }

        let turn = Turn {
            id: handle.turn_id.clone(),

            items: Vec::new(),

            items_view: TurnItemsView::Full,

            status,

            error: turn_error,

            started_at: Some(started_at_ms / 1000),

            completed_at: Some(completed_at_ms / 1000),

            duration_ms: Some(duration_ms),
        };

        let turn_notification =
            ServerNotification::TurnCompleted(TurnCompletedNotification { thread_id, turn });

        if let Err(err) = self
            .event_tx
            .send(AppServerEvent::ServerNotification(turn_notification))
            .await
        {
            tracing::warn!(error = %err, "failed to publish TurnCompleted");
        }
    }
}
