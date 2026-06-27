use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Utc;
use codeforge_core::office_tools;
use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE, USER_AGENT,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::agent::stream::StreamingChatCompletionAccumulator;
use crate::codex_cli_runner::{self, CODEX_CLI_PROVIDER_TYPE, CODEX_CLI_TOOL_NAME};
use crate::goal_state::GoalState;
use crate::mcp_runtime::McpRuntime;
use crate::project_registry::ProjectSession;
use crate::subagent_manager::SubagentManager;
use crate::tool_interface::ToolOutput;
use crate::tool_registry::{
    self, ToolExecutionContext, CALCULATOR_ADD_TOOL_NAME, PRESENTATION_READ_PPTX_TOOL_NAME,
};
use crate::tool_trace::{
    self, ContextCompactionResult, MockAgentRun, SubagentTraceRun, ToolTraceEvent, TraceEventType,
    TraceStatus,
};
use crate::vs_registry::{
    infer_model_supports_vision, AppSettings, ProviderConfig, ProviderCredential, ProviderModel,
};

pub const TOOL_CALL_TEST_PROMPT: &str = "请必须调用 calculator.add 工具计算 1+1，然后告诉我结果。";
const DEFAULT_MAX_TOOL_ROUNDS: usize = 32;
const EMPTY_TOOL_CALL_RESPONSE_RETRY_LIMIT: usize = 1;
const MODEL_REQUEST_TIMEOUT_SECONDS: u64 = 360;
const MCP_AGENT_STARTUP_BUDGET: Duration = Duration::from_secs(2);
const STREAMING_TRACE_INTERVAL_MS: u64 = 750;
const MODEL_RATE_LIMIT_RETRY_DELAYS_MS: [u64; 3] = [3_000, 10_000, 30_000];
const PPTX_IMAGE_ANALYSIS_BATCH_SIZE: usize = 16;
const PARALLEL_READONLY_TOOL_LIMIT: usize = 4;
const CONTEXT_COMPACTION_ESTIMATED_TOKEN_LIMIT: usize = 96_000;
const CONTEXT_COMPACTION_RECENT_TOKEN_BUDGET: usize = 20_000;
const CONTEXT_COMPACTION_MIN_MESSAGE_COUNT: usize = 12;
const CONTEXT_COMPACTION_MESSAGE_PREFIX: &str = "[CodeForge context compacted]";
const CODEFORGE_DEVELOPER_INSTRUCTION_FILES: [&str; 4] = [
    ".codeforge/codeforge.md",
    ".codeforge/CODEFORGE.md",
    "codeforge.md",
    "CODEFORGE.md",
];
const AGENTS_USER_INSTRUCTION_FILES: [&str; 1] = ["AGENTS.md"];
const AI_CONTEXT_INDEX_FILE: &str = "doc/ai-context/README.md";

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRunInput {
    pub project_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    pub user_prompt: String,
    pub messages: Option<Vec<AgentConversationMessage>>,
    pub provider_id: Option<String>,
    pub credential_id: Option<String>,
    pub model_id: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub allow_shell: bool,
    #[serde(default)]
    pub assume_yes: bool,
    #[serde(default)]
    pub cli_mode: bool,
    #[serde(default)]
    pub goal: Option<GoalState>,
    #[serde(default)]
    pub parent_task_id: Option<String>,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub task_name: Option<String>,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub subagent_depth: u32,
    /// Optional mutable slot for tools to write goal changes back to. When set,
    /// the tool execution context receives a &mut Option<GoalState> and any
    /// goal/set calls from tools mutate this slot directly. Callers that do
    /// not need write-back can leave this as None and pass goal instead.
    #[serde(default, skip)]
    pub goal_slot: Option<Box<Option<GoalState>>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConversationMessage {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub attachments: Vec<AgentMessageAttachment>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentMessageAttachment {
    pub kind: String,
    pub name: String,
    pub mime_type: String,
    pub data_url: String,
}

#[derive(Clone)]
struct SelectedModel {
    provider: ProviderConfig,
    credential: Option<ProviderCredential>,
    model_id: String,
    model: Option<ProviderModel>,
    reasoning_effort: Option<String>,
}

impl SelectedModel {
    fn credential_api_key(&self) -> String {
        let credential_key = self
            .credential
            .as_ref()
            .map(|credential| credential.api_key.as_str())
            .unwrap_or("")
            .trim();
        if !credential_key.is_empty() {
            return credential_key.to_string();
        }
        let env_key = self.provider.env_key.trim();
        if env_key.is_empty() {
            return String::new();
        }
        std::env::var(env_key)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatMessage {
    role: String,
    content: String,
    #[serde(skip)]
    attachments: Vec<AgentMessageAttachment>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct TokenUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    total_tokens: Option<u64>,
    input_cached_tokens: Option<u64>,
    input_uncached_tokens: Option<u64>,
}

#[derive(Debug)]
struct ProviderCompletion {
    message: String,
    duration_ms: u64,
    token_usage: TokenUsage,
    request_body: Value,
    response_body: Value,
}

#[derive(Debug)]
struct ChatCompletionResult {
    duration_ms: u64,
    request_body: Value,
    response_body: Value,
    token_usage: TokenUsage,
}

#[derive(Clone, Debug)]
struct PromptLayers {
    system: String,
    developer: String,
    user_context: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentIntentMode {
    Default,
    Research,
    Debug,
    Implement,
    Review,
    Verify,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IntentClassification {
    mode: AgentIntentMode,
    reason: &'static str,
}

impl AgentIntentMode {
    fn as_str(self) -> &'static str {
        match self {
            AgentIntentMode::Default => "default",
            AgentIntentMode::Research => "research",
            AgentIntentMode::Debug => "debug",
            AgentIntentMode::Implement => "implement",
            AgentIntentMode::Review => "review",
            AgentIntentMode::Verify => "verify",
        }
    }

    fn enforces_read_only(self) -> bool {
        !matches!(self, AgentIntentMode::Implement)
    }

    fn allows_auto_readonly_subagents(self) -> bool {
        matches!(
            self,
            AgentIntentMode::Research | AgentIntentMode::Debug | AgentIntentMode::Review
        )
    }
}

#[derive(Clone, Debug)]
struct InstructionFile {
    path: PathBuf,
    content: String,
}

struct StreamingTraceSink<'a> {
    task_id: &'a str,
    step_index: u32,
    on_trace: &'a mut (dyn FnMut(&ToolTraceEvent) + Send),
}

#[derive(Clone, Debug)]
struct ParsedToolCall {
    tool_call: OpenAiToolCall,
    tool_name: String,
    arguments: Value,
}

#[derive(Clone, Debug)]
struct CompletedToolCall {
    tool_call: OpenAiToolCall,
    tool_name: String,
    arguments: Value,
    result: ToolOutput,
}

#[derive(Clone, Debug)]
struct ParallelToolExecutionContext {
    workspace_root: String,
    vs_bridge_endpoint: Option<String>,
    allow_shell: bool,
    assume_yes: bool,
    cli_mode: bool,
}

#[derive(Clone, Debug, Default)]
struct ToolNameMap {
    pairs: Vec<(String, String)>,
}

struct AppliedContextCompaction {
    messages: Vec<ChatMessage>,
    result: ContextCompactionResult,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OpenAiToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: OpenAiFunctionCall,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OpenAiFunctionCall {
    name: String,
    arguments: String,
}

impl ToolNameMap {
    fn from_tools(tools: &[Value]) -> Self {
        let mut pairs = Vec::new();
        let mut used_model_names: Vec<String> = Vec::new();

        for tool in tools {
            let Some(original_name) = tool
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let model_name = unique_model_tool_name(original_name, &used_model_names);
            used_model_names.push(model_name.clone());
            pairs.push((model_name, original_name.to_string()));
        }

        Self { pairs }
    }

    fn tools_for_model(&self, tools: &[Value]) -> Vec<Value> {
        tools
            .iter()
            .map(|tool| {
                let mut tool = tool.clone();
                if let Some(function) = tool.get_mut("function").and_then(Value::as_object_mut) {
                    let original_name = function
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    if let Some(original_name) = original_name {
                        if let Some(model_name) = self.model_name(&original_name) {
                            function.insert("name".to_string(), Value::String(model_name));
                        }
                    }
                }
                tool
            })
            .collect()
    }

    fn original_name(&self, model_name: &str) -> String {
        self.pairs
            .iter()
            .find(|(name, _)| name == model_name)
            .map(|(_, original)| original.clone())
            .unwrap_or_else(|| model_name.to_string())
    }

    fn model_name(&self, original_name: &str) -> Option<String> {
        self.pairs
            .iter()
            .find(|(_, original)| original == original_name)
            .map(|(name, _)| name.clone())
    }

    fn tool_call_names(&self, tool_calls: &[OpenAiToolCall]) -> Vec<String> {
        tool_calls
            .iter()
            .map(|tool_call| self.original_name(&tool_call.function.name))
            .collect()
    }
}

fn unique_model_tool_name(original_name: &str, used_model_names: &[String]) -> String {
    let base = safe_model_tool_name(original_name);
    if !used_model_names.iter().any(|name| name == &base) {
        return base;
    }

    let mut index = 2usize;
    loop {
        let candidate = format!("{base}_{index}");
        if !used_model_names.iter().any(|name| name == &candidate) {
            return candidate;
        }
        index += 1;
    }
}

fn safe_model_tool_name(original_name: &str) -> String {
    let safe = original_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if safe.trim_matches('_').is_empty() {
        "tool".to_string()
    } else {
        safe
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
    finish_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OpenAiMessage {
    role: Option<String>,
    content: Option<String>,
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<OpenAiToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
    prompt_tokens_details: Option<OpenAiPromptTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct OpenAiPromptTokensDetails {
    cached_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct OllamaChatResponse {
    message: Option<OllamaMessage>,
    response: Option<String>,
    prompt_eval_count: Option<u64>,
    eval_count: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct OllamaMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
struct ClaudeMessagesResponse {
    content: Vec<ClaudeContentBlock>,
    usage: Option<ClaudeUsage>,
}

#[derive(Debug, Deserialize)]
struct ClaudeContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClaudeUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
}

pub async fn run_agent(
    project: &ProjectSession,
    settings: &AppSettings,
    mut input: AgentRunInput,
    mut on_trace: impl FnMut(&ToolTraceEvent) + Send,
) -> Result<MockAgentRun, String> {
    let task_id = input
        .task_id
        .take()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let init_command = is_init_command(&input.user_prompt);
    let intent_classification = if init_command {
        IntentClassification {
            mode: AgentIntentMode::Implement,
            reason: "init_command",
        }
    } else {
        classify_intent_mode(&input.user_prompt)
    };
    let intent_mode = intent_classification.mode;
    let effective_read_only =
        input.read_only || (!init_command && intent_mode.enforces_read_only());
    let mut conversation_messages = if init_command {
        vec![chat_message("user", ai_context_init_prompt(project))]
    } else {
        normalize_conversation_messages(input.messages.as_deref(), &input.user_prompt)
    };
    let mut context_compaction: Option<ContextCompactionResult> = None;
    let mut traces = Vec::new();
    push_trace(
        &mut traces,
        trace(
            &task_id,
            1,
            TraceEventType::UserMessage,
            None,
            "user_message",
            Some(json!({
                "projectId": project.id,
                "projectName": project.name,
                "prompt": input.user_prompt,
            })),
            None,
            Some(input.user_prompt.clone()),
            TraceStatus::Success,
            0,
        ),
        &mut on_trace,
    );

    push_trace(
        &mut traces,
        trace(
            &task_id,
            2,
            TraceEventType::SystemEvent,
            None,
            "intent_mode",
            Some(json!({
                "prompt": input.user_prompt,
            })),
            Some(json!({
                "mode": intent_mode.as_str(),
                "reason": intent_classification.reason,
                "readOnly": effective_read_only,
            })),
            Some(format!(
                "Mode: {}{}",
                intent_mode.as_str(),
                if effective_read_only {
                    " (read-only)"
                } else {
                    ""
                }
            )),
            TraceStatus::Success,
            0,
        ),
        &mut on_trace,
    );

    let selected = match select_model(
        settings,
        input.provider_id.as_deref(),
        input.credential_id.as_deref(),
        input.model_id.as_deref(),
        input.reasoning_effort.as_deref(),
    ) {
        Ok(selected) => selected,
        Err(error) => {
            push_trace(
                &mut traces,
                error_trace(
                    &task_id,
                    3,
                    "select_model failed",
                    Some(json!({
                        "providerId": input.provider_id,
                        "credentialId": input.credential_id,
                        "modelId": input.model_id,
                        "reasoningEffort": input.reasoning_effort,
                    })),
                    &error,
                ),
                &mut on_trace,
            );
            return Ok(mock_agent_run(
                task_id,
                traces,
                Vec::new(),
                context_compaction,
            ));
        }
    };

    push_trace(
        &mut traces,
        trace(
            &task_id,
            3,
            TraceEventType::SystemEvent,
            None,
            "select_model",
            Some(json!({
                "providerId": selected.provider.id,
                "credentialId": selected.credential.as_ref().map(|credential| credential.id.clone()),
                "modelId": selected.model_id,
            })),
            Some(json!({
                "provider": selected.provider.name,
                "credential": selected
                    .credential
                    .as_ref()
                    .map(|credential| credential.name.clone()),
                "type": selected.provider.provider_type,
                "baseUrl": selected.provider.base_url,
                "model": selected.model_id,
            })),
            Some(format!(
                "{} / {}",
                selected.provider.name, selected.model_id
            )),
            TraceStatus::Success,
            0,
        ),
        &mut on_trace,
    );

    let auto_readonly_subagents = input.subagent_depth == 0
        && !input.cli_mode
        && intent_mode.allows_auto_readonly_subagents();
    let mut subagent_manager = if auto_readonly_subagents {
        Some(SubagentManager::new(
            task_id.clone(),
            project.clone(),
            settings.clone(),
            Some(selected.provider.id.clone()),
            selected
                .credential
                .as_ref()
                .map(|credential| credential.id.clone()),
            Some(selected.model_id.clone()),
            selected.reasoning_effort.clone(),
        ))
    } else {
        None
    };

    let mut step_index = 4;
    if !init_command && selected.provider.provider_type != CODEX_CLI_PROVIDER_TYPE {
        if let Some(compaction) = maybe_compact_conversation(
            project,
            &selected,
            &conversation_messages,
            input.cli_mode,
            &task_id,
            &mut traces,
            &mut step_index,
            &mut on_trace,
        )
        .await
        {
            conversation_messages = compaction.messages;
            context_compaction = Some(compaction.result);
        }
    }

    if codex_cli_runner::is_codex_cli_provider(&selected.provider.provider_type) {
        if init_command {
            push_trace(
                &mut traces,
                error_trace(
                    &task_id,
                    step_index,
                    "/init requires workspace tools",
                    Some(json!({
                        "provider": selected.provider.name,
                        "type": selected.provider.provider_type,
                        "model": selected.model_id,
                    })),
                    "/init needs the workspace read/search/write tools. Select a tool-capable OpenAI-compatible model.",
                ),
                &mut on_trace,
            );
            return Ok(mock_agent_run(
                task_id,
                traces,
                Vec::new(),
                context_compaction,
            ));
        }
        record_codex_cli_completion(
            project,
            &selected,
            &conversation_messages,
            &task_id,
            &mut traces,
            &mut step_index,
            &mut on_trace,
        )
        .await;
        return Ok(mock_agent_run(
            task_id,
            traces,
            Vec::new(),
            context_compaction,
        ));
    }

    if init_command && !supports_openai_tool_calls(&selected) {
        push_trace(
            &mut traces,
            error_trace(
                &task_id,
                step_index,
                "/init requires workspace tools",
                Some(json!({
                    "provider": selected.provider.name,
                    "type": selected.provider.provider_type,
                    "model": selected.model_id,
                })),
                "/init needs the workspace read/search/write tools. Select a tool-capable OpenAI-compatible model.",
            ),
            &mut on_trace,
        );
        return Ok(mock_agent_run(
            task_id,
            traces,
            Vec::new(),
            context_compaction,
        ));
    }

    if supports_openai_tool_calls(&selected) {
        let mcp_runtime = match tokio::time::timeout(
            MCP_AGENT_STARTUP_BUDGET,
            McpRuntime::load_from_default_config(),
        )
        .await
        {
            Ok(Ok(runtime)) => runtime,
            Ok(Err(error)) => {
                push_trace(
                    &mut traces,
                    trace(
                        &task_id,
                        step_index,
                        TraceEventType::SystemEvent,
                        None,
                        "mcp_startup skipped",
                        None,
                        Some(json!({ "error": error })),
                        Some("MCP startup failed; continuing without MCP tools".to_string()),
                        TraceStatus::Warning,
                        0,
                    ),
                    &mut on_trace,
                );
                step_index += 1;
                None
            }
            Err(_) => {
                push_trace(
                    &mut traces,
                    trace(
                        &task_id,
                        step_index,
                        TraceEventType::SystemEvent,
                        None,
                        "mcp_startup skipped",
                        None,
                        Some(json!({ "timeoutMs": MCP_AGENT_STARTUP_BUDGET.as_millis() })),
                        Some("MCP startup timed out; continuing without MCP tools".to_string()),
                        TraceStatus::Warning,
                        0,
                    ),
                    &mut on_trace,
                );
                step_index += 1;
                None
            }
        };
        if let Some(runtime) = mcp_runtime.as_ref() {
            push_trace(
                &mut traces,
                trace(
                    &task_id,
                    step_index,
                    TraceEventType::SystemEvent,
                    None,
                    "mcp_startup",
                    None,
                    Some(json!({
                        "servers": runtime.server_names(),
                        "toolCount": runtime.tool_count(),
                    })),
                    Some(format!(
                        "MCP ready: {} tool(s) from {} server(s)",
                        runtime.tool_count(),
                        runtime.server_names().len()
                    )),
                    TraceStatus::Success,
                    0,
                ),
                &mut on_trace,
            );
            step_index += 1;
        }
        let require_read_tool_call =
            should_require_local_read_tool_call(&conversation_messages, &input.user_prompt);
        let mut initial_messages = build_openai_messages_for_mode(
            project,
            &conversation_messages,
            input.cli_mode,
            &selected,
            auto_readonly_subagents,
            Some(intent_mode),
        );
        if require_read_tool_call {
            initial_messages.push(json!({
                "role": "system",
                "content": local_read_tool_required_message(),
            }));
        }
        let mut tool_context = ToolExecutionContext {
            workspace_root: &project.repo_root,
            vs_bridge_endpoint: project.vs_bridge_endpoint.as_deref(),
            allow_shell: input.allow_shell,
            assume_yes: input.assume_yes,
            cli_mode: input.cli_mode,
            goal: input.goal_slot.as_deref_mut(),
            mcp_runtime: mcp_runtime.as_ref(),
        };
        let tools = openai_tool_definitions(
            &selected,
            &tool_context,
            effective_read_only,
            auto_readonly_subagents,
        );
        run_openai_tool_agent_loop(
            &task_id,
            &selected,
            &mut tool_context,
            initial_messages,
            tools,
            &mut traces,
            &mut step_index,
            DEFAULT_MAX_TOOL_ROUNDS,
            require_read_tool_call,
            subagent_manager.as_mut(),
            &mut on_trace,
        )
        .await?;
    } else {
        record_plain_provider_completion(
            project,
            &selected,
            &conversation_messages,
            input.cli_mode,
            intent_mode,
            &task_id,
            &mut traces,
            step_index,
            &mut on_trace,
        )
        .await;
    }

    let subagent_runs = if let Some(mut manager) = subagent_manager {
        manager.finish_all().await;
        manager.into_trace_runs()
    } else {
        Vec::new()
    };
    push_subagent_fallback_final_response(
        &task_id,
        &subagent_runs,
        &mut traces,
        &mut step_index,
        &mut on_trace,
    );
    Ok(mock_agent_run(
        task_id,
        traces,
        subagent_runs,
        context_compaction,
    ))
}

/// End-to-end tool-calling test harness.
///
/// This is a demo/test entry point that exercises the OpenAI tool-calling
/// loop with a hard-coded prompt that asks the model to call
/// calculator.add. It is intentionally kept as the primary way to verify
/// the tool-calling pipeline end-to-end (the goal tools are the new
/// production path, but they don't exercise the full tool-call → result →
/// follow-up message loop the same way). Do not call this from production
/// CLI code paths; it exists for the Tauri run_tool_call_test command and
/// for integration tests.
pub async fn run_tool_call_test(
    project: &ProjectSession,
    settings: &AppSettings,
    provider_id: Option<&str>,
    credential_id: Option<&str>,
    model_id: Option<&str>,
    mut on_trace: impl FnMut(&ToolTraceEvent) + Send,
) -> Result<MockAgentRun, String> {
    let task_id = Uuid::new_v4().to_string();
    let mut traces = Vec::new();
    let mut step_index = 1;

    push_trace(
        &mut traces,
        trace(
            &task_id,
            step_index,
            TraceEventType::UserMessage,
            None,
            "user_message",
            Some(json!({
                "projectId": project.id,
                "projectName": project.name,
                "prompt": TOOL_CALL_TEST_PROMPT,
            })),
            None,
            Some("请必须调用 calculator.add 工具计算 1+1".to_string()),
            TraceStatus::Success,
            0,
        ),
        &mut on_trace,
    );
    step_index += 1;

    let selected = match select_model(settings, provider_id, credential_id, model_id, None) {
        Ok(selected) => selected,
        Err(error) => {
            push_trace(
                &mut traces,
                error_trace(
                    &task_id,
                    step_index,
                    "select_model failed",
                    Some(json!({
                        "providerId": provider_id,
                        "credentialId": credential_id,
                        "modelId": model_id,
                    })),
                    &error,
                ),
                &mut on_trace,
            );
            return Ok(mock_agent_run(task_id, traces, Vec::new(), None));
        }
    };

    if matches!(
        selected.provider.provider_type.as_str(),
        "claude" | "ollama" | CODEX_CLI_PROVIDER_TYPE
    ) {
        let error =
            "Run Tool Call Test supports OpenAI-compatible Chat Completions providers only.";
        push_trace(
            &mut traces,
            error_trace(
                &task_id,
                step_index,
                "provider_not_supported",
                Some(json!({
                    "provider": selected.provider.name,
                    "type": selected.provider.provider_type,
                    "model": selected.model_id,
                })),
                error,
            ),
            &mut on_trace,
        );
        return Ok(mock_agent_run(task_id, traces, Vec::new(), None));
    }

    let initial_messages = build_tool_call_test_messages(project);
    let mut tool_context = ToolExecutionContext {
        workspace_root: &project.repo_root,
        vs_bridge_endpoint: project.vs_bridge_endpoint.as_deref(),
        allow_shell: false,
        assume_yes: false,
        cli_mode: false,
        goal: None,
        mcp_runtime: None,
    };
    run_openai_tool_agent_loop(
        &task_id,
        &selected,
        &mut tool_context,
        initial_messages,
        tool_registry::tool_call_test_definitions(),
        &mut traces,
        &mut step_index,
        DEFAULT_MAX_TOOL_ROUNDS,
        true,
        None,
        &mut on_trace,
    )
    .await?;

    Ok(mock_agent_run(task_id, traces, Vec::new(), None))
}

fn mock_agent_run(
    task_id: String,
    traces: Vec<ToolTraceEvent>,
    subagent_runs: Vec<SubagentTraceRun>,
    context_compaction: Option<ContextCompactionResult>,
) -> MockAgentRun {
    MockAgentRun {
        task_id,
        traces,
        subagent_runs,
        context_compaction,
    }
}

async fn maybe_compact_conversation(
    project: &ProjectSession,
    selected: &SelectedModel,
    conversation_messages: &[ChatMessage],
    cli_mode: bool,
    task_id: &str,
    traces: &mut Vec<ToolTraceEvent>,
    step_index: &mut u32,
    on_trace: &mut (impl FnMut(&ToolTraceEvent) + Send),
) -> Option<AppliedContextCompaction> {
    let original_message_count = conversation_messages.len();
    if original_message_count < CONTEXT_COMPACTION_MIN_MESSAGE_COUNT {
        return None;
    }

    let estimated_original_tokens = estimate_conversation_tokens(conversation_messages);
    if estimated_original_tokens < CONTEXT_COMPACTION_ESTIMATED_TOKEN_LIMIT {
        return None;
    }

    let retain_start = retained_message_start(conversation_messages);
    if retain_start == 0 || retain_start >= original_message_count {
        return None;
    }

    let dropped_messages = &conversation_messages[..retain_start];
    let retained_messages = conversation_messages[retain_start..].to_vec();
    let prompt = context_compaction_prompt(project, dropped_messages, retained_messages.len());
    let compaction_messages = vec![chat_message("user", prompt)];

    let started = Instant::now();
    match call_provider(project, selected, &compaction_messages, cli_mode).await {
        Ok(completion) => {
            let summary = completion.message.trim().to_string();
            if summary.is_empty() {
                push_trace(
                    traces,
                    trace(
                        task_id,
                        *step_index,
                        TraceEventType::SystemEvent,
                        None,
                        "context_compaction",
                        Some(json!({
                            "originalMessageCount": original_message_count,
                            "retainedMessageCount": retained_messages.len(),
                            "droppedMessageCount": dropped_messages.len(),
                            "estimatedOriginalTokens": estimated_original_tokens,
                        })),
                        Some(json!({
                            "warning": "empty_compaction_summary",
                            "response": completion.response_body,
                            "tokenUsage": serde_json::to_value(&completion.token_usage).unwrap_or_default(),
                        })),
                        Some(
                            "Context compaction returned an empty summary; keeping full history."
                                .to_string(),
                        ),
                        TraceStatus::Warning,
                        completion.duration_ms,
                    ),
                    on_trace,
                );
                *step_index += 1;
                return None;
            }

            let summary_message = chat_message("user", compacted_context_message(&summary));
            let mut compacted_messages = vec![summary_message];
            compacted_messages.extend(retained_messages);
            let estimated_compacted_tokens = estimate_conversation_tokens(&compacted_messages);
            let result = ContextCompactionResult {
                summary: summary.clone(),
                original_message_count,
                retained_message_count: compacted_messages.len().saturating_sub(1),
                dropped_message_count: dropped_messages.len(),
                estimated_original_tokens,
                estimated_compacted_tokens,
            };

            push_trace(
                traces,
                trace(
                    task_id,
                    *step_index,
                    TraceEventType::SystemEvent,
                    None,
                    "context_compaction",
                    Some(json!({
                        "originalMessageCount": result.original_message_count,
                        "retainedMessageCount": result.retained_message_count,
                        "droppedMessageCount": result.dropped_message_count,
                        "estimatedOriginalTokens": result.estimated_original_tokens,
                        "estimatedTokenLimit": CONTEXT_COMPACTION_ESTIMATED_TOKEN_LIMIT,
                        "recentTokenBudget": CONTEXT_COMPACTION_RECENT_TOKEN_BUDGET,
                    })),
                    Some(json!({
                        "summary": summary,
                        "estimatedCompactedTokens": result.estimated_compacted_tokens,
                        "provider": selected.provider.name,
                        "model": selected.model_id,
                        "tokenUsage": serde_json::to_value(&completion.token_usage).unwrap_or_default(),
                    })),
                    Some(format!(
                        "Context compacted: {} message(s) summarized, {} recent message(s) retained.",
                        result.dropped_message_count, result.retained_message_count
                    )),
                    TraceStatus::Success,
                    completion.duration_ms,
                ),
                on_trace,
            );
            *step_index += 1;

            Some(AppliedContextCompaction {
                messages: compacted_messages,
                result,
            })
        }
        Err(error) => {
            push_trace(
                traces,
                trace(
                    task_id,
                    *step_index,
                    TraceEventType::SystemEvent,
                    None,
                    "context_compaction",
                    Some(json!({
                        "originalMessageCount": original_message_count,
                        "estimatedOriginalTokens": estimated_original_tokens,
                        "estimatedTokenLimit": CONTEXT_COMPACTION_ESTIMATED_TOKEN_LIMIT,
                    })),
                    Some(json!({
                        "error": error,
                    })),
                    Some("Context compaction failed; keeping full history.".to_string()),
                    TraceStatus::Warning,
                    started.elapsed().as_millis() as u64,
                ),
                on_trace,
            );
            *step_index += 1;
            None
        }
    }
}

fn retained_message_start(messages: &[ChatMessage]) -> usize {
    if messages.is_empty() {
        return 0;
    }

    let mut retained_tokens = 0usize;
    let mut start = messages.len().saturating_sub(1);
    for (index, message) in messages.iter().enumerate().rev() {
        let message_tokens = estimate_message_tokens(message);
        if index < messages.len() - 1
            && retained_tokens.saturating_add(message_tokens)
                > CONTEXT_COMPACTION_RECENT_TOKEN_BUDGET
        {
            break;
        }
        retained_tokens = retained_tokens.saturating_add(message_tokens);
        start = index;
    }

    if let Some(relative_index) = messages[start..]
        .iter()
        .position(|message| is_context_compaction_message(&message.content))
    {
        start = (start + relative_index + 1).min(messages.len().saturating_sub(1));
    }

    start
}

fn context_compaction_prompt(
    project: &ProjectSession,
    dropped_messages: &[ChatMessage],
    retained_message_count: usize,
) -> String {
    let mut prompt = String::new();
    prompt.push_str("Summarize the earlier part of this CodeForge coding-agent conversation for future continuation.\n\n");
    prompt.push_str("Rules:\n");
    prompt.push_str("- Preserve durable facts, user preferences, explicit constraints, decisions, unresolved tasks, files/modules inspected, and evidence already found.\n");
    prompt.push_str(
        "- Preserve whether a claim was verified from code/tool output or was only an inference.\n",
    );
    prompt.push_str("- Do not invent code facts, file paths, test results, or decisions.\n");
    prompt.push_str(
        "- Do not answer the user's latest request. Only produce the compact context summary.\n",
    );
    prompt.push_str("- Recent messages after this summarized section will be kept verbatim; avoid repeating transient wording unless it is needed for continuity.\n");
    prompt.push_str(
        "- Write in the same working language used by the conversation when possible.\n\n",
    );
    prompt.push_str("Workspace:\n");
    prompt.push_str("- Name: ");
    prompt.push_str(project.name.trim());
    prompt.push_str("\n- Root: ");
    prompt.push_str(project.repo_root.trim());
    prompt.push_str("\n- Recent messages retained verbatim after this summary: ");
    prompt.push_str(&retained_message_count.to_string());
    prompt.push_str("\n\nEarlier conversation transcript to summarize:\n");
    prompt.push_str(&conversation_transcript(dropped_messages));
    prompt.push_str("\n\nReturn only the summary.");
    prompt
}

fn conversation_transcript(messages: &[ChatMessage]) -> String {
    let mut transcript = String::new();
    for (index, message) in messages.iter().enumerate() {
        transcript.push_str("\n--- message ");
        transcript.push_str(&(index + 1).to_string());
        transcript.push_str(" / ");
        transcript.push_str(message.role.as_str());
        transcript.push_str(" ---\n");
        let content = message.content.trim();
        if content.is_empty() {
            transcript.push_str("[No text]\n");
        } else {
            transcript.push_str(content);
            transcript.push('\n');
        }
        for attachment in &message.attachments {
            transcript.push_str("[Attachment omitted: ");
            transcript.push_str(attachment.name.trim());
            transcript.push_str(" (");
            transcript.push_str(attachment.mime_type.trim());
            transcript.push_str(")]\n");
        }
    }
    transcript
}

fn compacted_context_message(summary: &str) -> String {
    format!(
        "{CONTEXT_COMPACTION_MESSAGE_PREFIX}\n\nEarlier conversation summary:\n{}",
        summary.trim()
    )
}

fn is_context_compaction_message(content: &str) -> bool {
    content
        .trim_start()
        .starts_with(CONTEXT_COMPACTION_MESSAGE_PREFIX)
}

fn estimate_conversation_tokens(messages: &[ChatMessage]) -> usize {
    messages.iter().map(estimate_message_tokens).sum()
}

fn estimate_message_tokens(message: &ChatMessage) -> usize {
    let role_tokens = message.role.len().max(1).div_ceil(4);
    let content_tokens = message.content.len().max(1).div_ceil(4);
    let attachment_tokens = message
        .attachments
        .iter()
        .map(|attachment| {
            attachment.name.len().div_ceil(4)
                + attachment.mime_type.len().div_ceil(4)
                + attachment.data_url.len().div_ceil(4)
                + 8
        })
        .sum::<usize>();
    role_tokens + content_tokens + attachment_tokens + 4
}

async fn run_openai_tool_agent_loop(
    task_id: &str,
    selected: &SelectedModel,
    tool_context: &mut ToolExecutionContext<'_>,
    mut messages: Vec<Value>,
    tools: Vec<Value>,
    traces: &mut Vec<ToolTraceEvent>,
    step_index: &mut u32,
    max_tool_rounds: usize,
    require_tool_call: bool,
    mut subagent_manager: Option<&mut SubagentManager>,
    on_trace: &mut (impl FnMut(&ToolTraceEvent) + Send),
) -> Result<(), String> {
    let mut empty_tool_call_response_retries = 0usize;
    let mut required_tool_call_response_retries = 0usize;
    let mut next_tool_choice: Option<Value> = require_tool_call.then(|| json!("required"));
    let tool_name_map = ToolNameMap::from_tools(&tools);
    let model_tools = tool_name_map.tools_for_model(&tools);

    for round_index in 0..=max_tool_rounds {
        let request = build_chat_completion_request_with_tool_choice(
            selected,
            messages.clone(),
            Some(model_tools.clone()),
            next_tool_choice.take(),
        );
        let request_title = format!("llm_request:{}", round_index + 1);
        push_trace(
            traces,
            trace(
                task_id,
                *step_index,
                TraceEventType::LlmRequest,
                None,
                &request_title,
                Some(redact_trace_value(&request)),
                None,
                Some(request_summary(&request)),
                TraceStatus::Success,
                0,
            ),
            on_trace,
        );
        *step_index += 1;

        let completion = match send_chat_completion(
            selected,
            &request,
            Some(StreamingTraceSink {
                task_id,
                step_index: *step_index,
                on_trace: &mut *on_trace,
            }),
        )
        .await
        {
            Ok(completion) => completion,
            Err(error) => {
                push_trace(
                    traces,
                    error_trace(
                        task_id,
                        *step_index,
                        &format!("{request_title} failed"),
                        Some(redact_trace_value(&request)),
                        &error,
                    ),
                    on_trace,
                );
                *step_index += 1;
                return Ok(());
            }
        };

        let response_title = format!("llm_response:{}", round_index + 1);
        push_trace(
            traces,
            trace(
                task_id,
                *step_index,
                TraceEventType::LlmResponse,
                None,
                &response_title,
                Some(json!({
                    "request": redact_trace_value(&completion.request_body),
                    "tokenUsage": serde_json::to_value(&completion.token_usage).unwrap_or_default(),
                })),
                Some(completion.response_body.clone()),
                Some(response_summary(&completion.response_body)),
                TraceStatus::Success,
                completion.duration_ms,
            ),
            on_trace,
        );
        *step_index += 1;

        let tool_calls = match parse_tool_calls(&completion.response_body) {
            Ok(tool_calls) => tool_calls,
            Err(error) => {
                push_trace(
                    traces,
                    error_trace(
                        task_id,
                        *step_index,
                        "parse_tool_calls failed",
                        Some(completion.response_body),
                        &error,
                    ),
                    on_trace,
                );
                *step_index += 1;
                return Ok(());
            }
        };
        let finish_reason = response_finish_reason(&completion.response_body);

        if !tool_calls.is_empty() {
            push_assistant_model_message_trace(
                task_id,
                traces,
                step_index,
                &completion.response_body,
                on_trace,
            );
        }

        if tool_calls.is_empty() {
            if require_tool_call
                && round_index == 0
                && required_tool_call_response_retries == 0
                && finish_reason.as_deref() != Some("tool_calls")
            {
                required_tool_call_response_retries += 1;
                push_required_tool_call_retry_trace(
                    task_id,
                    traces,
                    step_index,
                    &completion,
                    on_trace,
                );
                messages.push(json!({
                    "role": "system",
                    "content": required_tool_call_retry_message(),
                }));
                next_tool_choice = Some(json!("required"));
                continue;
            }

            if finish_reason.as_deref() == Some("tool_calls") {
                let can_retry = empty_tool_call_response_retries
                    < EMPTY_TOOL_CALL_RESPONSE_RETRY_LIMIT
                    && round_index < max_tool_rounds;
                let retry_tool_choice = can_retry.then(|| {
                    empty_tool_call_retry_tool_choice(&completion.response_body, &model_tools)
                });

                push_empty_tool_call_response_trace(
                    task_id,
                    traces,
                    step_index,
                    &completion,
                    can_retry,
                    retry_tool_choice.as_ref(),
                    on_trace,
                );

                if can_retry {
                    empty_tool_call_response_retries += 1;
                    next_tool_choice = retry_tool_choice;
                    messages.push(json!({
                        "role": "system",
                        "content": "Your previous response ended with finish_reason=tool_calls but did not include any tool_calls. The next request will require a tool call. Choose the most relevant available tool and provide valid JSON arguments. Do not return an empty assistant message.",
                    }));
                    continue;
                }

                let mut final_messages = messages.clone();
                final_messages.push(json!({
                    "role": "system",
                    "content": "Tool calling is unavailable for this response because the previous model response ended with finish_reason=tool_calls but did not include any tool_calls. Answer the user's question now in natural language without calling tools. If the workspace was not inspected, say that explicitly instead of inventing code details.",
                }));
                request_final_answer_without_tools(
                    task_id,
                    selected,
                    final_messages,
                    traces,
                    step_index,
                    round_index + 2,
                    on_trace,
                )
                .await?;
                return Ok(());
            }

            push_final_response_trace(
                task_id,
                traces,
                step_index,
                &completion,
                require_tool_call && round_index == 0,
                on_trace,
            );
            return Ok(());
        }

        if round_index >= max_tool_rounds {
            let requested_tool_names = tool_name_map.tool_call_names(&tool_calls);
            let requested_tool_names_text = requested_tool_names.join(", ");
            let requested_tool_count = tool_calls.len();
            push_trace(
                traces,
                trace(
                    task_id,
                    *step_index,
                    TraceEventType::SystemEvent,
                    None,
                    "tool_round_budget_reached",
                    Some(json!({
                        "maxToolRounds": max_tool_rounds,
                        "usedToolRounds": round_index,
                        "currentModelRound": round_index + 1,
                        "blockedToolCallCount": requested_tool_count,
                        "blockedToolNames": requested_tool_names,
                        "requestedToolCalls": tool_calls,
                    })),
                    None,
                    Some(format!(
                        "Tool round budget reached after {max_tool_rounds} tool rounds; model round {} requested {requested_tool_count} more tool call(s): {}. Asking the model to answer with available evidence.",
                        round_index + 1,
                        requested_tool_names_text
                    )),
                    TraceStatus::Warning,
                    0,
                ),
                on_trace,
            );
            *step_index += 1;

            let mut final_messages = messages.clone();
            final_messages.push(json!({
                "role": "system",
                "content": "Tool round budget reached. Do not call more tools. Answer the user's question now using the available tool results. If evidence is incomplete, state what is missing instead of requesting more tools.",
            }));
            let final_request = build_chat_completion_request(selected, final_messages, None);
            let final_request_title = format!("llm_request:{}:final", round_index + 1);
            push_trace(
                traces,
                trace(
                    task_id,
                    *step_index,
                    TraceEventType::LlmRequest,
                    None,
                    &final_request_title,
                    Some(redact_trace_value(&final_request)),
                    None,
                    Some(request_summary(&final_request)),
                    TraceStatus::Success,
                    0,
                ),
                on_trace,
            );
            *step_index += 1;

            let final_completion = match send_chat_completion(
                selected,
                &final_request,
                Some(StreamingTraceSink {
                    task_id,
                    step_index: *step_index,
                    on_trace: &mut *on_trace,
                }),
            )
            .await
            {
                Ok(completion) => completion,
                Err(error) => {
                    push_trace(
                        traces,
                        error_trace(
                            task_id,
                            *step_index,
                            &format!("{final_request_title} failed"),
                            Some(redact_trace_value(&final_request)),
                            &error,
                        ),
                        on_trace,
                    );
                    *step_index += 1;
                    return Ok(());
                }
            };

            let final_response_title = format!("llm_response:{}:final", round_index + 1);
            push_trace(
                traces,
                trace(
                    task_id,
                    *step_index,
                    TraceEventType::LlmResponse,
                    None,
                    &final_response_title,
                    Some(json!({
                        "request": redact_trace_value(&final_completion.request_body),
                    })),
                    Some(final_completion.response_body.clone()),
                    Some(response_summary(&final_completion.response_body)),
                    TraceStatus::Success,
                    final_completion.duration_ms,
                ),
                on_trace,
            );
            *step_index += 1;

            push_final_response_trace(
                task_id,
                traces,
                step_index,
                &final_completion,
                false,
                on_trace,
            );
            return Ok(());
        }

        match build_assistant_tool_call_message(&completion.response_body) {
            Ok(message) => messages.push(message),
            Err(error) => {
                push_trace(
                    traces,
                    error_trace(
                        task_id,
                        *step_index,
                        "assistant_tool_call_message failed",
                        Some(completion.response_body),
                        &error,
                    ),
                    on_trace,
                );
                *step_index += 1;
                return Ok(());
            }
        }

        let mut post_tool_messages = Vec::new();
        let mut parsed_tool_calls = Vec::new();

        for tool_call in tool_calls {
            let original_tool_name = tool_name_map.original_name(&tool_call.function.name);
            let arguments = match parse_tool_arguments(&tool_call.function.arguments) {
                Ok(arguments) => arguments,
                Err(error) => {
                    let tool_result = tool_error_result(&error);
                    push_trace(
                        traces,
                        trace(
                            task_id,
                            *step_index,
                            TraceEventType::Error,
                            Some(&original_tool_name),
                            "tool_arguments parse failed",
                            Some(json!({ "toolCall": tool_call.clone() })),
                            Some(tool_result.clone()),
                            Some(error),
                            TraceStatus::Failed,
                            0,
                        ),
                        on_trace,
                    );
                    *step_index += 1;
                    messages.push(build_tool_result_message(&tool_call, &tool_result));
                    continue;
                }
            };

            push_trace(
                traces,
                trace(
                    task_id,
                    *step_index,
                    TraceEventType::ToolCall,
                    Some(&original_tool_name),
                    "tool_call",
                    Some(json!({
                        "toolCall": tool_call.clone(),
                        "toolName": original_tool_name.clone(),
                        "arguments": arguments.clone(),
                    })),
                    None,
                    Some(tool_call_summary(&original_tool_name, &arguments)),
                    TraceStatus::Success,
                    0,
                ),
                on_trace,
            );
            *step_index += 1;

            parsed_tool_calls.push(ParsedToolCall {
                tool_call,
                tool_name: original_tool_name,
                arguments,
            });
        }

        let completed_tool_calls = execute_parsed_tool_calls(
            tool_context,
            subagent_manager.as_deref_mut(),
            parsed_tool_calls,
        )
        .await;

        for completed in completed_tool_calls {
            let tool_result = completed.result.to_model_value();
            let trace_status = if completed.result.is_ok() {
                TraceStatus::Success
            } else {
                TraceStatus::Failed
            };
            let event_type = if completed.result.is_ok() {
                TraceEventType::ToolResult
            } else {
                TraceEventType::Error
            };

            push_trace(
                traces,
                trace(
                    task_id,
                    *step_index,
                    event_type,
                    Some(&completed.tool_name),
                    "tool_result",
                    Some(json!({
                        "toolName": completed.tool_name.clone(),
                        "modelToolName": completed.tool_call.function.name.clone(),
                        "arguments": completed.arguments.clone(),
                    })),
                    Some(tool_result.clone()),
                    Some(
                        completed
                            .result
                            .summary
                            .clone()
                            .unwrap_or_else(|| tool_result_summary(&tool_result)),
                    ),
                    trace_status,
                    completed.result.elapsed_ms,
                ),
                on_trace,
            );
            *step_index += 1;

            messages.push(build_tool_result_message(
                &completed.tool_call,
                &tool_result,
            ));
            append_pptx_image_followup(
                selected,
                tool_context.workspace_root,
                task_id,
                traces,
                step_index,
                on_trace,
                &completed.tool_name,
                &completed.result,
                &mut post_tool_messages,
            )
            .await;
        }

        messages.extend(post_tool_messages);
    }

    Ok(())
}

async fn execute_parsed_tool_calls(
    tool_context: &mut ToolExecutionContext<'_>,
    subagent_manager: Option<&mut SubagentManager>,
    parsed_tool_calls: Vec<ParsedToolCall>,
) -> Vec<CompletedToolCall> {
    if should_execute_tool_calls_in_parallel(&parsed_tool_calls) {
        execute_parallel_readonly_tool_calls(tool_context, parsed_tool_calls).await
    } else {
        execute_tool_calls_sequentially(tool_context, subagent_manager, parsed_tool_calls).await
    }
}

fn should_execute_tool_calls_in_parallel(parsed_tool_calls: &[ParsedToolCall]) -> bool {
    parsed_tool_calls.len() > 1
        && parsed_tool_calls
            .iter()
            .all(|call| is_parallel_readonly_tool(&call.tool_name))
}

async fn execute_tool_calls_sequentially(
    tool_context: &mut ToolExecutionContext<'_>,
    mut subagent_manager: Option<&mut SubagentManager>,
    parsed_tool_calls: Vec<ParsedToolCall>,
) -> Vec<CompletedToolCall> {
    let mut completed = Vec::with_capacity(parsed_tool_calls.len());
    for parsed in parsed_tool_calls {
        let result = if let Some(manager) = subagent_manager.as_deref_mut() {
            if let Some(result) = manager
                .execute_tool(&parsed.tool_name, &parsed.arguments)
                .await
            {
                result
            } else {
                tool_registry::execute_tool_result(
                    tool_context,
                    &parsed.tool_name,
                    &parsed.arguments,
                )
                .await
            }
        } else {
            tool_registry::execute_tool_result(tool_context, &parsed.tool_name, &parsed.arguments)
                .await
        };
        completed.push(CompletedToolCall {
            tool_call: parsed.tool_call,
            tool_name: parsed.tool_name,
            arguments: parsed.arguments,
            result,
        });
    }
    completed
}

async fn execute_parallel_readonly_tool_calls(
    tool_context: &ToolExecutionContext<'_>,
    mut parsed_tool_calls: Vec<ParsedToolCall>,
) -> Vec<CompletedToolCall> {
    let parallel_context = ParallelToolExecutionContext::from(tool_context);
    let mut completed = Vec::with_capacity(parsed_tool_calls.len());

    while !parsed_tool_calls.is_empty() {
        let take = parsed_tool_calls.len().min(PARALLEL_READONLY_TOOL_LIMIT);
        let batch = parsed_tool_calls.drain(..take).collect::<Vec<_>>();
        let mut handles = Vec::with_capacity(batch.len());

        for parsed in batch {
            let task_context = parallel_context.clone();
            let fallback_tool_call = parsed.tool_call.clone();
            let fallback_tool_name = parsed.tool_name.clone();
            let fallback_arguments = parsed.arguments.clone();
            let handle = tauri::async_runtime::spawn(async move {
                task_context
                    .execute_readonly(parsed.tool_call, parsed.tool_name, parsed.arguments)
                    .await
            });
            handles.push((
                fallback_tool_call,
                fallback_tool_name,
                fallback_arguments,
                handle,
            ));
        }

        for (fallback_tool_call, fallback_tool_name, fallback_arguments, handle) in handles {
            let completed_tool_call = match handle.await {
                Ok(completed_tool_call) => completed_tool_call,
                Err(error) => CompletedToolCall {
                    tool_call: fallback_tool_call,
                    tool_name: fallback_tool_name,
                    arguments: fallback_arguments,
                    result: ToolOutput::error(format!("parallel_tool_join_failed: {error}"), 0),
                },
            };
            completed.push(completed_tool_call);
        }
    }

    completed
}

impl ParallelToolExecutionContext {
    fn from(tool_context: &ToolExecutionContext<'_>) -> Self {
        Self {
            workspace_root: tool_context.workspace_root.to_string(),
            vs_bridge_endpoint: tool_context.vs_bridge_endpoint.map(str::to_string),
            allow_shell: tool_context.allow_shell,
            assume_yes: tool_context.assume_yes,
            cli_mode: tool_context.cli_mode,
        }
    }

    async fn execute_readonly(
        self,
        tool_call: OpenAiToolCall,
        tool_name: String,
        arguments: Value,
    ) -> CompletedToolCall {
        let mut context = ToolExecutionContext {
            workspace_root: &self.workspace_root,
            vs_bridge_endpoint: self.vs_bridge_endpoint.as_deref(),
            allow_shell: self.allow_shell,
            assume_yes: self.assume_yes,
            cli_mode: self.cli_mode,
            goal: None,
            mcp_runtime: None,
        };
        let result = tool_registry::execute_tool_result(&mut context, &tool_name, &arguments).await;

        CompletedToolCall {
            tool_call,
            tool_name,
            arguments,
            result,
        }
    }
}

fn is_parallel_readonly_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        tool_registry::CALCULATOR_ADD_TOOL_NAME
            | tool_registry::LIST_DIR_TOOL_NAME
            | tool_registry::WORKSPACE_LIST_DIR_TOOL_NAME
            | tool_registry::READ_FILE_TOOL_NAME
            | tool_registry::WORKSPACE_READ_FILE_TOOL_NAME
            | tool_registry::SEARCH_FILE_TOOL_NAME
            | tool_registry::WORKSPACE_SEARCH_FILE_TOOL_NAME
            | tool_registry::SEARCH_CONTENT_TOOL_NAME
            | tool_registry::WORKSPACE_SEARCH_CONTENT_TOOL_NAME
            | tool_registry::WORKSPACE_SEARCH_CONTENT_ALIAS_TOOL_NAME
            | tool_registry::GET_FILE_CONTEXT_TOOL_NAME
            | tool_registry::WORKSPACE_GET_FILE_CONTEXT_TOOL_NAME
            | tool_registry::DOCUMENT_READ_DOCX_TOOL_NAME
            | tool_registry::PRESENTATION_READ_PPTX_TOOL_NAME
            | tool_registry::VS_CURRENT_SOLUTION_TOOL_NAME
            | tool_registry::VS_CURRENT_DOCUMENT_TOOL_NAME
            | tool_registry::VS_CURRENT_SELECTION_TOOL_NAME
            | tool_registry::VS_LIST_PROJECTS_TOOL_NAME
            | tool_registry::VS_LIST_PROJECT_FILES_TOOL_NAME
            | tool_registry::VS_GET_ERROR_LIST_TOOL_NAME
            | tool_registry::GOAL_GET_TOOL_NAME
    )
}

fn openai_tool_definitions(
    selected: &SelectedModel,
    tool_context: &ToolExecutionContext<'_>,
    read_only: bool,
    include_agent_tools: bool,
) -> Vec<Value> {
    let mut tools = if tool_context.cli_mode {
        tool_registry::cli_tool_definitions(
            &selected.provider.provider_type,
            &selected.model_id,
            tool_context.allow_shell,
        )
    } else if read_only {
        let mut tools = tool_registry::read_only_tool_definitions();
        if include_agent_tools {
            tools.extend(tool_registry::agent_tool_definitions());
        }
        tools
    } else {
        let mut tools = tool_registry::tool_definitions();
        if include_agent_tools {
            tools.extend(tool_registry::agent_tool_definitions());
        }
        tools
    };
    if !read_only {
        if let Some(runtime) = tool_context.mcp_runtime {
            tools.extend(runtime.tool_definitions());
        }
    }
    tools
}

async fn append_pptx_image_followup(
    selected: &SelectedModel,
    workspace_root: &str,
    task_id: &str,
    traces: &mut Vec<ToolTraceEvent>,
    step_index: &mut u32,
    on_trace: &mut (impl FnMut(&ToolTraceEvent) + Send),
    tool_name: &str,
    result: &ToolOutput,
    post_tool_messages: &mut Vec<Value>,
) {
    if tool_name != PRESENTATION_READ_PPTX_TOOL_NAME || !result.is_ok() {
        return;
    }
    let Some(output) = result.output.as_ref() else {
        return;
    };
    let image_count = pptx_output_image_count(output);
    if image_count == 0 {
        return;
    }

    if !selected_model_supports_vision(selected) {
        let message = format!(
            "The selected model is configured as text-only or has no known vision support. The previous presentation/read_pptx result found {image_count} embedded PPT image(s), but their visual contents were not sent or understood. Do not infer image contents; say explicitly that PPT images were not interpreted."
        );
        post_tool_messages.push(json!({
            "role": "user",
            "content": message,
        }));
        push_trace(
            traces,
            trace(
                task_id,
                *step_index,
                TraceEventType::ToolResult,
                Some(PRESENTATION_READ_PPTX_TOOL_NAME),
                "pptx_images_not_forwarded",
                Some(json!({
                    "model": selected.model_id,
                    "imageCount": image_count,
                    "supportsVision": false,
                })),
                Some(json!({
                    "forwarded": false,
                    "reason": "selected model does not support images",
                })),
                Some(format!(
                    "Skipped {image_count} PPT image(s): model has no vision support"
                )),
                TraceStatus::Warning,
                0,
            ),
            on_trace,
        );
        *step_index += 1;
        return;
    }

    match office_tools::pptx_model_image_attachments(workspace_root, output) {
        Ok(payload) => {
            let available = pptx_payload_attachment_count(&payload);
            let skipped = pptx_payload_skipped_count(&payload);
            if available == 0 {
                post_tool_messages.push(json!({
                    "role": "user",
                    "content": format!(
                        "The previous presentation/read_pptx result found {image_count} embedded PPT image(s), but none could be analyzed because they were unsupported, missing, or too large. Do not infer image contents."
                    ),
                }));
                push_trace(
                    traces,
                    trace(
                        task_id,
                        *step_index,
                        TraceEventType::ToolResult,
                        Some(PRESENTATION_READ_PPTX_TOOL_NAME),
                        "pptx_images_not_analyzed",
                        Some(json!({
                            "model": selected.model_id,
                            "imageCount": image_count,
                            "supportsVision": true,
                        })),
                        Some(redact_trace_value(&payload)),
                        Some(format!("Analyzed 0 PPT image(s), skipped {skipped}")),
                        TraceStatus::Warning,
                        0,
                    ),
                    on_trace,
                );
                *step_index += 1;
                return;
            }

            match analyze_pptx_images(selected, &payload, task_id, traces, step_index, on_trace)
                .await
            {
                Ok(analyses) => {
                    post_tool_messages.push(build_pptx_image_analysis_followup_message(
                        image_count,
                        skipped,
                        &analyses,
                    ));
                    push_trace(
                        traces,
                        trace(
                            task_id,
                            *step_index,
                            TraceEventType::ToolResult,
                            Some(PRESENTATION_READ_PPTX_TOOL_NAME),
                            "pptx_image_analyses_ready",
                            Some(json!({
                                "model": selected.model_id,
                                "imageCount": image_count,
                                "analyzedImageCount": available,
                                "skippedImageCount": skipped,
                            })),
                            Some(json!({
                                "analysisBatchCount": analyses.len(),
                                "imageAnalyses": analyses,
                            })),
                            Some(format!(
                                "Prepared PPT image analyses for {available} image(s), skipped {skipped}"
                            )),
                            TraceStatus::Success,
                            0,
                        ),
                        on_trace,
                    );
                    *step_index += 1;
                }
                Err(error) => {
                    post_tool_messages.push(json!({
                        "role": "user",
                        "content": format!(
                            "The previous presentation/read_pptx result found {image_count} embedded PPT image(s), but CodeForge failed to analyze them with the selected vision model: {error}. Do not infer image contents."
                        ),
                    }));
                    push_trace(
                        traces,
                        error_trace(
                            task_id,
                            *step_index,
                            "pptx image analysis failed",
                            Some(json!({
                                "model": selected.model_id,
                                "imageCount": image_count,
                            })),
                            &error,
                        ),
                        on_trace,
                    );
                    *step_index += 1;
                }
            }
        }
        Err(error) => {
            post_tool_messages.push(json!({
                "role": "user",
                "content": format!(
                    "The previous presentation/read_pptx result found {image_count} embedded PPT image(s), but CodeForge failed to prepare them for the model: {error}. Do not infer image contents."
                ),
            }));
            push_trace(
                traces,
                error_trace(
                    task_id,
                    *step_index,
                    "pptx image forwarding failed",
                    Some(json!({
                        "model": selected.model_id,
                        "imageCount": image_count,
                    })),
                    &error,
                ),
                on_trace,
            );
            *step_index += 1;
        }
    }
}

async fn analyze_pptx_images(
    selected: &SelectedModel,
    payload: &Value,
    task_id: &str,
    traces: &mut Vec<ToolTraceEvent>,
    step_index: &mut u32,
    on_trace: &mut (impl FnMut(&ToolTraceEvent) + Send),
) -> Result<Vec<Value>, String> {
    let attachments = payload
        .get("attachments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let total_images = attachments.len();
    let mut analyses = Vec::new();

    for (batch_index, batch) in attachments
        .chunks(PPTX_IMAGE_ANALYSIS_BATCH_SIZE)
        .enumerate()
    {
        let batch_number = batch_index + 1;
        let messages = vec![
            json!({
                "role": "system",
                "content": "You are analyzing embedded images extracted from a PowerPoint file. Return concise, structured JSON. Do not invent details that are not visible."
            }),
            build_pptx_image_analysis_request_message(batch, batch_number, total_images),
        ];
        let request = build_chat_completion_request(selected, messages, None);
        push_trace(
            traces,
            trace(
                task_id,
                *step_index,
                TraceEventType::LlmRequest,
                None,
                &format!("pptx_image_analysis_request:{batch_number}"),
                Some(redact_trace_value(&request)),
                None,
                Some(format!(
                    "Analyzing PPT image batch {batch_number} ({} image(s))",
                    batch.len()
                )),
                TraceStatus::Success,
                0,
            ),
            on_trace,
        );
        *step_index += 1;

        let completion = send_chat_completion(selected, &request, None).await?;
        let message = extract_message_from_response(&completion.response_body)
            .unwrap_or_default()
            .trim()
            .to_string();
        let metadata = batch
            .iter()
            .map(pptx_image_attachment_metadata)
            .collect::<Vec<_>>();
        let analysis = json!({
            "batchIndex": batch_number,
            "imageCount": batch.len(),
            "images": metadata,
            "analysis": message.clone(),
            "durationMs": completion.duration_ms,
            "inputTokens": completion.token_usage.input_tokens,
            "outputTokens": completion.token_usage.output_tokens,
            "totalTokens": completion.token_usage.total_tokens,
        });

        push_trace(
            traces,
            trace(
                task_id,
                *step_index,
                TraceEventType::LlmResponse,
                None,
                &format!("pptx_image_analysis_response:{batch_number}"),
                Some(json!({
                    "model": selected.model_id,
                    "imageCount": batch.len(),
                })),
                Some(json!({
                    "response": completion.response_body.clone(),
                    "analysis": message,
                    "tokenUsage": completion.token_usage.clone(),
                })),
                Some(format!(
                    "Analyzed PPT image batch {batch_number}: {} chars",
                    analysis
                        .get("analysis")
                        .and_then(Value::as_str)
                        .map(str::len)
                        .unwrap_or(0)
                )),
                TraceStatus::Success,
                completion.duration_ms,
            ),
            on_trace,
        );
        *step_index += 1;

        analyses.push(analysis);
    }

    Ok(analyses)
}

fn selected_model_supports_vision(selected: &SelectedModel) -> bool {
    if let Some(model) = selected.model.as_ref() {
        return model
            .supports_vision
            .unwrap_or_else(|| infer_model_supports_vision(&model.id, &model.name));
    }
    infer_model_supports_vision(&selected.model_id, &selected.model_id)
}

fn pptx_output_image_count(output: &Value) -> usize {
    output
        .get("slides")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|slide| {
            slide
                .get("images")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0)
        })
        .sum()
}

fn pptx_payload_attachment_count(payload: &Value) -> usize {
    payload
        .get("attachments")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}

fn pptx_payload_skipped_count(payload: &Value) -> usize {
    payload
        .get("skipped")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}

fn build_pptx_image_analysis_request_message(
    attachments: &[Value],
    batch_number: usize,
    total_images: usize,
) -> Value {
    let image_metadata = attachments
        .iter()
        .map(pptx_image_attachment_metadata)
        .collect::<Vec<_>>();
    let mut text = format!(
        "Analyze this PowerPoint image batch {batch_number}. Total extracted PPT images: {total_images}. Return JSON only with an `images` array. For each image, include: imageIndex, slideIndex, target, description, visibleText, diagramMeaning, relevance (high|medium|low|decorative), and needsRecheck (boolean). Image metadata:\n{}\n",
        serde_json::to_string_pretty(&image_metadata).unwrap_or_else(|_| "[]".to_string())
    );
    text.push_str(
        "If an image is a logo/background/decorative asset, mark relevance as decorative and keep the description short. Do not OCR as a separate process; just report visible text if the vision model can read it.",
    );

    let mut content = vec![json!({
        "type": "text",
        "text": text,
    })];
    for attachment in attachments {
        if let Some(data_url) = attachment.get("dataUrl").and_then(Value::as_str) {
            content.push(json!({
                "type": "image_url",
                "image_url": {
                    "url": data_url,
                },
            }));
        }
    }

    json!({
        "role": "user",
        "content": content,
    })
}

fn build_pptx_image_analysis_followup_message(
    original_image_count: usize,
    skipped: usize,
    analyses: &[Value],
) -> Value {
    let analyzed = analyses
        .iter()
        .map(|analysis| {
            analysis
                .get("imageCount")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize
        })
        .sum::<usize>();
    let text = format!(
        "PPT embedded image understanding is complete. Use these image analyses together with the previous presentation/read_pptx slide text and notes. Original embedded images: {original_image_count}; analyzed: {analyzed}; skipped: {skipped}. If a later answer needs more precision for a specific image, ask to recheck that slideIndex/target instead of guessing.\n\nimageAnalyses:\n{}",
        serde_json::to_string_pretty(analyses).unwrap_or_else(|_| "[]".to_string())
    );

    json!({
        "role": "user",
        "content": text,
    })
}

fn pptx_image_attachment_metadata(attachment: &Value) -> Value {
    json!({
        "name": attachment.get("name").and_then(Value::as_str),
        "mimeType": attachment.get("mimeType").and_then(Value::as_str),
        "slideIndex": attachment.get("slideIndex").and_then(Value::as_u64),
        "target": attachment.get("target").and_then(Value::as_str),
    })
}

fn push_assistant_model_message_trace(
    task_id: &str,
    traces: &mut Vec<ToolTraceEvent>,
    step_index: &mut u32,
    response_body: &Value,
    on_trace: &mut (impl FnMut(&ToolTraceEvent) + Send),
) {
    let message = extract_message_from_response(response_body)
        .unwrap_or_default()
        .trim()
        .to_string();
    if message.is_empty() {
        return;
    }

    push_trace(
        traces,
        trace(
            task_id,
            *step_index,
            TraceEventType::ModelMessage,
            None,
            "model_message",
            None,
            Some(json!({
                "message": message.clone(),
            })),
            Some(message),
            TraceStatus::Success,
            0,
        ),
        on_trace,
    );
    *step_index += 1;
}

async fn request_final_answer_without_tools(
    task_id: &str,
    selected: &SelectedModel,
    messages: Vec<Value>,
    traces: &mut Vec<ToolTraceEvent>,
    step_index: &mut u32,
    request_index: usize,
    on_trace: &mut (impl FnMut(&ToolTraceEvent) + Send),
) -> Result<(), String> {
    let final_request = build_chat_completion_request(selected, messages, None);
    let final_request_title = format!("llm_request:{request_index}:no_tools_final");
    push_trace(
        traces,
        trace(
            task_id,
            *step_index,
            TraceEventType::LlmRequest,
            None,
            &final_request_title,
            Some(redact_trace_value(&final_request)),
            None,
            Some(request_summary(&final_request)),
            TraceStatus::Success,
            0,
        ),
        on_trace,
    );
    *step_index += 1;

    let final_completion = match send_chat_completion(
        selected,
        &final_request,
        Some(StreamingTraceSink {
            task_id,
            step_index: *step_index,
            on_trace: &mut *on_trace,
        }),
    )
    .await
    {
        Ok(completion) => completion,
        Err(error) => {
            push_trace(
                traces,
                error_trace(
                    task_id,
                    *step_index,
                    &format!("{final_request_title} failed"),
                    Some(redact_trace_value(&final_request)),
                    &error,
                ),
                on_trace,
            );
            *step_index += 1;
            return Ok(());
        }
    };

    let final_response_title = format!("llm_response:{request_index}:no_tools_final");
    push_trace(
        traces,
        trace(
            task_id,
            *step_index,
            TraceEventType::LlmResponse,
            None,
            &final_response_title,
            Some(json!({
                "request": redact_trace_value(&final_completion.request_body),
            })),
            Some(final_completion.response_body.clone()),
            Some(response_summary(&final_completion.response_body)),
            TraceStatus::Success,
            final_completion.duration_ms,
        ),
        on_trace,
    );
    *step_index += 1;

    push_final_response_trace(
        task_id,
        traces,
        step_index,
        &final_completion,
        false,
        on_trace,
    );
    Ok(())
}

fn push_empty_tool_call_response_trace(
    task_id: &str,
    traces: &mut Vec<ToolTraceEvent>,
    step_index: &mut u32,
    completion: &ChatCompletionResult,
    retrying: bool,
    retry_tool_choice: Option<&Value>,
    on_trace: &mut (impl FnMut(&ToolTraceEvent) + Send),
) {
    let summary = if retrying {
        "Model ended with finish_reason=tool_calls but returned no tool_calls; retrying once."
    } else {
        "Model ended with finish_reason=tool_calls but returned no tool_calls; requesting a final answer without tools."
    };

    push_trace(
        traces,
        trace(
            task_id,
            *step_index,
            TraceEventType::SystemEvent,
            None,
            "empty_tool_call_response",
            Some(json!({
                "request": redact_trace_value(&completion.request_body),
            })),
            Some(json!({
                "response": completion.response_body.clone(),
                "message": "",
                "warning": "empty_tool_call_response",
                "retrying": retrying,
                "retryToolChoice": retry_tool_choice.cloned(),
                "tokenUsage": serde_json::to_value(&completion.token_usage).unwrap_or_default(),
            })),
            Some(summary.to_string()),
            TraceStatus::Warning,
            completion.duration_ms,
        ),
        on_trace,
    );
    *step_index += 1;
}

fn push_required_tool_call_retry_trace(
    task_id: &str,
    traces: &mut Vec<ToolTraceEvent>,
    step_index: &mut u32,
    completion: &ChatCompletionResult,
    on_trace: &mut (impl FnMut(&ToolTraceEvent) + Send),
) {
    push_trace(
        traces,
        trace(
            task_id,
            *step_index,
            TraceEventType::SystemEvent,
            None,
            "required_tool_call_missing",
            Some(json!({
                "request": redact_trace_value(&completion.request_body),
            })),
            Some(json!({
                "response": completion.response_body.clone(),
                "warning": "required_tool_call_missing",
                "retrying": true,
                "tokenUsage": serde_json::to_value(&completion.token_usage).unwrap_or_default(),
            })),
            Some("Model answered without the required tool call; retrying once with tool_choice=required.".to_string()),
            TraceStatus::Warning,
            completion.duration_ms,
        ),
        on_trace,
    );
    *step_index += 1;
}

fn empty_tool_call_retry_tool_choice(response_body: &Value, tools: &[Value]) -> Value {
    let available_names = tool_definition_names(tools);
    let intent = response_intent_text(response_body);
    if available_names.iter().any(|name| name == "search_file") && intent_indicates_search(&intent)
    {
        return json!({
            "type": "function",
            "function": { "name": "search_file" },
        });
    }
    json!("required")
}

fn tool_definition_names(tools: &[Value]) -> Vec<String> {
    tools
        .iter()
        .filter_map(|tool| tool.get("function")?.get("name")?.as_str())
        .map(str::to_string)
        .collect()
}

fn response_intent_text(response_body: &Value) -> String {
    parse_openai_response(response_body)
        .ok()
        .and_then(|parsed| parsed.choices.into_iter().next())
        .map(|choice| {
            [
                choice.message.reasoning_content.unwrap_or_default(),
                choice.message.content.unwrap_or_default(),
            ]
            .join("\n")
        })
        .unwrap_or_default()
}

fn intent_indicates_search(intent: &str) -> bool {
    let intent = intent.to_ascii_lowercase();
    [
        "search",
        "find",
        "locate",
        "relevant file",
        "related file",
        "workspace",
        "repository",
        "repo",
        "codebase",
        "查找",
        "搜索",
        "相关文件",
        "代码",
        "仓库",
    ]
    .iter()
    .any(|needle| intent.contains(needle))
}

fn push_final_response_trace(
    task_id: &str,
    traces: &mut Vec<ToolTraceEvent>,
    step_index: &mut u32,
    completion: &ChatCompletionResult,
    warn_if_no_tool_call: bool,
    on_trace: &mut (impl FnMut(&ToolTraceEvent) + Send),
) {
    let final_message = extract_message_from_response(&completion.response_body)
        .unwrap_or_default()
        .trim()
        .to_string();
    let final_summary = if warn_if_no_tool_call {
        "model_did_not_call_tool".to_string()
    } else if final_message.is_empty() {
        "Final response was empty".to_string()
    } else {
        final_message.clone()
    };
    let title = if warn_if_no_tool_call {
        "model_did_not_call_tool"
    } else {
        "final_response"
    };
    let status = if warn_if_no_tool_call || final_message.is_empty() {
        TraceStatus::Warning
    } else {
        TraceStatus::Success
    };

    push_trace(
        traces,
        trace(
            task_id,
            *step_index,
            TraceEventType::FinalResponse,
            None,
            title,
            Some(json!({
                "request": redact_trace_value(&completion.request_body),
            })),
            Some(json!({
                "response": completion.response_body.clone(),
                "message": final_message,
                "warning": if warn_if_no_tool_call {
                    Some("model_did_not_call_tool")
                } else {
                    None
                },
                "tokenUsage": serde_json::to_value(&completion.token_usage).unwrap_or_default(),
            })),
            Some(final_summary),
            status,
            completion.duration_ms,
        ),
        on_trace,
    );
    *step_index += 1;
}

fn push_subagent_fallback_final_response(
    task_id: &str,
    subagent_runs: &[SubagentTraceRun],
    traces: &mut Vec<ToolTraceEvent>,
    step_index: &mut u32,
    on_trace: &mut (impl FnMut(&ToolTraceEvent) + Send),
) {
    if !needs_subagent_fallback_final_response(traces) {
        return;
    }
    let Some(message) = build_subagent_fallback_message(subagent_runs) else {
        return;
    };

    push_trace(
        traces,
        trace(
            task_id,
            *step_index,
            TraceEventType::FinalResponse,
            None,
            "final_response",
            Some(json!({
                "source": "subagent_fallback",
                "reason": "parent_final_response_empty",
                "subagentCount": subagent_runs.len(),
            })),
            Some(json!({
                "message": message,
                "source": "subagent_fallback",
            })),
            Some(message),
            TraceStatus::Success,
            0,
        ),
        on_trace,
    );
    *step_index += 1;
}

fn needs_subagent_fallback_final_response(traces: &[ToolTraceEvent]) -> bool {
    let final_responses = traces
        .iter()
        .filter(|event| matches!(event.event_type, TraceEventType::FinalResponse))
        .collect::<Vec<_>>();
    if final_responses.iter().any(|event| {
        matches!(event.status, TraceStatus::Success)
            && event
                .output_summary
                .as_deref()
                .is_some_and(|summary| !summary.trim().is_empty())
    }) {
        return false;
    }

    final_responses.last().is_some_and(|event| {
        matches!(event.status, TraceStatus::Warning)
            && event.output_summary.as_deref() == Some("Final response was empty")
    })
}

fn build_subagent_fallback_message(subagent_runs: &[SubagentTraceRun]) -> Option<String> {
    let summaries = subagent_runs
        .iter()
        .filter(|run| run.status == "completed")
        .filter_map(|run| {
            let summary = run.summary.as_deref()?.trim();
            if summary.is_empty() || summary == "Final response was empty" {
                return None;
            }
            Some((run, summary))
        })
        .collect::<Vec<_>>();

    if summaries.is_empty() {
        return None;
    }
    let mut sections = Vec::new();
    sections.push("父级模型返回了空最终回答；下面是已完成子任务的汇总结果。".to_string());
    for (run, summary) in summaries {
        let title = if run.task_name.trim().is_empty() {
            run.agent_name.trim()
        } else {
            run.task_name.trim()
        };
        sections.push(format!("## {title}\n\n{summary}"));
    }
    Some(sections.join("\n\n"))
}

async fn record_plain_provider_completion(
    project: &ProjectSession,
    selected: &SelectedModel,
    conversation_messages: &[ChatMessage],
    cli_mode: bool,
    intent_mode: AgentIntentMode,
    task_id: &str,
    traces: &mut Vec<ToolTraceEvent>,
    step_index: u32,
    on_trace: &mut (impl FnMut(&ToolTraceEvent) + Send),
) {
    match call_provider_for_mode(
        project,
        selected,
        conversation_messages,
        cli_mode,
        Some(intent_mode),
    )
    .await
    {
        Ok(completion) => {
            let message = completion.message;
            let message_chars = message.chars().count();
            let trace_request = redact_trace_value(&completion.request_body);
            push_trace(
                traces,
                trace(
                    task_id,
                    step_index,
                    TraceEventType::ToolResult,
                    Some("chat_completion"),
                    "chat_completion",
                    Some(json!({
                        "provider": selected.provider.name,
                        "type": selected.provider.provider_type,
                        "baseUrl": selected.provider.base_url,
                        "request": trace_request,
                    })),
                    Some(json!({
                        "provider": selected.provider.name,
                        "type": selected.provider.provider_type,
                        "baseUrl": selected.provider.base_url,
                        "response": completion.response_body,
                        "message": message.clone(),
                        "messageChars": message_chars,
                        "model": selected.model_id,
                        "inputTokens": completion.token_usage.input_tokens,
                        "outputTokens": completion.token_usage.output_tokens,
                        "totalTokens": completion.token_usage.total_tokens,
                        "inputCachedTokens": completion.token_usage.input_cached_tokens,
                        "inputUncachedTokens": completion.token_usage.input_uncached_tokens,
                    })),
                    Some(format!("Received {message_chars} chars")),
                    TraceStatus::Success,
                    completion.duration_ms,
                ),
                on_trace,
            );
            push_trace(
                traces,
                trace(
                    task_id,
                    step_index + 1,
                    TraceEventType::ModelMessage,
                    None,
                    "model_message",
                    None,
                    Some(json!({ "message": message.clone() })),
                    Some(message),
                    TraceStatus::Success,
                    0,
                ),
                on_trace,
            );
        }
        Err(error) => {
            push_trace(
                traces,
                error_trace(
                    task_id,
                    step_index,
                    "chat_completion failed",
                    Some(json!({
                        "provider": selected.provider.name,
                        "credential": selected
                            .credential
                            .as_ref()
                            .map(|credential| credential.name.clone()),
                        "type": selected.provider.provider_type,
                        "baseUrl": selected.provider.base_url,
                        "model": selected.model_id,
                        "messages": conversation_messages,
                        "apiKey": mask_secret(&selected.credential_api_key()),
                    })),
                    &error,
                ),
                on_trace,
            );
        }
    }
}

async fn record_codex_cli_completion(
    project: &ProjectSession,
    selected: &SelectedModel,
    conversation_messages: &[ChatMessage],
    task_id: &str,
    traces: &mut Vec<ToolTraceEvent>,
    step_index: &mut u32,
    on_trace: &mut (impl FnMut(&ToolTraceEvent) + Send),
) {
    let prompt = build_codex_cli_prompt(project, conversation_messages);
    let model_override = codex_cli_runner::model_override(&selected.model_id);
    push_trace(
        traces,
        trace(
            task_id,
            *step_index,
            TraceEventType::ToolCall,
            Some(CODEX_CLI_TOOL_NAME),
            "codex_exec",
            Some(json!({
                "provider": selected.provider.name,
                "type": selected.provider.provider_type,
                "workspaceRoot": project.repo_root,
                "sandbox": "workspace-write",
            "model": selected.model_id,
            "modelOverride": model_override,
            "reasoningEffort": selected.reasoning_effort,
            "prompt": prompt.clone(),
            })),
            None,
            Some("codex exec --json".to_string()),
            TraceStatus::Success,
            0,
        ),
        on_trace,
    );
    *step_index += 1;

    match codex_cli_runner::execute(
        &project.repo_root,
        &prompt,
        model_override,
        selected.reasoning_effort.as_deref(),
    )
    .await
    {
        Ok(execution) => {
            let usage = codex_cli_runner::token_usage_from_codex_usage(execution.usage.as_ref());
            let duration_ms = execution.duration_ms;
            let exit_code = execution.exit_code;
            let timed_out = execution.timed_out;
            let final_message = execution.final_message.clone();
            let output = json!({
                "executable": execution.executable,
                "args": execution.args,
                "exitCode": exit_code,
                "timedOut": timed_out,
                "durationMs": duration_ms,
                "stderr": execution.stderr,
                "stdoutLineCount": execution.stdout.lines().count(),
                "promptWriteError": execution.prompt_write_error,
                "events": codex_cli_runner::limited_events(&execution.events),
                "nonJsonStdoutLines": execution.non_json_stdout_lines,
                "finalMessage": final_message.clone(),
                "usage": execution.usage,
                "tokenUsage": usage.clone(),
            });

            if timed_out || !exit_code.is_some_and(|code| code == 0) {
                let summary = if timed_out {
                    "Codex CLI timed out".to_string()
                } else {
                    format!(
                        "Codex CLI failed with exit code {}",
                        exit_code
                            .map(|code| code.to_string())
                            .unwrap_or_else(|| "unknown".to_string())
                    )
                };
                push_trace(
                    traces,
                    trace(
                        task_id,
                        *step_index,
                        TraceEventType::Error,
                        Some(CODEX_CLI_TOOL_NAME),
                        "codex_exec failed",
                        Some(json!({
                            "provider": selected.provider.name,
                            "type": selected.provider.provider_type,
                            "workspaceRoot": project.repo_root,
                            "model": selected.model_id,
                        })),
                        Some(output),
                        Some(summary),
                        TraceStatus::Failed,
                        duration_ms,
                    ),
                    on_trace,
                );
                *step_index += 1;
                return;
            }

            let final_message = final_message.trim().to_string();
            let message_chars = final_message.chars().count();
            let summary = if final_message.is_empty() {
                "Codex CLI completed without a final message".to_string()
            } else {
                format!("Codex CLI returned {message_chars} chars")
            };
            let status = if final_message.is_empty() {
                TraceStatus::Warning
            } else {
                TraceStatus::Success
            };

            push_trace(
                traces,
                trace(
                    task_id,
                    *step_index,
                    TraceEventType::ToolResult,
                    Some(CODEX_CLI_TOOL_NAME),
                    "codex_exec",
                    Some(json!({
                        "provider": selected.provider.name,
                        "type": selected.provider.provider_type,
                        "workspaceRoot": project.repo_root,
                        "model": selected.model_id,
                    })),
                    Some(output),
                    Some(summary),
                    status.clone(),
                    duration_ms,
                ),
                on_trace,
            );
            *step_index += 1;

            push_trace(
                traces,
                trace(
                    task_id,
                    *step_index,
                    TraceEventType::FinalResponse,
                    None,
                    "final_response",
                    Some(json!({
                        "provider": selected.provider.name,
                        "type": selected.provider.provider_type,
                        "model": selected.model_id,
                    })),
                    Some(json!({
                        "message": final_message,
                        "provider": selected.provider.name,
                        "type": selected.provider.provider_type,
                        "model": selected.model_id,
                        "tokenUsage": usage,
                    })),
                    Some(if final_message.is_empty() {
                        "Final response was empty".to_string()
                    } else {
                        final_message
                    }),
                    status,
                    0,
                ),
                on_trace,
            );
            *step_index += 1;
        }
        Err(error) => {
            push_trace(
                traces,
                error_trace(
                    task_id,
                    *step_index,
                    "codex_exec failed",
                    Some(json!({
                        "provider": selected.provider.name,
                        "type": selected.provider.provider_type,
                        "workspaceRoot": project.repo_root,
                        "model": selected.model_id,
                    })),
                    &error,
                ),
                on_trace,
            );
            *step_index += 1;
        }
    }
}

fn supports_openai_tool_calls(selected: &SelectedModel) -> bool {
    !matches!(
        selected.provider.provider_type.as_str(),
        "claude" | "ollama" | CODEX_CLI_PROVIDER_TYPE
    )
}

fn select_model(
    settings: &AppSettings,
    provider_id: Option<&str>,
    credential_id: Option<&str>,
    model_id: Option<&str>,
    reasoning_effort: Option<&str>,
) -> Result<SelectedModel, String> {
    let provider = if let Some(provider_id) = provider_id.filter(|value| !value.trim().is_empty()) {
        let requested_provider = settings
            .providers
            .iter()
            .find(|provider| provider.id == provider_id)
            .ok_or_else(|| format!("Provider not found: {provider_id}"))?;

        if is_provider_usable(requested_provider) {
            requested_provider.clone()
        } else {
            return Err(format!(
                "Provider is disabled: {}. Enable the provider, one model, and one credential in Settings first.",
                requested_provider.name
            ));
        }
    } else {
        settings
            .providers
            .iter()
            .find(|provider| is_provider_usable(provider))
            .cloned()
            .ok_or_else(|| {
                "No enabled provider. Enable a provider or model in Settings first.".to_string()
            })?
    };

    let credential = select_credential(&provider, credential_id)?;

    let model_id = model_id
        .filter(|value| {
            !value.trim().is_empty()
                && provider.models.iter().any(|model| {
                    model_is_enabled_for_credential(model, credential.as_ref())
                        && model.id == *value
                })
        })
        .map(str::to_string)
        .or_else(|| {
            provider
                .models
                .iter()
                .find(|model| model_is_enabled_for_credential(model, credential.as_ref()))
                .map(|model| model.id.clone())
        })
        .or_else(|| default_model_without_model_list(&provider))
        .ok_or_else(|| {
            format!(
                "No enabled model for provider {}. Enable at least one model in Settings first.",
                provider.name
            )
        })?;

    if model_id.trim().is_empty() {
        return Err(format!("Model is empty for provider {}", provider.name));
    }

    let selected_model = provider
        .models
        .iter()
        .find(|model| {
            model_is_enabled_for_credential(model, credential.as_ref()) && model.id == model_id
        })
        .cloned();
    let reasoning_effort =
        normalize_model_reasoning_effort(reasoning_effort, selected_model.as_ref());

    Ok(SelectedModel {
        provider,
        credential,
        model_id,
        model: selected_model,
        reasoning_effort,
    })
}

fn normalize_reasoning_effort(reasoning_effort: Option<&str>) -> Option<String> {
    let value = reasoning_effort?.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("default") {
        return None;
    }
    Some(value.to_string())
}

fn normalize_model_reasoning_effort(
    reasoning_effort: Option<&str>,
    model: Option<&ProviderModel>,
) -> Option<String> {
    let explicit = normalize_reasoning_effort(reasoning_effort);
    let Some(model) = model else {
        return explicit;
    };
    if let Some(reasoning) = model.reasoning.as_ref().filter(|config| !config.levels.is_empty()) {
        let requested = explicit.as_deref().or_else(|| {
            let default = model.default_reasoning.trim();
            (!default.is_empty()).then_some(default)
        });
        return matching_model_reasoning_level(reasoning, requested)
            .or_else(|| matching_model_reasoning_level(reasoning, Some(&reasoning.default)))
            .or_else(|| reasoning.levels.first().map(|level| level.level.clone()));
    }
    let value = explicit
        .as_deref()
        .or_else(|| {
            let default = model.default_reasoning.trim();
            (!default.is_empty()).then_some(default)
        })?
        .trim()
        .to_ascii_lowercase();
    match resolved_model_reasoning_mode(model) {
        // For thinking-only models (e.g. minimax-m3), the UI exposes
        // Low/Medium/High/Extra High but only Low vs others is meaningful:
        // Low turns thinking off, anything above turns thinking on.
        "toggle" => match value.as_str() {
            "on" | "minimal" | "medium" | "high" | "xhigh" => Some("on".to_string()),
            "off" | "none" | "low" => Some("off".to_string()),
            _ => None,
        },
        "effort" => match value.as_str() {
            "minimal" | "low" | "medium" | "high" | "xhigh" => Some(value),
            "on" => Some("medium".to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn matching_model_reasoning_level(
    reasoning: &crate::vs_registry::ModelReasoningConfig,
    requested: Option<&str>,
) -> Option<String> {
    let requested = requested?.trim();
    if requested.is_empty() {
        return None;
    }
    reasoning
        .levels
        .iter()
        .find(|level| level.level.eq_ignore_ascii_case(requested))
        .map(|level| level.level.clone())
}

fn default_model_without_model_list(provider: &ProviderConfig) -> Option<String> {
    if !provider.models.is_empty() {
        return None;
    }
    if provider.provider_type != CODEX_CLI_PROVIDER_TYPE && provider.provider_type != "ollama" {
        return None;
    }
    let default_model = provider.default_model.trim();
    (!default_model.is_empty()).then(|| default_model.to_string())
}

fn model_is_enabled_for_credential(
    model: &ProviderModel,
    credential: Option<&ProviderCredential>,
) -> bool {
    if !model.enabled {
        return false;
    }
    let model_credential_id = model.credential_id.trim();
    if model_credential_id.is_empty() {
        return true;
    }
    credential
        .map(|credential| credential.id == model_credential_id)
        .unwrap_or(false)
}

fn is_provider_usable(provider: &ProviderConfig) -> bool {
    let model_enabled = provider.models.iter().any(|model| model.enabled);
    if provider.provider_type == "ollama" || provider.provider_type == CODEX_CLI_PROVIDER_TYPE {
        return provider.enabled || model_enabled;
    }
    (provider.enabled
        || model_enabled
        || provider
            .credentials
            .iter()
            .any(|credential| credential.enabled))
        && provider
            .credentials
            .iter()
            .any(|credential| credential.enabled)
}

fn select_credential(
    provider: &ProviderConfig,
    credential_id: Option<&str>,
) -> Result<Option<ProviderCredential>, String> {
    if provider.provider_type == "ollama" || provider.provider_type == CODEX_CLI_PROVIDER_TYPE {
        return Ok(None);
    }

    if let Some(credential_id) = credential_id.filter(|value| !value.trim().is_empty()) {
        let credential = provider
            .credentials
            .iter()
            .find(|credential| credential.id == credential_id)
            .ok_or_else(|| {
                format!(
                    "Credential not found: {} for provider {}",
                    credential_id, provider.name
                )
            })?;
        if !credential.enabled {
            return Err(format!(
                "Credential is disabled: {} for provider {}",
                credential.name, provider.name
            ));
        }
        return Ok(Some(credential.clone()));
    }

    provider
        .credentials
        .iter()
        .find(|credential| credential.id == provider.default_credential_id && credential.enabled)
        .or_else(|| {
            provider
                .credentials
                .iter()
                .find(|credential| credential.enabled)
        })
        .cloned()
        .map(Some)
        .ok_or_else(|| format!("No enabled credential for provider {}", provider.name))
}

async fn call_provider(
    project: &ProjectSession,
    selected: &SelectedModel,
    conversation_messages: &[ChatMessage],
    cli_mode: bool,
) -> Result<ProviderCompletion, String> {
    call_provider_for_mode(project, selected, conversation_messages, cli_mode, None).await
}

async fn call_provider_for_mode(
    project: &ProjectSession,
    selected: &SelectedModel,
    conversation_messages: &[ChatMessage],
    cli_mode: bool,
    intent_mode: Option<AgentIntentMode>,
) -> Result<ProviderCompletion, String> {
    let provider_type = selected.provider.provider_type.as_str();
    if provider_type == "claude" {
        return call_claude_for_mode(
            project,
            selected,
            conversation_messages,
            cli_mode,
            intent_mode,
        )
        .await;
    }
    if provider_type == "ollama" {
        return call_ollama_for_mode(
            project,
            selected,
            conversation_messages,
            cli_mode,
            intent_mode,
        )
        .await;
    }
    if provider_type == CODEX_CLI_PROVIDER_TYPE {
        return Err("Codex CLI provider must be executed through codex exec.".to_string());
    }
    call_openai_compatible_for_mode(
        project,
        selected,
        conversation_messages,
        cli_mode,
        intent_mode,
    )
    .await
}

fn build_chat_completion_request(
    selected: &SelectedModel,
    messages: Vec<Value>,
    tools: Option<Vec<Value>>,
) -> Value {
    build_chat_completion_request_with_tool_choice(selected, messages, tools, None)
}

fn build_chat_completion_request_with_tool_choice(
    selected: &SelectedModel,
    messages: Vec<Value>,
    tools: Option<Vec<Value>>,
    tool_choice: Option<Value>,
) -> Value {
    let uses_streaming = true;
    let mut request_body = json!({
        "model": selected.model_id,
        "messages": messages,
        "stream": uses_streaming,
    });

    if !apply_custom_model_reasoning_request(&mut request_body, selected) {
        let is_toggle_mode =
            selected.model.as_ref().map(resolved_model_reasoning_mode) == Some("toggle");

        if is_toggle_mode {
            // MiniMax-M3 via /v1/chat/completions:
            // - `thinking` is a top-level object, not `reasoning.effort`
            //   (that shape belongs to /v1/responses and is silently dropped
            //   on this endpoint).
            // - Valid `type` values are `adaptive` and `disabled`. `enabled`
            //   returns HTTP 400.
            // - Default is `adaptive` (thinking on); only flip to `disabled`
            //   when the user explicitly asks for off via --reasoning off|none.
            // - `reasoning_split: true` surfaces the thinking into a separate
            //   `reasoning_content` field in the response; without it the
            //   thinking stays embedded inside `content` as a `<think>` tag,
            //   which the streaming parser does not extract.
            request_body["reasoning_split"] = json!(true);
            let thinking_type = match selected.reasoning_effort.as_deref() {
                Some("off") | Some("none") => "disabled",
                _ => "adaptive",
            };
            request_body["thinking"] = json!({ "type": thinking_type });
        } else if let Some(reasoning_effort) = selected.reasoning_effort.as_deref() {
            // Other providers (e.g. OpenAI o1/o3) accept top-level
            // `reasoning_effort`.
            request_body["reasoning_effort"] = json!(reasoning_effort);
        }
    }

    if let Some(tools) = tools {
        request_body["tools"] = json!(tools);
        request_body["tool_choice"] = normalize_tool_choice_for_provider(
            selected,
            tool_choice.unwrap_or_else(|| json!("auto")),
        );
    }

    if uses_streaming {
        request_body["stream_options"] = json!({ "include_usage": true });
    }

    request_body
}

fn apply_custom_model_reasoning_request(request_body: &mut Value, selected: &SelectedModel) -> bool {
    let Some(level) = selected_custom_reasoning_level(selected) else {
        return false;
    };
    if let Some(request) = level.request.as_ref() {
        merge_request_fragment(request_body, request);
    }
    true
}

fn selected_custom_reasoning_level(
    selected: &SelectedModel,
) -> Option<&crate::vs_registry::ModelReasoningLevel> {
    let reasoning = selected
        .model
        .as_ref()?
        .reasoning
        .as_ref()
        .filter(|config| !config.levels.is_empty())?;
    let requested = selected
        .reasoning_effort
        .as_deref()
        .or_else(|| Some(reasoning.default.as_str()));
    let requested = requested?.trim();
    if requested.is_empty() {
        return reasoning.levels.first();
    }
    reasoning
        .levels
        .iter()
        .find(|level| level.level.eq_ignore_ascii_case(requested))
        .or_else(|| reasoning.levels.first())
}

fn merge_request_fragment(target: &mut Value, fragment: &Value) {
    let (Some(target_object), Some(fragment_object)) = (target.as_object_mut(), fragment.as_object())
    else {
        return;
    };
    for (key, value) in fragment_object {
        merge_request_value(target_object.entry(key.clone()).or_insert(Value::Null), value);
    }
}

fn merge_request_value(target: &mut Value, value: &Value) {
    match (target.as_object_mut(), value.as_object()) {
        (Some(target_object), Some(value_object)) => {
            for (key, nested_value) in value_object {
                merge_request_value(
                    target_object.entry(key.clone()).or_insert(Value::Null),
                    nested_value,
                );
            }
        }
        _ => {
            *target = value.clone();
        }
    }
}

fn normalize_tool_choice_for_provider(selected: &SelectedModel, tool_choice: Value) -> Value {
    if provider_supports_forced_tool_choice(selected) || !is_forced_tool_choice(&tool_choice) {
        return tool_choice;
    }
    json!("auto")
}

fn is_forced_tool_choice(tool_choice: &Value) -> bool {
    match tool_choice {
        Value::String(value) => value != "auto" && value != "none",
        Value::Object(_) => true,
        _ => false,
    }
}

fn provider_supports_forced_tool_choice(selected: &SelectedModel) -> bool {
    let mut names = vec![selected.model_id.as_str()];
    if let Some(model) = selected.model.as_ref() {
        names.push(model.id.as_str());
        names.push(model.name.as_str());
    }
    !names
        .iter()
        .any(|name| name.to_ascii_lowercase().contains("kimi"))
}

fn resolved_model_reasoning_mode(model: &ProviderModel) -> &str {
    if model.id.eq_ignore_ascii_case("MiniMax-M3") || model.name.eq_ignore_ascii_case("MiniMax-M3")
    {
        "toggle"
    } else {
        model.reasoning_mode.trim()
    }
}

async fn send_chat_completion(
    selected: &SelectedModel,
    request_body: &Value,
    streaming_trace: Option<StreamingTraceSink<'_>>,
) -> Result<ChatCompletionResult, String> {
    let mut streaming_trace = streaming_trace;
    let mut last_error = String::new();
    for attempt in 0..=MODEL_RATE_LIMIT_RETRY_DELAYS_MS.len() {
        match send_chat_completion_once(selected, request_body, streaming_trace.as_mut()).await {
            Ok(completion) => return Ok(completion),
            Err(error) => {
                if !is_rate_limit_error(&error) || attempt >= MODEL_RATE_LIMIT_RETRY_DELAYS_MS.len()
                {
                    return Err(error);
                }
                last_error = error;
                sleep_rate_limit_retry(MODEL_RATE_LIMIT_RETRY_DELAYS_MS[attempt]).await;
            }
        }
    }
    Err(last_error)
}

async fn send_chat_completion_once(
    selected: &SelectedModel,
    request_body: &Value,
    streaming_trace: Option<&mut StreamingTraceSink<'_>>,
) -> Result<ChatCompletionResult, String> {
    let base_url = selected.provider.base_url.trim().trim_end_matches('/');
    if base_url.is_empty() {
        return Err(format!(
            "Base URL is empty for provider {}",
            selected.provider.name
        ));
    }
    if selected.credential_api_key().trim().is_empty() {
        return Err(format!(
            "API key is empty for provider {} credential {}",
            selected.provider.name,
            selected
                .credential
                .as_ref()
                .map(|credential| credential.name.as_str())
                .unwrap_or("default")
        ));
    }

    let url = format!("{base_url}/chat/completions");
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let auth = format!("Bearer {}", selected.credential_api_key().trim());
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&auth).map_err(|error| format!("Invalid API key header: {error}"))?,
    );
    if is_codebuddy_provider(selected) {
        add_codebuddy_vscode_headers(&mut headers);
    }

    let started = Instant::now();
    let mut response = model_http_client()?
        .post(&url)
        .headers(headers)
        .json(request_body)
        .send()
        .await
        .map_err(|error| format!("Model request failed: {error}"))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(format!(
            "Model request failed. status={}; body={}",
            status.as_u16(),
            body
        ));
    }

    let uses_streaming = request_body
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let response_body = if uses_streaming {
        read_streaming_chat_completion(&mut response, request_body, streaming_trace, started)
            .await?
    } else {
        let body = response.text().await.unwrap_or_default();
        serde_json::from_str::<Value>(&body)
            .map_err(|error| format!("Model response parse failed: {error}; body={}", body))?
    };
    let duration_ms = started.elapsed().as_millis() as u64;

    let token_usage = extract_token_usage_from_response(&response_body);
    Ok(ChatCompletionResult {
        duration_ms,
        request_body: request_body.clone(),
        response_body,
        token_usage,
    })
}

fn is_rate_limit_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("accountratelimitexceeded")
        || lower.contains("toomanyrequests")
        || lower.contains("too many requests")
        || lower.contains("requests are too frequent")
        || lower.contains("status=429")
        || lower.contains("\"type\":\"toomanyrequests\"")
}

async fn sleep_rate_limit_retry(delay_ms: u64) {
    let delay = Duration::from_millis(delay_ms);
    let _ = tauri::async_runtime::spawn_blocking(move || std::thread::sleep(delay)).await;
}

fn is_codebuddy_provider(selected: &SelectedModel) -> bool {
    selected.provider.id == "codebuddy" || selected.provider.provider_type == "codebuddy"
}

fn extract_token_usage_from_response(body: &Value) -> TokenUsage {
    let Some(usage) = body.get("usage").and_then(Value::as_object) else {
        return TokenUsage::default();
    };
    let prompt = usage.get("prompt_tokens").and_then(Value::as_u64);
    let completion = usage.get("completion_tokens").and_then(Value::as_u64);
    let total = usage
        .get("total_tokens")
        .and_then(Value::as_u64)
        .or_else(|| prompt.zip(completion).map(|(p, c)| p + c));
    let cached = usage
        .get("cached_tokens")
        .and_then(Value::as_u64)
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(Value::as_object)
                .and_then(|d| d.get("cached_tokens"))
                .and_then(Value::as_u64)
        });
    let uncached = prompt.zip(cached).map(|(p, c)| p.saturating_sub(c));
    TokenUsage {
        input_tokens: prompt,
        output_tokens: completion,
        total_tokens: total,
        input_cached_tokens: cached,
        input_uncached_tokens: uncached,
    }
}

fn add_codebuddy_vscode_headers(headers: &mut HeaderMap) {
    headers.insert(
        HeaderName::from_static("x-agent-intent"),
        HeaderValue::from_static("craft"),
    );
    headers.insert(
        HeaderName::from_static("x-ide-type"),
        HeaderValue::from_static("VSCode"),
    );
    headers.insert(
        HeaderName::from_static("x-ide-name"),
        HeaderValue::from_static("VSCode"),
    );
    headers.insert(
        HeaderName::from_static("x-ide-version"),
        HeaderValue::from_static("0.0.0"),
    );
    headers.insert(
        HeaderName::from_static("x-product"),
        HeaderValue::from_static("CodeBuddy"),
    );
    headers.insert(USER_AGENT, HeaderValue::from_static("CodeBuddyIDE/0.0.0"));
}

async fn read_streaming_chat_completion(
    response: &mut reqwest::Response,
    request_body: &Value,
    mut streaming_trace: Option<&mut StreamingTraceSink<'_>>,
    request_started: Instant,
) -> Result<Value, String> {
    let mut accumulator = StreamingChatCompletionAccumulator::default();
    let mut body = String::new();
    let mut line_buffer = String::new();
    let stream_event_id = Uuid::new_v4().to_string();
    let stream_started_at = Utc::now().to_rfc3339();
    let mut last_emit: Option<Instant> = None;
    let mut emitted_content_chars = 0usize;
    let mut emitted_reasoning_chars = 0usize;

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("Model stream read failed: {error}"))?
    {
        let text = String::from_utf8_lossy(&chunk);
        body.push_str(&text);
        line_buffer.push_str(&text);

        while let Some(newline_index) = line_buffer.find('\n') {
            let raw_line = line_buffer[..newline_index]
                .trim_end_matches('\r')
                .to_string();
            line_buffer.drain(..=newline_index);
            accumulator.accept_line(&raw_line)?;
        }

        maybe_emit_streaming_model_message(
            streaming_trace.as_deref_mut(),
            request_body,
            &accumulator,
            &stream_event_id,
            &stream_started_at,
            request_started,
            &mut emitted_content_chars,
            &mut emitted_reasoning_chars,
            &mut last_emit,
            false,
        );
    }

    if !line_buffer.trim().is_empty() {
        accumulator.accept_line(line_buffer.trim_end_matches('\r'))?;
    }

    maybe_emit_streaming_model_message(
        streaming_trace.as_deref_mut(),
        request_body,
        &accumulator,
        &stream_event_id,
        &stream_started_at,
        request_started,
        &mut emitted_content_chars,
        &mut emitted_reasoning_chars,
        &mut last_emit,
        true,
    );

    accumulator
        .into_response()
        .map_err(|error| format!("{error}; body={body}"))
}

fn maybe_emit_streaming_model_message(
    sink: Option<&mut StreamingTraceSink<'_>>,
    request_body: &Value,
    accumulator: &StreamingChatCompletionAccumulator,
    event_id: &str,
    stream_started_at: &str,
    request_started: Instant,
    emitted_content_chars: &mut usize,
    emitted_reasoning_chars: &mut usize,
    last_emit: &mut Option<Instant>,
    force: bool,
) {
    let Some(sink) = sink else {
        return;
    };
    let content = accumulator.content();
    let reasoning = accumulator.reasoning_content();
    if content.is_empty() && reasoning.is_empty() {
        return;
    }

    let content_chars = content.chars().count();
    let reasoning_chars = reasoning.chars().count();
    let has_new_text =
        content_chars != *emitted_content_chars || reasoning_chars != *emitted_reasoning_chars;
    if !has_new_text && !(force && last_emit.is_some()) {
        return;
    }
    if !force {
        if let Some(last_emit) = last_emit {
            if last_emit.elapsed() < Duration::from_millis(STREAMING_TRACE_INTERVAL_MS) {
                return;
            }
        }
    }

    let duration_ms = request_started.elapsed().as_millis() as u64;
    let event = ToolTraceEvent {
        id: event_id.to_string(),
        task_id: sink.task_id.to_string(),
        parent_task_id: None,
        agent_name: None,
        task_name: None,
        read_only: None,
        subagent_depth: None,
        step_index: sink.step_index,
        event_type: TraceEventType::ModelMessage,
        tool_name: None,
        title: "streaming_model_message".to_string(),
        input: Some(json!({
            "model": request_body.get("model").cloned().unwrap_or(Value::Null),
            "stream": true,
        })),
        output: Some(json!({
            "reasoning_content": reasoning,
            "content": content,
            "model": request_body.get("model").cloned().unwrap_or(Value::Null),
        })),
        output_summary: Some(format!("Streaming response ({content_chars} chars)")),
        started_at: stream_started_at.to_string(),
        ended_at: if force {
            Some(Utc::now().to_rfc3339())
        } else {
            None
        },
        duration_ms: Some(duration_ms),
        status: if force {
            TraceStatus::Success
        } else {
            TraceStatus::Running
        },
    };
    (sink.on_trace)(&event);
    *emitted_content_chars = content_chars;
    *emitted_reasoning_chars = reasoning_chars;
    *last_emit = Some(Instant::now());
}

fn model_http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(MODEL_REQUEST_TIMEOUT_SECONDS))
        .build()
        .map_err(|error| format!("Model client build failed: {error}"))
}

async fn call_openai_compatible_for_mode(
    project: &ProjectSession,
    selected: &SelectedModel,
    conversation_messages: &[ChatMessage],
    cli_mode: bool,
    intent_mode: Option<AgentIntentMode>,
) -> Result<ProviderCompletion, String> {
    let messages = build_openai_messages_for_mode(
        project,
        conversation_messages,
        cli_mode,
        selected,
        false,
        intent_mode,
    );
    let request_body = build_chat_completion_request(selected, messages, None);
    let completion = send_chat_completion(selected, &request_body, None).await?;
    let response_body = completion.response_body.clone();
    let parsed = serde_json::from_value::<OpenAiChatResponse>(response_body.clone())
        .map_err(|error| format!("Model response parse failed: {error}; body={response_body}"))?;
    let message = parsed
        .choices
        .first()
        .and_then(|choice| choice.message.content.as_deref())
        .unwrap_or("")
        .trim()
        .to_string();
    if message.is_empty() {
        return Err("Model response was empty.".to_string());
    }

    let token_usage = parsed
        .usage
        .map(|usage| {
            let cached_tokens = usage
                .prompt_tokens_details
                .as_ref()
                .and_then(|details| details.cached_tokens);
            TokenUsage {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
                total_tokens: usage.total_tokens,
                input_cached_tokens: cached_tokens,
                input_uncached_tokens: usage.prompt_tokens.zip(cached_tokens).map(
                    |(input_tokens, cached_tokens)| input_tokens.saturating_sub(cached_tokens),
                ),
            }
        })
        .unwrap_or_default();

    Ok(ProviderCompletion {
        message,
        duration_ms: completion.duration_ms,
        token_usage,
        request_body: completion.request_body,
        response_body,
    })
}

async fn call_claude_for_mode(
    project: &ProjectSession,
    selected: &SelectedModel,
    conversation_messages: &[ChatMessage],
    cli_mode: bool,
    intent_mode: Option<AgentIntentMode>,
) -> Result<ProviderCompletion, String> {
    let mut layers = prompt_layers(project, cli_mode);
    if let Some(intent_mode) = intent_mode {
        layers.developer.push_str("\n");
        layers.developer.push_str(intent_mode_guidance(intent_mode));
    }
    let base_url = selected
        .provider
        .base_url
        .trim()
        .trim_end_matches('/')
        .to_string();
    let base_url = if base_url.is_empty() {
        "https://api.anthropic.com/v1".to_string()
    } else {
        base_url
    };
    if selected.credential_api_key().trim().is_empty() {
        return Err(format!(
            "API key is empty for provider {} credential {}",
            selected.provider.name,
            selected
                .credential
                .as_ref()
                .map(|credential| credential.name.as_str())
                .unwrap_or("default")
        ));
    }

    let url = format!("{base_url}/messages");
    let request_body = json!({
        "model": selected.model_id,
        "max_tokens": 4096,
        "system": merged_system_prompt(&layers),
        "messages": build_conversation_with_user_context(&layers, conversation_messages),
    });

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        HeaderName::from_static("x-api-key"),
        HeaderValue::from_str(selected.credential_api_key().trim())
            .map_err(|error| format!("Invalid Claude API key header: {error}"))?,
    );
    headers.insert(
        HeaderName::from_static("anthropic-version"),
        HeaderValue::from_static("2023-06-01"),
    );

    let started = Instant::now();
    let response = model_http_client()?
        .post(&url)
        .headers(headers)
        .json(&request_body)
        .send()
        .await
        .map_err(|error| format!("Claude request failed: {error}"))?;
    let duration_ms = started.elapsed().as_millis() as u64;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "Claude request failed. status={}; body={}",
            status.as_u16(),
            body
        ));
    }

    let response_body = serde_json::from_str::<Value>(&body)
        .map_err(|error| format!("Claude response parse failed: {error}; body={}", body))?;
    let parsed = serde_json::from_value::<ClaudeMessagesResponse>(response_body.clone())
        .map_err(|error| format!("Claude response parse failed: {error}; body={body}"))?;
    let message = parsed
        .content
        .into_iter()
        .filter(|block| block.block_type == "text")
        .filter_map(|block| block.text)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    if message.is_empty() {
        return Err("Claude response was empty.".to_string());
    }

    let token_usage = parsed
        .usage
        .map(|usage| {
            let input_uncached_tokens =
                sum_optional_tokens(usage.input_tokens, usage.cache_creation_input_tokens);
            let input_tokens =
                sum_optional_tokens(input_uncached_tokens, usage.cache_read_input_tokens);
            TokenUsage {
                input_tokens,
                output_tokens: usage.output_tokens,
                total_tokens: input_tokens
                    .zip(usage.output_tokens)
                    .map(|(input_tokens, output_tokens)| input_tokens + output_tokens),
                input_cached_tokens: usage.cache_read_input_tokens,
                input_uncached_tokens,
            }
        })
        .unwrap_or_default();

    Ok(ProviderCompletion {
        message,
        duration_ms,
        token_usage,
        request_body,
        response_body,
    })
}

async fn call_ollama_for_mode(
    project: &ProjectSession,
    selected: &SelectedModel,
    conversation_messages: &[ChatMessage],
    cli_mode: bool,
    intent_mode: Option<AgentIntentMode>,
) -> Result<ProviderCompletion, String> {
    let base_url = selected.provider.base_url.trim().trim_end_matches('/');
    if base_url.is_empty() {
        return Err("Ollama Base URL is empty.".to_string());
    }

    let url = format!("{base_url}/api/chat");
    let request_body = json!({
        "model": selected.model_id,
        "messages": intent_mode
            .map(|mode| build_messages_for_mode(project, conversation_messages, cli_mode, mode))
            .unwrap_or_else(|| build_messages(project, conversation_messages, cli_mode)),
        "stream": false,
    });

    let started = Instant::now();
    let response = model_http_client()?
        .post(&url)
        .json(&request_body)
        .send()
        .await
        .map_err(|error| format!("Ollama request failed: {error}"))?;
    let duration_ms = started.elapsed().as_millis() as u64;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "Ollama request failed. status={}; body={}",
            status.as_u16(),
            body
        ));
    }

    let response_body = serde_json::from_str::<Value>(&body)
        .map_err(|error| format!("Ollama response parse failed: {error}; body={}", body))?;
    let parsed = serde_json::from_value::<OllamaChatResponse>(response_body.clone())
        .map_err(|error| format!("Ollama response parse failed: {error}; body={body}"))?;
    let message = parsed
        .message
        .map(|message| message.content)
        .or(parsed.response)
        .unwrap_or_default()
        .trim()
        .to_string();
    if message.is_empty() {
        return Err("Ollama response was empty.".to_string());
    }

    let token_usage = TokenUsage {
        input_tokens: parsed.prompt_eval_count,
        output_tokens: parsed.eval_count,
        total_tokens: parsed
            .prompt_eval_count
            .zip(parsed.eval_count)
            .map(|(input_tokens, output_tokens)| input_tokens + output_tokens),
        input_cached_tokens: None,
        input_uncached_tokens: None,
    };

    Ok(ProviderCompletion {
        message,
        duration_ms,
        token_usage,
        request_body,
        response_body,
    })
}

fn normalize_conversation_messages(
    messages: Option<&[AgentConversationMessage]>,
    user_prompt: &str,
) -> Vec<ChatMessage> {
    let normalized = messages
        .unwrap_or(&[])
        .iter()
        .filter_map(|message| {
            if is_synthetic_continuation_reminder(&message.content) {
                return None;
            }
            let role = match message.role.as_str() {
                "assistant" => "assistant",
                "user" => "user",
                "system" if is_context_compaction_message(&message.content) => "user",
                _ => return None,
            };
            let content = message.content.trim().to_string();
            if content.is_empty() {
                if message.attachments.is_empty() {
                    return None;
                }
            }
            Some(ChatMessage {
                role: role.to_string(),
                content,
                attachments: message.attachments.clone(),
            })
        })
        .collect::<Vec<_>>();

    if normalized.is_empty() {
        let content = if is_synthetic_continuation_reminder(user_prompt) {
            String::new()
        } else {
            user_prompt.trim().to_string()
        };
        return vec![ChatMessage {
            role: "user".to_string(),
            content,
            attachments: Vec::new(),
        }];
    }

    normalized
}

fn should_require_local_read_tool_call(messages: &[ChatMessage], user_prompt: &str) -> bool {
    let current = normalize_intent_text(user_prompt);
    if current.is_empty() {
        return false;
    }

    let asks_to_read = contains_any(
        &current,
        &[
            "读",
            "读取",
            "去读",
            "看日志",
            "读日志",
            "读文件",
            "读取文件",
            "read ",
            "read_file",
            "read the",
            "open file",
            "check log",
            "read log",
        ],
    );
    let local_file_context = contains_any(
        &current,
        &[
            "日志",
            ".log",
            ".txt",
            ".md",
            ".cpp",
            ".h",
            ".hpp",
            ".rs",
            ".json",
            ".xml",
            ".toml",
            ".yaml",
            ".yml",
            ".bat",
            ".ps1",
            "文件",
            "路径",
            "temp",
            "%temp%",
            "appdata",
            "c:\\",
            "d:\\",
            "读取不到",
            "读不到",
            "权限读",
            "有权限读",
        ],
    );
    if asks_to_read && local_file_context {
        return true;
    }

    if !contains_any(&current, &["读", "读取", "权限读", "有权限读", "read"]) {
        return false;
    }

    let recent_context = messages
        .iter()
        .rev()
        .take(8)
        .map(|message| normalize_intent_text(&message.content))
        .collect::<Vec<_>>()
        .join("\n");
    contains_any(
        &recent_context,
        &[
            "日志",
            ".log",
            "wz_render_frame_trace",
            "wz_model_render_trace",
            "%temp%",
            "appdata\\local\\temp",
            "c:\\users",
            "路径",
            "读取不到",
            "读不到",
        ],
    )
}

fn classify_intent_mode(prompt: &str) -> IntentClassification {
    let normalized = normalize_intent_text(prompt);
    if normalized.is_empty() {
        return IntentClassification {
            mode: AgentIntentMode::Default,
            reason: "empty_prompt",
        };
    }

    let read_only_requested = contains_any(
        &normalized,
        &[
            "先回答",
            "只回答",
            "先别做",
            "先不要做",
            "不要改",
            "别改",
            "不用改",
            "不要写",
            "别写",
            "只读",
            "只看",
            "don't edit",
            "do not edit",
            "no changes",
            "read only",
            "answer first",
        ],
    );
    if read_only_requested || looks_like_question(&normalized) {
        return classify_read_only_intent(&normalized, "question_or_read_only");
    }

    if contains_any(
        &normalized,
        &[
            "do it",
            "start doing",
            "开始做",
            "动手",
            "实现",
            "改成",
            "修改",
            "更改",
            "修复",
            "修一下",
            "帮我改",
            "给我改",
            "加一个",
            "加上",
            "新增",
            "删除",
            "替换",
            "重构",
            "优化",
            "接入",
            "写一个",
            "补上",
            "做一下",
            "做下",
            "做成",
            "弄一下",
            "搞一下",
            "implement",
            "fix ",
            "fix the",
            "add ",
            "remove ",
            "delete ",
            "update ",
            "change ",
            "refactor",
            "optimize",
            "wire ",
        ],
    ) {
        return IntentClassification {
            mode: AgentIntentMode::Implement,
            reason: "explicit_write_intent",
        };
    }

    classify_read_only_intent(&normalized, "default_read_only")
}

fn classify_read_only_intent(
    normalized: &str,
    fallback_reason: &'static str,
) -> IntentClassification {
    if contains_capability_question(normalized) {
        return IntentClassification {
            mode: AgentIntentMode::Research,
            reason: "capability_question",
        };
    }
    if contains_review_intent(normalized) {
        return IntentClassification {
            mode: AgentIntentMode::Review,
            reason: "review_keywords",
        };
    }
    if contains_verify_intent(normalized) {
        return IntentClassification {
            mode: AgentIntentMode::Verify,
            reason: "verify_keywords",
        };
    }
    if contains_debug_intent(normalized) {
        return IntentClassification {
            mode: AgentIntentMode::Debug,
            reason: "debug_keywords",
        };
    }
    if contains_research_intent(normalized) || looks_like_question(normalized) {
        return IntentClassification {
            mode: AgentIntentMode::Research,
            reason: "research_or_question_keywords",
        };
    }
    IntentClassification {
        mode: AgentIntentMode::Default,
        reason: fallback_reason,
    }
}

fn normalize_intent_text(prompt: &str) -> String {
    prompt
        .trim()
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn looks_like_question(normalized: &str) -> bool {
    normalized.ends_with('?')
        || normalized.ends_with('？')
        || contains_any(
            normalized,
            &[
                "?",
                "？",
                "吗",
                "么",
                "是不是",
                "是否",
                "有没有",
                "哪里",
                "在哪",
                "哪个",
                "什么",
                "怎么",
                "如何",
                "为什么",
                "为何",
                "对吗",
                "可以吗",
                "能不能",
                "能否",
            ],
        )
        || starts_with_any(
            normalized,
            &[
                "what ", "where ", "how ", "why ", "which ", "who ", "can ", "could ", "should ",
                "is ", "are ", "does ",
            ],
        )
        || (normalized.starts_with("do ") && !normalized.starts_with("do it"))
}

fn contains_review_intent(normalized: &str) -> bool {
    contains_any(
        normalized,
        &[
            "审查",
            "代码审查",
            "找问题",
            "风险",
            "安全问题",
            "测试缺口",
            "code review",
            "review code",
            "review this",
            "review the",
            "find issues",
            "security issue",
            "test gap",
            "risk",
        ],
    )
}

fn contains_verify_intent(normalized: &str) -> bool {
    contains_any(
        normalized,
        &[
            "确认",
            "验证",
            "是否已经",
            "是不是已经",
            "有没有生效",
            "检查结果",
            "生效了吗",
            "prove",
            "confirm",
            "verify",
            "confirmed",
            "is it fixed",
            "does it pass",
            "already fixed",
        ],
    )
}

fn contains_debug_intent(normalized: &str) -> bool {
    contains_any(
        normalized,
        &[
            "为什么",
            "报错",
            "崩溃",
            "不生效",
            "失败",
            "异常",
            "错误",
            "没反应",
            "定位",
            "原因",
            "bug",
            "crash",
            "error",
            "fails",
            "failure",
            "not working",
            "doesn't work",
            "wrong behavior",
            "why is",
        ],
    )
}

fn contains_research_intent(normalized: &str) -> bool {
    contains_any(
        normalized,
        &[
            "有没有",
            "在哪",
            "哪里",
            "哪个文件",
            "谁调用",
            "怎么实现",
            "如何实现",
            "什么",
            "看看",
            "查一下",
            "分析",
            "解释",
            "看下",
            "trace",
            "where",
            "how",
            "what",
            "who calls",
            "find where",
            "look up",
            "inspect",
            "explain",
            "analyze",
        ],
    )
}

fn contains_capability_question(normalized: &str) -> bool {
    contains_any(
        normalized,
        &[
            "功能吗",
            "有这个功能",
            "有代码审查",
            "能做什么",
            "可以做什么",
            "what can",
            "can it",
            "can this",
            "do i have",
        ],
    )
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn starts_with_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.starts_with(needle))
}

fn is_synthetic_continuation_reminder(content: &str) -> bool {
    let trimmed = content.trim();
    trimmed.starts_with("[System reminder:")
        && trimmed.contains("Output token limit hit")
        && trimmed.contains("Resume directly")
}

fn sum_optional_tokens(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left + right),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn build_messages(
    project: &ProjectSession,
    conversation_messages: &[ChatMessage],
    cli_mode: bool,
) -> Vec<ChatMessage> {
    build_layered_chat_messages_for_mode(
        project,
        conversation_messages,
        cli_mode,
        false,
        false,
        None,
    )
}

fn build_messages_for_mode(
    project: &ProjectSession,
    conversation_messages: &[ChatMessage],
    cli_mode: bool,
    intent_mode: AgentIntentMode,
) -> Vec<ChatMessage> {
    build_layered_chat_messages_for_mode(
        project,
        conversation_messages,
        cli_mode,
        false,
        false,
        Some(intent_mode),
    )
}

#[cfg(test)]
fn build_openai_messages(
    project: &ProjectSession,
    conversation_messages: &[ChatMessage],
    cli_mode: bool,
    selected: &SelectedModel,
    auto_readonly_subagents: bool,
) -> Vec<Value> {
    build_openai_messages_for_mode(
        project,
        conversation_messages,
        cli_mode,
        selected,
        auto_readonly_subagents,
        None,
    )
}

fn build_openai_messages_for_mode(
    project: &ProjectSession,
    conversation_messages: &[ChatMessage],
    cli_mode: bool,
    selected: &SelectedModel,
    auto_readonly_subagents: bool,
    intent_mode: Option<AgentIntentMode>,
) -> Vec<Value> {
    build_layered_chat_messages_for_mode(
        project,
        conversation_messages,
        cli_mode,
        provider_supports_developer_role(selected),
        auto_readonly_subagents,
        intent_mode,
    )
    .into_iter()
    .map(openai_chat_message_value)
    .collect()
}

fn build_layered_chat_messages_for_mode(
    project: &ProjectSession,
    conversation_messages: &[ChatMessage],
    cli_mode: bool,
    use_developer_role: bool,
    auto_readonly_subagents: bool,
    intent_mode: Option<AgentIntentMode>,
) -> Vec<ChatMessage> {
    let mut layers = prompt_layers(project, cli_mode);
    if let Some(intent_mode) = intent_mode {
        layers.developer.push_str("\n");
        layers.developer.push_str(intent_mode_guidance(intent_mode));
    }
    if auto_readonly_subagents {
        layers.developer.push_str("\n");
        layers.developer.push_str(auto_readonly_subagent_guidance());
    }
    let mut messages = Vec::new();
    if use_developer_role {
        messages.push(chat_message("system", layers.system));
        messages.push(chat_message("developer", layers.developer));
    } else {
        messages.push(chat_message("system", merged_system_prompt(&layers)));
    }
    if let Some(user_context) = layers.user_context {
        messages.push(chat_message("user", user_context));
    }
    messages.extend(conversation_messages.iter().cloned());
    messages
}

fn build_conversation_with_user_context(
    layers: &PromptLayers,
    conversation_messages: &[ChatMessage],
) -> Vec<ChatMessage> {
    let mut messages = Vec::new();
    if let Some(user_context) = layers.user_context.clone() {
        messages.push(chat_message("user", user_context));
    }
    messages.extend(conversation_messages.iter().cloned());
    messages
}

fn chat_message(role: &str, content: String) -> ChatMessage {
    ChatMessage {
        role: role.to_string(),
        content,
        attachments: Vec::new(),
    }
}

fn openai_chat_message_value(message: ChatMessage) -> Value {
    if message.attachments.is_empty() {
        return json!({
            "role": message.role,
            "content": message.content,
        });
    }

    let mut content_parts = Vec::new();
    if !message.content.trim().is_empty() {
        content_parts.push(json!({
            "type": "text",
            "text": message.content,
        }));
    }

    for attachment in message.attachments {
        if attachment.kind == "image"
            && attachment.mime_type.starts_with("image/")
            && attachment.data_url.starts_with("data:image/")
        {
            content_parts.push(json!({
                "type": "image_url",
                "image_url": {
                    "url": attachment.data_url,
                },
            }));
        }
    }

    json!({
        "role": message.role,
        "content": content_parts,
    })
}

fn build_codex_cli_prompt(
    project: &ProjectSession,
    conversation_messages: &[ChatMessage],
) -> String {
    let layers = prompt_layers(project, true);
    let mut prompt = String::new();
    prompt.push_str("<system>\n");
    prompt.push_str(&layers.system);
    prompt.push_str("\n</system>\n\n<developer>\n");
    prompt.push_str(&layers.developer);
    prompt.push_str("\n</developer>\n\n");
    if let Some(user_context) = layers.user_context {
        prompt.push_str("<user_context>\n");
        prompt.push_str(&user_context);
        prompt.push_str("\n</user_context>\n\n");
    }
    prompt.push_str("Conversation:\n");

    for message in conversation_messages {
        let role = match message.role.as_str() {
            "assistant" => "Assistant",
            _ => "User",
        };
        prompt.push_str(role);
        prompt.push_str(": ");
        prompt.push_str(message.content.trim());
        for attachment in &message.attachments {
            prompt.push_str("\n[Attachment omitted: ");
            prompt.push_str(attachment.name.trim());
            prompt.push_str(" (");
            prompt.push_str(attachment.mime_type.trim());
            prompt.push_str(")]");
        }
        prompt.push_str("\n\n");
    }

    prompt
}

fn prompt_layers(project: &ProjectSession, cli_mode: bool) -> PromptLayers {
    PromptLayers {
        system: core_system_prompt(project, cli_mode),
        developer: developer_prompt(project, cli_mode),
        user_context: user_context_prompt(project),
    }
}

fn core_system_prompt(project: &ProjectSession, cli_mode: bool) -> String {
    if cli_mode {
        return format!(
            "You are CodeForge CLI, a command-line coding assistant for the active workspace. Workspace name: \"{}\". Workspace path: {}. Do not identify yourself as a desktop app in CLI mode. Do not infer a specific project name from unrelated prior context; use only the current workspace and the user's request. Do not execute arbitrary shell commands.",
            project.name,
            project.repo_root
        );
    }

    format!(
        "You are CodeForge Desktop, a coding assistant for the project \"{}\". Repo root: {}. Internal SnowAgent class or path names may remain unchanged. Do not execute arbitrary shell commands.",
        project.name,
        project.repo_root
    )
}

fn developer_prompt(project: &ProjectSession, cli_mode: bool) -> String {
    let mut prompt = String::new();
    if cli_mode {
        prompt.push_str("Use a plain terminal style: no emoji, no marketing copy, and no generic capability list. Do not advertise tools or demo capabilities unless the user asks about them. Never mention calculator or arithmetic demo tools unless directly relevant to the user's request. For a simple greeting, reply with one short sentence asking what task to work on; do not include examples or bullet lists.\n");
    }
    prompt.push_str("Prefer Visual Studio context tools when the bridge is connected, and use repository tools when VS context is unavailable or insufficient. Use document/read_docx for .docx files and presentation/read_pptx for .pptx files; do not use text read_file for Office packages.\n");
    prompt.push_str("Read-only file tools can inspect local absolute paths outside the workspace. When the user asks to read, inspect, search, or verify local files or logs, call list_dir/read_file/search_file/search_content (or their workspace/ aliases) before answering. Do not claim a read/search tool failed unless a tool_result in the current turn shows that failure. A successful read_file/search_content result is fresh current-filesystem evidence for that turn; do not dismiss it as a stale snapshot, do not ask the user to open VS Code or run git just to confirm the same file state, and do not treat \"verify against current source\" as a reason to avoid read/search tools.\n");
    prompt.push_str("Treat user statements about the code as hypotheses until they are verified against workspace code, tool output, logs, or diagnostics. For code-specific answers, gather enough concrete evidence before concluding. If you did not inspect fresh code in the current turn, say whether the answer is based on previous context or inference. Distinguish verified facts, reused prior evidence, and inference when the distinction matters.\n");
    prompt.push_str("Do not claim rg or text search is precise semantic analysis. Prefer exact definitions, call sites, implementations, and diagnostics over name-only search results. Keep edits surgical and cite concrete code locations when relevant.\n");
    prompt.push_str("If an AI context index is provided from doc/ai-context/README.md, treat it as a navigation map only. Do not read every linked doc by default. Read only the context docs that are directly relevant to the user's task, then verify any code-specific conclusion against current source code before answering or editing.\n");
    prompt.push_str("Answer concisely. ");
    prompt.push_str(code_navigation_response_guidance());

    if let Some(file) =
        read_first_instruction_file(&project.repo_root, &CODEFORGE_DEVELOPER_INSTRUCTION_FILES)
    {
        prompt.push_str("\n\nAdditional CodeForge developer instructions from ");
        prompt.push_str(&display_instruction_path(project, &file.path));
        prompt.push_str(":\n<codeforge_developer_instructions>\n");
        prompt.push_str(&file.content);
        prompt.push_str("\n</codeforge_developer_instructions>");
    }

    prompt
}

fn local_read_tool_required_message() -> &'static str {
    "This turn is asking you to read or verify a local file/log. Before answering, call at least one read-only file tool: list_dir, read_file, search_file, search_content, workspace/list_dir, workspace/read_file, workspace/search_file, or workspace/search. These tools accept both workspace-relative paths and absolute local paths such as C:\\Users\\name\\AppData\\Local\\Temp. Successful read/search results are current filesystem evidence for this turn, not stale snapshots. Do not answer that you cannot read the path until an actual tool_result in this turn proves the path is unavailable."
}

fn required_tool_call_retry_message() -> &'static str {
    "Your previous response did not include the required tool call. Call the most relevant available tool now with valid JSON arguments. Do not answer from assumptions, and do not claim that a tool failed unless a current tool_result shows the failure."
}

fn user_context_prompt(project: &ProjectSession) -> Option<String> {
    let mut sections = Vec::new();

    if let Some(file) =
        read_first_instruction_file(&project.repo_root, &AGENTS_USER_INSTRUCTION_FILES)
    {
        let mut prompt = String::new();
        prompt.push_str("Project user instructions from ");
        prompt.push_str(&display_instruction_path(project, &file.path));
        prompt.push_str(". These are workspace guidance supplied as user-level context. Follow them when applicable unless they conflict with system/developer instructions or the current user request.\n<project_user_instructions>\n");
        prompt.push_str(&file.content);
        prompt.push_str("\n</project_user_instructions>");
        sections.push(prompt);
    }

    if let Some(file) = read_instruction_file(&project.repo_root, AI_CONTEXT_INDEX_FILE) {
        let mut prompt = String::new();
        prompt.push_str("AI context index from ");
        prompt.push_str(&display_instruction_path(project, &file.path));
        prompt.push_str(". This file is an index only. Use it to decide which context doc to read when helpful; do not treat it as source of truth and do not load all linked docs by default.\n<ai_context_index>\n");
        prompt.push_str(&file.content);
        prompt.push_str("\n</ai_context_index>");
        sections.push(prompt);
    }

    if sections.is_empty() {
        None
    } else {
        Some(sections.join("\n\n"))
    }
}

fn read_instruction_file(repo_root: &str, candidate: &str) -> Option<InstructionFile> {
    let path = Path::new(repo_root).join(candidate);
    if !path.is_file() {
        return None;
    }
    let content = std::fs::read_to_string(&path).ok()?.trim().to_string();
    if content.is_empty() {
        return None;
    }
    Some(InstructionFile { path, content })
}

fn is_init_command(prompt: &str) -> bool {
    prompt.trim().eq_ignore_ascii_case("/init")
}

fn ai_context_init_prompt(project: &ProjectSession) -> String {
    format!(
        r#"Run CodeForge /init for this workspace.

Workspace name: {workspace_name}
Workspace root: {workspace_root}

Goal:
Create or update a retrieval-style AI context library under `doc/ai-context/`. This library is for future coding agents to navigate the project faster; it must not replace reading current code.

Required behavior:
1. Use workspace tools to inspect the project before writing. Start with `AGENTS.md` if present, existing `doc/ai-context/README.md` if present, the top-level tree, build/config files, and key source directories.
2. Write only documentation files under `doc/ai-context/`.
3. Create or update `doc/ai-context/README.md` as the only default index. It must explicitly say:
   - it is an index only,
   - linked docs are not source of truth,
   - agents should read only relevant docs,
   - code-specific claims must be verified against current source.
4. Create or update focused markdown files that are actually justified by the inspected project. Include at least:
   - `doc/ai-context/architecture.md`
   - `doc/ai-context/modules.md`
   - `doc/ai-context/build-and-run.md`
   - `doc/ai-context/code-navigation.md`
   Add up to eight more focused docs only when the codebase clearly has matching areas.
5. Each doc must include:
   - Purpose
   - When to read
   - Source scope with concrete paths
   - Key entry points or relationships
   - Verification notes, including that current code wins over this doc
   - Last verified date: {date}
6. Keep docs concise and navigational. Prefer file paths, module ownership, call-flow hints, and search keywords over broad prose.
7. Do not invent APIs, architecture, build commands, or module relationships. If unclear, write "unknown; verify in code" and cite the files that were inspected.
8. Final answer should summarize which files were created or updated and what remains uncertain.

Use the available workspace read/search/write tools. Do not use shell commands."#,
        workspace_name = project.name,
        workspace_root = project.repo_root,
        date = Utc::now().date_naive()
    )
}

fn read_first_instruction_file(repo_root: &str, candidates: &[&str]) -> Option<InstructionFile> {
    let root = Path::new(repo_root);
    for candidate in candidates {
        let path = root.join(candidate);
        if !path.is_file() {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let content = content.trim().to_string();
        if content.is_empty() {
            continue;
        }
        return Some(InstructionFile { path, content });
    }
    None
}

fn display_instruction_path(project: &ProjectSession, path: &Path) -> String {
    let root = Path::new(&project.repo_root);
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn merged_system_prompt(layers: &PromptLayers) -> String {
    let mut prompt = layers.system.clone();
    prompt.push_str("\n\n<developer>\n");
    prompt.push_str(&layers.developer);
    prompt.push_str("\n</developer>");
    prompt
}

fn provider_supports_developer_role(selected: &SelectedModel) -> bool {
    if let Some(supports_developer_role) = selected
        .model
        .as_ref()
        .and_then(|model| model.supports_developer_role)
    {
        return supports_developer_role;
    }

    let provider_type = selected.provider.provider_type.trim().to_ascii_lowercase();
    let provider_id = selected.provider.id.trim().to_ascii_lowercase();
    let base_url = selected.provider.base_url.trim().to_ascii_lowercase();
    provider_type == "openai"
        || provider_id == "openai"
        || (selected.provider.requires_openai_auth && base_url.contains("api.openai.com"))
        || base_url.starts_with("https://api.openai.com/")
}

fn code_navigation_response_guidance() -> &'static str {
    concat!(
        "For code-location answers, do not paste C/C++ source code blocks unless the user explicitly asks for source text. ",
        "For code-flow, inheritance, dispatch, or multi-category analysis, prefer grouped bullet/list sections over wide Markdown tables. ",
        "Use Markdown tables only for small comparisons with at most 3 columns and short cell content. ",
        "Every user-facing code location must be a standalone Markdown link with a compact visible label and a unique local-file target, for example [Foo.cpp:123](src/module/Foo.cpp:123). ",
        "For files under the repo root, convert absolute tool-returned paths to workspace-relative link targets; use absolute targets only for files outside the repo. ",
        "Link targets are local file paths, not URLs: keep literal spaces in directory names, and do not URL-encode or percent-encode them. ",
        "Use one concrete start line in each link target and label; do not write line ranges such as `:7-49` as links. ",
        "If a span matters, link the start line and describe the span in text, for example [Foo.cpp:7](src/module/Foo.cpp:7) covers lines 7-49. ",
        "Do not use shorthand such as \"same file\", \"same as above\", or \"同上 :11\" for locations; repeat a complete link instead. ",
        "Do not rely on bare filename links like `Foo.cpp:123` unless the target also appears in the Markdown link target. ",
        "Use single-backtick inline code only for short identifiers or commands, not as the primary way to cite code locations. ",
        "The UI displays the short label while opening the unique target in Visual Studio."
    )
}

fn intent_mode_guidance(intent_mode: AgentIntentMode) -> &'static str {
    match intent_mode {
        AgentIntentMode::Research => {
            concat!(
                "Internal mode: research. The user is asking to understand the current code or behavior, not to change it. Work read-only. ",
                "For non-trivial research, call progress/update_steps early with task-specific investigation steps, then update those steps as evidence is gathered; do not use a fixed template. ",
                "Inspect fresh code, definitions, dispatch or call sites, implementations, configuration, docs, and VS context before concluding. ",
                "For code-flow or architecture questions, name-search results are only starting points: inspect the entry point, core data structures, implementations that create or mutate the behavior, and each user-named category before final answer. ",
                "Ambiguous domain wording is not by itself a blocker for read-only research; state the working interpretation, continue with the most likely verifiable path, and separate verified facts, inferences, and unknowns. ",
                "Ask a concise clarification question only when no useful evidence can be gathered or the ambiguity would make a requested edit or unsafe action guesswork. ",
                "After successful read/search tools, do not finish with only a clarification question; provide the best verified partial answer and the exact remaining question."
            )
        }
        AgentIntentMode::Debug => {
            "Internal mode: debug. The user is reporting or investigating wrong behavior. For non-trivial debugging, call progress/update_steps early with task-specific diagnosis steps, then update those steps as evidence is gathered; do not use a fixed template. Start read-only: define expected versus actual behavior, gather evidence from code, diagnostics, logs available through tools, and VS context, then identify the most likely cause. Do not edit files unless the user explicitly asked for a fix and the requested change is clear."
        }
        AgentIntentMode::Implement => {
            "Internal mode: implement. The user has explicitly asked for a code or documentation change. For non-trivial implementation, call progress/update_steps before editing with task-specific implementation and validation steps, then update progress after meaningful milestones; do not use a fixed template. Before editing, make sure the goal, scope, and expected result are clear enough to execute safely; if not, ask a concise clarification question. Make the smallest change that satisfies the request, avoid unrelated cleanup, and verify honestly with the best available check."
        }
        AgentIntentMode::Review => {
            "Internal mode: review. Use a code-review stance. Work read-only. For non-trivial review, call progress/update_steps early with task-specific review areas, then update those steps as findings are checked; do not use a fixed template. Inspect the relevant code or active VS context before judging. Prioritize correctness bugs, regressions, safety issues, architecture mismatches, and missing tests. Report findings first, ordered by severity, with concrete evidence and file locations. If no actionable issues are found, say so and note residual risk or unreviewed scope."
        }
        AgentIntentMode::Verify => {
            "Internal mode: verify. The user is asking whether a fact, fix, behavior, or result is confirmed. For non-trivial verification, call progress/update_steps early with task-specific checks, then update those steps as checks complete; do not use a fixed template. Work read-only. Check the strongest available evidence before answering. Clearly separate verified facts from inference and state any blocker that prevents confirmation. Do not modify files while verifying."
        }
        AgentIntentMode::Default => {
            "Internal mode: default. Answer directly and conservatively. If the user's intent is ambiguous or only asks for confirmation or discussion, do not edit files. Ask a concise clarification question before taking any action that would require guessing."
        }
    }
}

fn auto_readonly_subagent_guidance() -> &'static str {
    "Auto read-only subagent policy: for broad, multi-file, review, architecture, debugging, performance, security, or test-gap tasks, proactively spawn bounded read-only subagents in the background instead of waiting for the user to ask. Keep this selective: do not spawn subagents for simple single-file edits, one-line answers, direct UI polish, or tasks where delegation would add noise. Use at most three subagents by default, set readOnly=true, give each child a narrow scope and required evidence format, wait for their summaries before the final answer, and keep final integration and correctness judgment in the main agent."
}

fn build_tool_call_test_messages(project: &ProjectSession) -> Vec<Value> {
    vec![
        json!({
            "role": "system",
            "content": format!(
                "You are CodeForge Desktop. For this test you must call the {CALCULATOR_ADD_TOOL_NAME} tool before answering. Project: {}.",
                project.name
            ),
        }),
        json!({
            "role": "user",
            "content": TOOL_CALL_TEST_PROMPT,
        }),
    ]
}

fn parse_openai_response(response_body: &Value) -> Result<OpenAiChatResponse, String> {
    serde_json::from_value::<OpenAiChatResponse>(response_body.clone())
        .map_err(|error| format!("Model response parse failed: {error}; body={response_body}"))
}

fn parse_tool_calls(response_body: &Value) -> Result<Vec<OpenAiToolCall>, String> {
    let parsed = parse_openai_response(response_body)?;
    Ok(parsed
        .choices
        .first()
        .and_then(|choice| choice.message.tool_calls.clone())
        .unwrap_or_default())
}

fn response_finish_reason(response_body: &Value) -> Option<String> {
    parse_openai_response(response_body)
        .ok()
        .and_then(|parsed| parsed.choices.into_iter().next())
        .and_then(|choice| choice.finish_reason)
}

fn build_assistant_tool_call_message(response_body: &Value) -> Result<Value, String> {
    let parsed = parse_openai_response(response_body)?;
    let choice = parsed
        .choices
        .first()
        .ok_or_else(|| "Model response had no choices.".to_string())?;
    let content = choice
        .message
        .content
        .clone()
        .map(Value::String)
        .unwrap_or(Value::Null);
    Ok(json!({
        "role": "assistant",
        "content": content,
        "tool_calls": choice.message.tool_calls.clone().unwrap_or_default(),
    }))
}

fn build_tool_result_message(tool_call: &OpenAiToolCall, result: &Value) -> Value {
    json!({
        "role": "tool",
        "tool_call_id": tool_call.id.clone(),
        "name": tool_call.function.name.clone(),
        "content": result.to_string(),
    })
}

fn tool_error_result(error: &str) -> Value {
    json!({
        "status": "error",
        "ok": false,
        "output": null,
        "error": error,
        "recoveryHint": "The tool failed. If a path was not found, use list_dir with path='.' or an absolute directory, or retry search_file/search_content with a valid workspace-relative or absolute root. Read tools can inspect local paths outside the active workspace; write and edit tools remain workspace-bound."
    })
}

fn parse_tool_arguments(arguments: &str) -> Result<Value, String> {
    serde_json::from_str::<Value>(arguments).map_err(|error| {
        format!("Tool arguments JSON parse failed: {error}; arguments={arguments}")
    })
}

fn extract_message_from_response(response_body: &Value) -> Option<String> {
    parse_openai_response(response_body)
        .ok()
        .and_then(|parsed| parsed.choices.into_iter().next())
        .and_then(|choice| choice.message.content)
}

fn redact_trace_value(value: &Value) -> Value {
    match value {
        Value::String(text) if text.starts_with("data:image/") => {
            let redacted = text
                .split_once(',')
                .map(|(prefix, _)| format!("{prefix},<redacted>"))
                .unwrap_or_else(|| "data:image/*;base64,<redacted>".to_string());
            Value::String(redacted)
        }
        Value::Array(items) => Value::Array(items.iter().map(redact_trace_value).collect()),
        Value::Object(entries) => Value::Object(
            entries
                .iter()
                .map(|(key, item)| (key.clone(), redact_trace_value(item)))
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn request_summary(request_body: &Value) -> String {
    format!(
        "model={}, tools={}, messages={}",
        string_field(request_body, "model").unwrap_or("unknown"),
        array_len(request_body, "tools"),
        array_len(request_body, "messages"),
    )
}

fn response_summary(response_body: &Value) -> String {
    match parse_openai_response(response_body) {
        Ok(parsed) => {
            let choice = parsed.choices.first();
            let finish_reason = choice
                .and_then(|choice| choice.finish_reason.as_deref())
                .unwrap_or("unknown");
            let tool_calls = choice
                .and_then(|choice| choice.message.tool_calls.as_ref())
                .map(Vec::len)
                .unwrap_or(0);
            let content_chars = choice
                .and_then(|choice| choice.message.content.as_deref())
                .map(|content| content.chars().count())
                .unwrap_or(0);
            format!(
                "finish_reason={finish_reason}, tool_calls={tool_calls}, content_chars={content_chars}"
            )
        }
        Err(_) => "response parse failed".to_string(),
    }
}

fn tool_call_summary(tool_name: &str, arguments: &Value) -> String {
    if tool_name == tool_registry::PROGRESS_UPDATE_STEPS_TOOL_NAME {
        let count = arguments
            .get("steps")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        let title = arguments
            .get("title")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("Steps");
        return format!("Update {title}: {count} step(s)");
    }

    if tool_name == CALCULATOR_ADD_TOOL_NAME {
        let a = arguments
            .get("a")
            .map(compact_json)
            .unwrap_or_else(|| "?".to_string());
        let b = arguments
            .get("b")
            .map(compact_json)
            .unwrap_or_else(|| "?".to_string());
        return format!("{CALCULATOR_ADD_TOOL_NAME}({{a:{a},b:{b}}})");
    }

    format!("{tool_name}({})", compact_json(arguments))
}

fn tool_result_summary(result: &Value) -> String {
    if result.get("source").and_then(Value::as_str) == Some("vsix") {
        let mut parts = Vec::new();
        if let Some(status) = result.get("status").and_then(Value::as_str) {
            parts.push(format!("status={status}"));
        }
        if let Some(ok) = result.get("ok").and_then(Value::as_bool) {
            parts.push(format!("ok={ok}"));
        }
        if let Some(message) = result.get("message").and_then(Value::as_str) {
            parts.push(format!("message={message}"));
        }
        if let Some(path) = result.get("path").and_then(Value::as_str) {
            parts.push(format!("path={path}"));
        }
        if let Some(count) = result.get("count").and_then(Value::as_u64) {
            parts.push(format!("count={count}"));
        }
        if let Some(truncated) = result.get("truncated").and_then(Value::as_bool) {
            parts.push(format!("truncated={truncated}"));
        }
        if let Some(text_truncated) = result.get("textTruncated").and_then(Value::as_bool) {
            parts.push(format!("textTruncated={text_truncated}"));
        }
        if let Some(available) = result.get("available").and_then(Value::as_bool) {
            parts.push(format!("available={available}"));
        }
        if !parts.is_empty() {
            return parts.join(", ");
        }
    }

    result
        .get("result")
        .map(|value| format!("result={}", compact_json(value)))
        .unwrap_or_else(|| compact_json(result))
}

fn string_field<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value.get(field).and_then(Value::as_str)
}

fn array_len(value: &Value, field: &str) -> usize {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

fn push_trace(
    traces: &mut Vec<ToolTraceEvent>,
    event: ToolTraceEvent,
    on_trace: &mut (impl FnMut(&ToolTraceEvent) + Send),
) {
    on_trace(&event);
    traces.push(event);
}

fn trace(
    task_id: &str,
    step_index: u32,
    event_type: TraceEventType,
    tool_name: Option<&str>,
    title: &str,
    input: Option<serde_json::Value>,
    output: Option<serde_json::Value>,
    output_summary: Option<String>,
    status: TraceStatus,
    duration_ms: u64,
) -> ToolTraceEvent {
    tool_trace::tool_event(
        task_id,
        step_index,
        event_type,
        tool_name.map(str::to_string),
        title.to_string(),
        input,
        output,
        output_summary,
        status,
        duration_ms,
    )
}

fn error_trace(
    task_id: &str,
    step_index: u32,
    title: &str,
    input: Option<serde_json::Value>,
    error: &str,
) -> ToolTraceEvent {
    trace(
        task_id,
        step_index,
        TraceEventType::Error,
        None,
        title,
        input,
        Some(json!({ "error": error })),
        Some(error.to_string()),
        TraceStatus::Failed,
        0,
    )
}

fn mask_secret(secret: &str) -> String {
    if secret.trim().is_empty() {
        return "not_set".to_string();
    }
    "set".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::thread;
    use std::time::Duration;

    use tiny_http::{Header, Request, Response, Server};

    #[test]
    fn chat_completion_request_enables_auto_tool_choice() {
        let selected = test_selected_model("openai");
        let request = build_chat_completion_request(
            &selected,
            vec![json!({ "role": "user", "content": "hello" })],
            Some(tool_registry::tool_definitions()),
        );

        assert_eq!(request["stream"], json!(true));
        assert_eq!(request["stream_options"]["include_usage"], json!(true));
        assert_eq!(request["tool_choice"], json!("auto"));
        assert!(request.get("temperature").is_none());
        let names = request_tool_names(&request);
        assert!(names.contains(&tool_registry::WORKSPACE_LIST_DIR_TOOL_NAME.to_string()));
        assert!(names.contains(&tool_registry::WORKSPACE_READ_FILE_TOOL_NAME.to_string()));
        assert!(names.contains(&tool_registry::WORKSPACE_SEARCH_CONTENT_TOOL_NAME.to_string()));
        assert!(names.contains(&tool_registry::WORKSPACE_EDIT_FILE_TOOL_NAME.to_string()));
        assert!(names.contains(&tool_registry::WORKSPACE_WRITE_FILE_TOOL_NAME.to_string()));
        assert!(!names.contains(&CALCULATOR_ADD_TOOL_NAME.to_string()));
    }

    #[test]
    fn kimi_chat_completion_request_downgrades_forced_tool_choice() {
        let mut selected = test_selected_model("openai-compatible");
        selected.model_id = "V-Kimi-K2.7-Code".to_string();
        let request = build_chat_completion_request_with_tool_choice(
            &selected,
            vec![json!({ "role": "user", "content": "hello" })],
            Some(tool_registry::tool_definitions()),
            Some(json!("required")),
        );

        assert_eq!(request["tool_choice"], json!("auto"));
    }

    #[test]
    fn kimi_chat_completion_request_downgrades_specific_tool_choice() {
        let mut selected = test_selected_model("openai-compatible");
        selected.model_id = "V-Kimi-K2.6".to_string();
        let request = build_chat_completion_request_with_tool_choice(
            &selected,
            vec![json!({ "role": "user", "content": "hello" })],
            Some(tool_registry::tool_definitions()),
            Some(json!({
                "type": "function",
                "function": { "name": "workspace_read_file" },
            })),
        );

        assert_eq!(request["tool_choice"], json!("auto"));
    }

    #[test]
    fn non_kimi_chat_completion_request_keeps_forced_tool_choice() {
        let selected = test_selected_model("openai-compatible");
        let request = build_chat_completion_request_with_tool_choice(
            &selected,
            vec![json!({ "role": "user", "content": "hello" })],
            Some(tool_registry::tool_definitions()),
            Some(json!("required")),
        );

        assert_eq!(request["tool_choice"], json!("required"));
    }

    #[test]
    fn custom_reasoning_request_is_merged_into_chat_completion_body() {
        let mut selected = test_selected_model("openai-compatible");
        let mut model = selected.provider.models[0].clone();
        model.reasoning_mode = "custom".to_string();
        model.default_reasoning = "enabled".to_string();
        model.reasoning = Some(crate::vs_registry::ModelReasoningConfig {
            request_field: "thinking".to_string(),
            default: "enabled".to_string(),
            levels: vec![
                crate::vs_registry::ModelReasoningLevel {
                    level: "enabled".to_string(),
                    label: "Enabled".to_string(),
                    description: String::new(),
                    request: Some(json!({ "thinking": { "type": "enabled" } })),
                },
                crate::vs_registry::ModelReasoningLevel {
                    level: "disabled".to_string(),
                    label: "Disabled".to_string(),
                    description: String::new(),
                    request: Some(json!({ "thinking": { "type": "disabled" } })),
                },
            ],
        });
        selected.model = Some(model);
        selected.reasoning_effort = Some("enabled".to_string());

        let request = build_chat_completion_request(
            &selected,
            vec![json!({ "role": "user", "content": "hello" })],
            None,
        );

        assert_eq!(request["thinking"]["type"], json!("enabled"));
        assert!(request.get("reasoning_effort").is_none());
    }

    #[test]
    fn rate_limit_errors_are_retryable() {
        assert!(is_rate_limit_error(
            r#"{"error":{"code":"AccountRateLimitExceeded","type":"TooManyRequests"}}"#
        ));
        assert!(is_rate_limit_error(
            "Model request failed. status=429; body=too many requests"
        ));
        assert!(!is_rate_limit_error(
            r#"{"error":{"code":"InvalidParameter","type":"BadRequest"}}"#
        ));
    }

    #[test]
    fn tool_name_map_rewrites_provider_unsafe_names() {
        let tools = vec![
            json!({
                "type": "function",
                "function": {
                    "name": "workspace/read_file",
                    "parameters": { "type": "object", "properties": {} },
                },
            }),
            json!({
                "type": "function",
                "function": {
                    "name": "vs.current_document",
                    "parameters": { "type": "object", "properties": {} },
                },
            }),
            json!({
                "type": "function",
                "function": {
                    "name": "workspace_read_file",
                    "parameters": { "type": "object", "properties": {} },
                },
            }),
        ];

        let name_map = ToolNameMap::from_tools(&tools);
        let model_tools = name_map.tools_for_model(&tools);
        let names = tool_definition_names(&model_tools);

        assert_eq!(names[0], "workspace_read_file");
        assert_eq!(names[1], "vs_current_document");
        assert_eq!(names[2], "workspace_read_file_2");
        assert_eq!(
            name_map.original_name("workspace_read_file"),
            "workspace/read_file"
        );
        assert_eq!(
            name_map.original_name("workspace_read_file_2"),
            "workspace_read_file"
        );
        assert_eq!(
            name_map.original_name("vs_current_document"),
            "vs.current_document"
        );
    }

    #[test]
    fn assistant_and_tool_messages_use_openai_tool_call_format() {
        let response = tool_call_response();
        let assistant_message = build_assistant_tool_call_message(&response).unwrap();
        let tool_call = parse_tool_calls(&response).unwrap().remove(0);
        let tool_message = build_tool_result_message(&tool_call, &json!({ "result": 2 }));

        assert_eq!(assistant_message["role"], json!("assistant"));
        assert_eq!(assistant_message["tool_calls"][0]["id"], json!("call_1"));
        assert_eq!(tool_message["role"], json!("tool"));
        assert_eq!(tool_message["tool_call_id"], json!("call_1"));
        assert_eq!(tool_message["name"], json!(CALCULATOR_ADD_TOOL_NAME));
        assert_eq!(tool_message["content"], json!("{\"result\":2}"));
    }

    #[test]
    fn openai_messages_use_developer_role_when_provider_supports_it() {
        let root = test_workspace();
        fs::create_dir_all(root.join(".codeforge")).unwrap();
        fs::write(
            root.join(".codeforge").join("codeforge.md"),
            "Use exact code evidence.",
        )
        .unwrap();
        fs::write(root.join("AGENTS.md"), "Project report rule.").unwrap();
        let project = test_project_with_root(root.to_str().unwrap());
        let selected = test_selected_model("openai");
        let messages = build_openai_messages(
            &project,
            &[chat_message(
                "user",
                "Where is this implemented?".to_string(),
            )],
            false,
            &selected,
            false,
        );

        assert_eq!(messages[0]["role"], json!("system"));
        assert_eq!(messages[1]["role"], json!("developer"));
        assert_eq!(messages[2]["role"], json!("user"));
        assert_eq!(messages[3]["role"], json!("user"));
        assert!(messages[0]["content"]
            .as_str()
            .is_some_and(|content| content.contains("CodeForge Desktop")));
        assert!(messages[1]["content"].as_str().is_some_and(|content| {
            content.contains("Treat user statements about the code as hypotheses")
                && content.contains("Use exact code evidence.")
        }));
        assert!(messages[2]["content"]
            .as_str()
            .is_some_and(|content| content.contains("Project report rule.")));
    }

    #[test]
    fn openai_compatible_messages_merge_developer_policy_into_system() {
        let root = test_workspace();
        fs::create_dir_all(root.join(".codeforge")).unwrap();
        fs::write(
            root.join(".codeforge").join("codeforge.md"),
            "Keep evidence visible.",
        )
        .unwrap();
        let project = test_project_with_root(root.to_str().unwrap());
        let selected = test_selected_model("openai-compatible");
        let messages = build_openai_messages(
            &project,
            &[chat_message("user", "Explain this.".to_string())],
            false,
            &selected,
            false,
        );

        assert_eq!(messages[0]["role"], json!("system"));
        assert!(messages
            .iter()
            .all(|message| message["role"].as_str() != Some("developer")));
        assert!(messages[0]["content"].as_str().is_some_and(|content| {
            content.contains("<developer>")
                && content.contains("Keep evidence visible.")
                && content.contains("Do not claim rg or text search is precise semantic analysis")
        }));
        assert_eq!(messages[1]["role"], json!("user"));
    }

    #[test]
    fn model_capability_enables_developer_role_for_openai_compatible_provider() {
        let root = test_workspace();
        let project = test_project_with_root(root.to_str().unwrap());
        let mut selected = test_selected_model("openai-compatible");
        selected.model = Some(selected.provider.models[0].clone());
        selected.model.as_mut().unwrap().supports_developer_role = Some(true);
        let messages = build_openai_messages(
            &project,
            &[chat_message("user", "Explain this.".to_string())],
            false,
            &selected,
            false,
        );

        assert_eq!(messages[0]["role"], json!("system"));
        assert_eq!(messages[1]["role"], json!("developer"));
        assert_eq!(messages[2]["role"], json!("user"));
    }

    #[test]
    fn model_capability_disables_developer_role_provider_heuristic() {
        let root = test_workspace();
        let project = test_project_with_root(root.to_str().unwrap());
        let mut selected = test_selected_model("openai");
        selected.model = Some(selected.provider.models[0].clone());
        selected.model.as_mut().unwrap().supports_developer_role = Some(false);
        let messages = build_openai_messages(
            &project,
            &[chat_message("user", "Explain this.".to_string())],
            false,
            &selected,
            false,
        );

        assert!(messages
            .iter()
            .all(|message| message["role"].as_str() != Some("developer")));
    }

    #[test]
    fn openai_messages_include_ai_context_index_only() {
        let root = test_workspace();
        fs::create_dir_all(root.join("doc").join("ai-context")).unwrap();
        fs::write(
            root.join("doc").join("ai-context").join("README.md"),
            "Read rendering.md only for rendering tasks.",
        )
        .unwrap();
        fs::write(
            root.join("doc").join("ai-context").join("rendering.md"),
            "Do not inject this full document by default.",
        )
        .unwrap();
        let project = test_project_with_root(root.to_str().unwrap());
        let selected = test_selected_model("openai-compatible");
        let messages = build_openai_messages(
            &project,
            &[chat_message("user", "Where is this?".to_string())],
            false,
            &selected,
            false,
        );
        let user_context = messages[1]["content"].as_str().unwrap();

        assert!(user_context.contains("<ai_context_index>"));
        assert!(user_context.contains("Read rendering.md only for rendering tasks."));
        assert!(!user_context.contains("Do not inject this full document by default."));
    }

    #[test]
    fn openai_messages_include_auto_readonly_subagent_policy_for_root_tool_runs() {
        let root = test_workspace();
        let project = test_project_with_root(root.to_str().unwrap());
        let selected = test_selected_model("openai");
        let messages = build_openai_messages(
            &project,
            &[chat_message("user", "Review this repo.".to_string())],
            false,
            &selected,
            true,
        );
        let developer = messages[1]["content"].as_str().unwrap();

        assert!(developer.contains("Auto read-only subagent policy"));
        assert!(developer.contains("proactively spawn bounded read-only subagents"));
        assert!(developer.contains("Use at most three subagents by default"));

        let child_messages = build_openai_messages(
            &project,
            &[chat_message("user", "Inspect one file.".to_string())],
            false,
            &selected,
            false,
        );
        let child_developer = child_messages[1]["content"].as_str().unwrap();
        assert!(!child_developer.contains("Auto read-only subagent policy"));
    }

    #[test]
    fn init_command_prompt_requires_retrieval_style_ai_context_docs() {
        let project = test_project();
        let prompt = ai_context_init_prompt(&project);

        assert!(is_init_command("/init"));
        assert!(prompt.contains("doc/ai-context/README.md"));
        assert!(prompt.contains("Write only documentation files under `doc/ai-context/`"));
        assert!(prompt.contains("current code wins over this doc"));
        assert!(prompt.contains("Do not use shell commands"));
    }

    #[test]
    fn intent_classifier_keeps_questions_readonly_and_detects_modes() {
        assert_eq!(
            classify_intent_mode("看看我现在有代码审查的功能吗?").mode,
            AgentIntentMode::Research
        );
        assert_eq!(
            classify_intent_mode("审查代码").mode,
            AgentIntentMode::Review
        );
        assert_eq!(
            classify_intent_mode("Review this repo.").mode,
            AgentIntentMode::Review
        );
        assert_eq!(
            classify_intent_mode("为什么这里崩溃").mode,
            AgentIntentMode::Debug
        );
        assert_eq!(
            classify_intent_mode("确认这个修复是否已经生效").mode,
            AgentIntentMode::Verify
        );
        assert_eq!(
            classify_intent_mode("把按钮改成蓝色").mode,
            AgentIntentMode::Implement
        );
        assert_eq!(
            classify_intent_mode("可以把这个做成自动识别吗?先回答").mode,
            AgentIntentMode::Research
        );
        assert_eq!(
            classify_intent_mode("do it").mode,
            AgentIntentMode::Implement
        );
    }

    #[test]
    fn openai_messages_include_intent_mode_guidance() {
        let root = test_workspace();
        let project = test_project_with_root(root.to_str().unwrap());
        let selected = test_selected_model("openai");
        let messages = build_openai_messages_for_mode(
            &project,
            &[chat_message(
                "user",
                "Where is this implemented?".to_string(),
            )],
            false,
            &selected,
            false,
            Some(AgentIntentMode::Research),
        );
        let developer = messages[1]["content"].as_str().unwrap();

        assert!(developer.contains("Internal mode: research"));
        assert!(developer.contains("Work read-only"));
        assert!(developer.contains("Inspect fresh code"));
        assert!(developer.contains("working interpretation"));
        assert!(developer.contains("each user-named category"));
        assert!(developer.contains("do not finish with only a clarification question"));
    }

    #[test]
    fn developer_prompt_treats_read_file_as_current_filesystem_evidence() {
        let project = test_project();
        let prompt = developer_prompt(&project, false);

        assert!(prompt.contains("fresh current-filesystem evidence"));
        assert!(prompt.contains("do not dismiss it as a stale snapshot"));
        assert!(prompt.contains("do not ask the user to open VS Code or run git"));
        assert!(prompt.contains("prefer grouped bullet/list sections over wide Markdown tables"));
        assert!(prompt.contains("at most 3 columns"));
        assert!(local_read_tool_required_message().contains("not stale snapshots"));
    }

    #[test]
    fn readonly_research_tools_keep_agent_spawn_without_write_tools() {
        let root = test_workspace();
        let project = test_project_with_root(root.to_str().unwrap());
        let selected = test_selected_model("openai");
        let tool_context = ToolExecutionContext {
            workspace_root: &project.repo_root,
            vs_bridge_endpoint: None,
            allow_shell: false,
            assume_yes: false,
            cli_mode: false,
            goal: None,
            mcp_runtime: None,
        };
        let tools = openai_tool_definitions(&selected, &tool_context, true, true);
        let names = tool_definition_names(&tools);

        assert!(names.contains(&tool_registry::WORKSPACE_READ_FILE_TOOL_NAME.to_string()));
        assert!(names.contains(&tool_registry::WORKSPACE_SEARCH_CONTENT_TOOL_NAME.to_string()));
        assert!(names.contains(&tool_registry::AGENT_SPAWN_TOOL_NAME.to_string()));
        assert!(!names.contains(&tool_registry::WORKSPACE_EDIT_FILE_TOOL_NAME.to_string()));
        assert!(!names.contains(&tool_registry::WORKSPACE_WRITE_FILE_TOOL_NAME.to_string()));
        assert!(!names.contains(&tool_registry::SHELL_COMMAND_TOOL_NAME.to_string()));
    }

    #[test]
    fn streaming_tool_call_chunks_without_index_are_merged() {
        let mut accumulator = StreamingChatCompletionAccumulator::default();
        accumulator.accept_chunk(&json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": tool_registry::WORKSPACE_READ_FILE_TOOL_NAME }
                    }]
                },
                "finish_reason": null
            }]
        }));
        accumulator.accept_chunk(&json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "function": { "arguments": "{\"path\":\"sample.txt\"}" }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }));

        let response = accumulator.into_response().unwrap();
        let tool_call = parse_tool_calls(&response).unwrap().remove(0);

        assert_eq!(tool_call.id, "call_1");
        assert_eq!(
            tool_call.function.name,
            tool_registry::WORKSPACE_READ_FILE_TOOL_NAME
        );
        assert_eq!(tool_call.function.arguments, "{\"path\":\"sample.txt\"}");
    }

    #[test]
    fn final_response_trace_records_normal_message() {
        let completion = ChatCompletionResult {
            duration_ms: 12,
            request_body: json!({ "messages": [] }),
            response_body: json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": "The result is 2."
                    }
                }]
            }),
            token_usage: TokenUsage::default(),
        };
        let mut traces = Vec::new();
        let mut step_index = 1;
        let mut ignore_trace = |_event: &ToolTraceEvent| {};

        push_final_response_trace(
            "task",
            &mut traces,
            &mut step_index,
            &completion,
            false,
            &mut ignore_trace,
        );

        assert_eq!(traces[0].title, "final_response");
        assert!(matches!(traces[0].status, TraceStatus::Success));
        assert_eq!(
            traces[0].output_summary.as_deref(),
            Some("The result is 2.")
        );
        assert_eq!(traces[0].duration_ms, Some(12));
    }

    #[test]
    fn subagent_fallback_final_response_replaces_empty_parent_final() {
        let mut traces = vec![trace(
            "parent",
            1,
            TraceEventType::FinalResponse,
            None,
            "final_response",
            None,
            Some(json!({ "message": "" })),
            Some("Final response was empty".to_string()),
            TraceStatus::Warning,
            0,
        )];
        let subagent_runs = vec![SubagentTraceRun {
            task_id: "child".to_string(),
            parent_task_id: "parent".to_string(),
            agent_name: "explorer".to_string(),
            task_name: "edge-buffer-map".to_string(),
            read_only: true,
            subagent_depth: 1,
            status: "completed".to_string(),
            summary: Some("子任务已经完成分析。".to_string()),
            traces: Vec::new(),
        }];
        let mut step_index = 2;
        let mut emitted = Vec::new();

        push_subagent_fallback_final_response(
            "parent",
            &subagent_runs,
            &mut traces,
            &mut step_index,
            &mut |event| emitted.push(event.clone()),
        );

        let final_response = traces.last().unwrap();
        assert_eq!(step_index, 3);
        assert_eq!(emitted.len(), 1);
        assert_eq!(final_response.title, "final_response");
        assert!(matches!(final_response.status, TraceStatus::Success));
        assert!(final_response
            .output_summary
            .as_deref()
            .is_some_and(|summary| summary.contains("子任务已经完成分析。")));
    }

    #[test]
    fn subagent_fallback_final_response_keeps_successful_parent_final() {
        let mut traces = vec![trace(
            "parent",
            1,
            TraceEventType::FinalResponse,
            None,
            "final_response",
            None,
            Some(json!({ "message": "父级回答。" })),
            Some("父级回答。".to_string()),
            TraceStatus::Success,
            0,
        )];
        let subagent_runs = vec![SubagentTraceRun {
            task_id: "child".to_string(),
            parent_task_id: "parent".to_string(),
            agent_name: "explorer".to_string(),
            task_name: "edge-buffer-map".to_string(),
            read_only: true,
            subagent_depth: 1,
            status: "completed".to_string(),
            summary: Some("子任务摘要。".to_string()),
            traces: Vec::new(),
        }];
        let mut step_index = 2;

        push_subagent_fallback_final_response(
            "parent",
            &subagent_runs,
            &mut traces,
            &mut step_index,
            &mut |_event| {},
        );

        assert_eq!(step_index, 2);
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0].output_summary.as_deref(), Some("父级回答。"));
    }

    #[test]
    fn final_response_trace_can_warn_when_required_tool_was_not_called() {
        let completion = ChatCompletionResult {
            duration_ms: 8,
            request_body: json!({ "messages": [] }),
            response_body: json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": "I can answer without a tool."
                    }
                }]
            }),
            token_usage: TokenUsage::default(),
        };
        let mut traces = Vec::new();
        let mut step_index = 1;
        let mut ignore_trace = |_event: &ToolTraceEvent| {};

        push_final_response_trace(
            "task",
            &mut traces,
            &mut step_index,
            &completion,
            true,
            &mut ignore_trace,
        );

        assert_eq!(traces[0].title, "model_did_not_call_tool");
        assert!(matches!(traces[0].status, TraceStatus::Warning));
        assert_eq!(
            traces[0].output_summary.as_deref(),
            Some("model_did_not_call_tool")
        );
    }

    #[test]
    fn run_agent_openai_loop_executes_workspace_read_file_tool() {
        let root = test_workspace();
        fs::write(root.join("sample.txt"), "alpha\nbeta\n").unwrap();
        let model_tool_name = safe_model_tool_name(tool_registry::WORKSPACE_READ_FILE_TOOL_NAME);
        let (base_url, server_thread) = start_mock_openai_server(vec![
            tool_call_response_with_args(&model_tool_name, json!({ "path": "sample.txt" })),
            final_message_response("Read sample.txt."),
        ]);
        let project = test_project_with_root(root.to_str().unwrap());
        let settings = test_settings(&base_url);
        let input = AgentRunInput {
            project_id: project.id.clone(),
            session_id: None,
            task_id: None,
            user_prompt: "请读取 sample.txt".to_string(),
            messages: None,
            provider_id: Some("provider".to_string()),
            credential_id: Some("default".to_string()),
            model_id: Some("test-model".to_string()),
            reasoning_effort: None,
            allow_shell: false,
            assume_yes: false,
            cli_mode: false,
            goal: None,
            parent_task_id: None,
            agent_name: None,
            task_name: None,
            read_only: false,
            subagent_depth: 0,
            goal_slot: None,
        };

        let run =
            tauri::async_runtime::block_on(run_agent(&project, &settings, input, |_event| {}))
                .unwrap();
        let requests = server_thread.join().unwrap();

        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["tool_choice"], json!("required"));
        let names = request_tool_names(&requests[0]);
        assert!(names.contains(&model_tool_name));
        assert!(!names.contains(&tool_registry::WORKSPACE_READ_FILE_TOOL_NAME.to_string()));
        assert!(!names.contains(&CALCULATOR_ADD_TOOL_NAME.to_string()));
        assert!(requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"].as_str() == Some("tool")
                    && message["tool_call_id"].as_str() == Some("call_1")
                    && message["name"].as_str() == Some(model_tool_name.as_str())
                    && message["content"].as_str().is_some_and(|content| {
                        content.contains("\"file\":\"sample.txt\"")
                            && content.contains("\"text\":\"alpha\"")
                    })
            }));
        assert!(run.traces.iter().any(|event| {
            matches!(&event.event_type, TraceEventType::ToolCall)
                && event.tool_name.as_deref() == Some(tool_registry::WORKSPACE_READ_FILE_TOOL_NAME)
        }));
        assert!(run.traces.iter().any(|event| {
            matches!(&event.event_type, TraceEventType::ToolResult)
                && event.tool_name.as_deref() == Some(tool_registry::WORKSPACE_READ_FILE_TOOL_NAME)
                && event
                    .output_summary
                    .as_deref()
                    .is_some_and(|summary| summary.contains("sample.txt"))
        }));
        assert!(run.traces.iter().any(|event| {
            event.title == "final_response"
                && event.output_summary.as_deref() == Some("Read sample.txt.")
        }));
    }

    #[test]
    fn local_read_followup_requires_tool_call_before_answering() {
        let messages = vec![
            chat_message(
                "user",
                "你现在给我去读日志,路径在 C:\\Users\\13436\\AppData\\Local\\Temp".to_string(),
            ),
            chat_message(
                "assistant",
                "我没这个工具读 %TEMP%,只能读工作区。".to_string(),
            ),
        ];

        assert!(should_require_local_read_tool_call(
            &messages,
            "你已经可以有权限读了"
        ));
    }

    #[test]
    fn run_agent_retries_when_local_read_request_answers_without_tool() {
        let root = test_workspace();
        let outside_dir =
            std::env::temp_dir().join(format!("codeforge-read-retry-{}", Uuid::new_v4()));
        fs::create_dir_all(&outside_dir).unwrap();
        fs::write(outside_dir.join("wz_model_render_trace_123.log"), "trace\n").unwrap();
        let (base_url, server_thread) = start_mock_openai_server(vec![
            final_message_response("读不到。"),
            tool_call_response_with_args(
                tool_registry::WORKSPACE_LIST_DIR_TOOL_NAME,
                json!({ "path": outside_dir.to_string_lossy() }),
            ),
            final_message_response("读到了日志目录。"),
        ]);
        let project = test_project_with_root(root.to_str().unwrap());
        let settings = test_settings(&base_url);
        let input = AgentRunInput {
            project_id: project.id.clone(),
            session_id: None,
            task_id: None,
            user_prompt: "你已经可以有权限读了".to_string(),
            messages: Some(vec![
                AgentConversationMessage {
                    role: "user".to_string(),
                    content: format!(
                        "你现在给我去读日志,路径在 {}",
                        outside_dir.to_string_lossy()
                    ),
                    attachments: Vec::new(),
                },
                AgentConversationMessage {
                    role: "assistant".to_string(),
                    content: "我没这个工具读 %TEMP%,只能读工作区。".to_string(),
                    attachments: Vec::new(),
                },
                AgentConversationMessage {
                    role: "user".to_string(),
                    content: "你已经可以有权限读了".to_string(),
                    attachments: Vec::new(),
                },
            ]),
            provider_id: Some("provider".to_string()),
            credential_id: Some("default".to_string()),
            model_id: Some("test-model".to_string()),
            reasoning_effort: None,
            allow_shell: false,
            assume_yes: false,
            cli_mode: false,
            goal: None,
            parent_task_id: None,
            agent_name: None,
            task_name: None,
            read_only: false,
            subagent_depth: 0,
            goal_slot: None,
        };

        let run =
            tauri::async_runtime::block_on(run_agent(&project, &settings, input, |_event| {}))
                .unwrap();
        let requests = server_thread.join().unwrap();
        let _ = fs::remove_dir_all(outside_dir);

        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0]["tool_choice"], json!("required"));
        assert_eq!(requests[1]["tool_choice"], json!("required"));
        assert!(requests[0]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"].as_str() == Some("system")
                    && message["content"].as_str().is_some_and(|content| {
                        content.contains("read-only file tool")
                            && content.contains("absolute local paths")
                    })
            }));
        assert!(run.traces.iter().any(|event| {
            event.title == "required_tool_call_missing"
                && matches!(&event.event_type, TraceEventType::SystemEvent)
                && matches!(&event.status, TraceStatus::Warning)
        }));
        assert!(run.traces.iter().any(|event| {
            matches!(&event.event_type, TraceEventType::ToolResult)
                && event.tool_name.as_deref() == Some(tool_registry::WORKSPACE_LIST_DIR_TOOL_NAME)
                && event
                    .output_summary
                    .as_deref()
                    .is_some_and(|summary| summary.contains("wz_model_render_trace_123.log"))
        }));
        assert!(run.traces.iter().any(|event| {
            event.title == "final_response"
                && event.output_summary.as_deref() == Some("读到了日志目录。")
        }));
    }

    #[test]
    fn run_agent_openai_loop_executes_parallel_readonly_tool_calls_in_order() {
        let root = test_workspace();
        fs::write(root.join("first.txt"), "first\n").unwrap();
        fs::write(root.join("second.txt"), "second\n").unwrap();
        let (base_url, server_thread) = start_mock_openai_server(vec![
            tool_call_response_with_calls(vec![
                (
                    tool_registry::WORKSPACE_READ_FILE_TOOL_NAME,
                    json!({ "path": "first.txt" }),
                ),
                (
                    tool_registry::WORKSPACE_READ_FILE_TOOL_NAME,
                    json!({ "path": "second.txt" }),
                ),
            ]),
            final_message_response("Read both files."),
        ]);
        let project = test_project_with_root(root.to_str().unwrap());
        let settings = test_settings(&base_url);
        let input = AgentRunInput {
            project_id: project.id.clone(),
            session_id: None,
            task_id: None,
            user_prompt: "请读取两个文件".to_string(),
            messages: None,
            provider_id: Some("provider".to_string()),
            credential_id: Some("default".to_string()),
            model_id: Some("test-model".to_string()),
            reasoning_effort: None,
            allow_shell: false,
            assume_yes: false,
            cli_mode: false,
            goal: None,
            parent_task_id: None,
            agent_name: None,
            task_name: None,
            read_only: false,
            subagent_depth: 0,
            goal_slot: None,
        };

        let run =
            tauri::async_runtime::block_on(run_agent(&project, &settings, input, |_event| {}))
                .unwrap();
        let requests = server_thread.join().unwrap();
        let tool_messages = requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"].as_str() == Some("tool"))
            .collect::<Vec<_>>();

        assert_eq!(requests.len(), 2);
        assert_eq!(tool_messages.len(), 2);
        assert_eq!(tool_messages[0]["tool_call_id"], json!("call_1"));
        assert_eq!(tool_messages[1]["tool_call_id"], json!("call_2"));
        assert!(tool_messages[0]["content"]
            .as_str()
            .is_some_and(|content| content.contains("\"file\":\"first.txt\"")));
        assert!(tool_messages[1]["content"]
            .as_str()
            .is_some_and(|content| content.contains("\"file\":\"second.txt\"")));
        assert_eq!(
            run.traces
                .iter()
                .filter(|event| matches!(&event.event_type, TraceEventType::ToolResult))
                .count(),
            2
        );
    }

    #[test]
    fn parallel_tool_execution_only_allows_multiple_readonly_tools() {
        assert!(should_execute_tool_calls_in_parallel(&[
            parsed_tool_call(tool_registry::WORKSPACE_READ_FILE_TOOL_NAME),
            parsed_tool_call(tool_registry::WORKSPACE_SEARCH_CONTENT_TOOL_NAME),
        ]));
        assert!(!should_execute_tool_calls_in_parallel(&[
            parsed_tool_call(tool_registry::WORKSPACE_READ_FILE_TOOL_NAME),
            parsed_tool_call(tool_registry::WORKSPACE_WRITE_FILE_TOOL_NAME),
        ]));
        assert!(!should_execute_tool_calls_in_parallel(&[parsed_tool_call(
            tool_registry::WORKSPACE_READ_FILE_TOOL_NAME
        ),]));
    }

    #[test]
    fn run_agent_openai_loop_retries_empty_tool_call_finish() {
        let (base_url, server_thread) = start_mock_openai_server(vec![
            empty_tool_call_finish_response(),
            final_message_response("Recovered answer."),
        ]);
        let project = test_project();
        let settings = test_settings(&base_url);
        let input = AgentRunInput {
            project_id: project.id.clone(),
            session_id: None,
            task_id: None,
            user_prompt: "请查一下项目".to_string(),
            messages: None,
            provider_id: Some("provider".to_string()),
            credential_id: Some("default".to_string()),
            model_id: Some("test-model".to_string()),
            reasoning_effort: None,
            allow_shell: false,
            assume_yes: false,
            cli_mode: false,
            goal: None,
            parent_task_id: None,
            agent_name: None,
            task_name: None,
            read_only: false,
            subagent_depth: 0,
            goal_slot: None,
        };

        let run =
            tauri::async_runtime::block_on(run_agent(&project, &settings, input, |_event| {}))
                .unwrap();
        let requests = server_thread.join().unwrap();

        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[1]["tool_choice"],
            json!({
                "type": "function",
                "function": { "name": "search_file" },
            })
        );
        assert!(requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"].as_str() == Some("system")
                    && message["content"].as_str().is_some_and(|content| {
                        content.contains("finish_reason=tool_calls")
                            && content.contains("require a tool call")
                    })
            }));
        assert!(run.traces.iter().any(|event| {
            event.title == "empty_tool_call_response"
                && matches!(&event.event_type, TraceEventType::SystemEvent)
                && matches!(&event.status, TraceStatus::Warning)
                && event
                    .output_summary
                    .as_deref()
                    .is_some_and(|summary| summary.contains("retrying once"))
        }));
        assert!(run.traces.iter().any(|event| {
            event.title == "final_response"
                && matches!(&event.status, TraceStatus::Success)
                && event.output_summary.as_deref() == Some("Recovered answer.")
        }));
        assert!(!run.traces.iter().any(|event| {
            event.title == "final_response"
                && event.output_summary.as_deref() == Some("Final response was empty")
        }));
    }

    #[test]
    fn run_agent_openai_loop_falls_back_to_no_tool_final_answer_after_empty_tool_call_retry() {
        let (base_url, server_thread) = start_mock_openai_server(vec![
            empty_tool_call_finish_response(),
            empty_tool_call_finish_response(),
            final_message_response("Answered without tools."),
        ]);
        let project = test_project();
        let settings = test_settings(&base_url);
        let input = AgentRunInput {
            project_id: project.id.clone(),
            session_id: None,
            task_id: None,
            user_prompt: "请查一下项目".to_string(),
            messages: None,
            provider_id: Some("provider".to_string()),
            credential_id: Some("default".to_string()),
            model_id: Some("test-model".to_string()),
            reasoning_effort: None,
            allow_shell: false,
            assume_yes: false,
            cli_mode: false,
            goal: None,
            parent_task_id: None,
            agent_name: None,
            task_name: None,
            read_only: false,
            subagent_depth: 0,
            goal_slot: None,
        };

        let run =
            tauri::async_runtime::block_on(run_agent(&project, &settings, input, |_event| {}))
                .unwrap();
        let requests = server_thread.join().unwrap();

        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests[1]["tool_choice"],
            json!({
                "type": "function",
                "function": { "name": "search_file" },
            })
        );
        assert!(requests[2].get("tools").is_none());
        assert!(requests[2].get("tool_choice").is_none());
        assert!(requests[2]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"].as_str() == Some("system")
                    && message["content"].as_str().is_some_and(|content| {
                        content.contains("Tool calling is unavailable")
                            && content.contains("without calling tools")
                    })
            }));
        let empty_tool_call_warnings = run
            .traces
            .iter()
            .filter(|event| {
                event.title == "empty_tool_call_response"
                    && matches!(&event.event_type, TraceEventType::SystemEvent)
                    && matches!(&event.status, TraceStatus::Warning)
            })
            .count();
        assert_eq!(empty_tool_call_warnings, 2);
        assert!(run.traces.iter().any(|event| {
            event.title == "llm_request:3:no_tools_final"
                && matches!(&event.event_type, TraceEventType::LlmRequest)
        }));
        assert!(run.traces.iter().any(|event| {
            event.title == "final_response"
                && matches!(&event.status, TraceStatus::Success)
                && event.output_summary.as_deref() == Some("Answered without tools.")
        }));
    }

    #[test]
    fn run_agent_emits_trace_events_while_running() {
        let (base_url, server_thread) =
            start_mock_openai_server(vec![final_message_response("Done.")]);
        let project = test_project();
        let settings = test_settings(&base_url);
        let input = AgentRunInput {
            project_id: project.id.clone(),
            session_id: None,
            task_id: None,
            user_prompt: "hello".to_string(),
            messages: None,
            provider_id: Some("provider".to_string()),
            credential_id: Some("default".to_string()),
            model_id: Some("test-model".to_string()),
            reasoning_effort: None,
            allow_shell: false,
            assume_yes: false,
            cli_mode: false,
            goal: None,
            parent_task_id: None,
            agent_name: None,
            task_name: None,
            read_only: false,
            subagent_depth: 0,
            goal_slot: None,
        };
        let mut streamed_titles = Vec::new();

        let run = tauri::async_runtime::block_on(run_agent(&project, &settings, input, |event| {
            streamed_titles.push(event.title.clone())
        }))
        .unwrap();
        let requests = server_thread.join().unwrap();

        assert_eq!(requests.len(), 1);
        assert!(streamed_titles.len() >= run.traces.len());
        assert!(streamed_titles.iter().any(|title| title == "llm_request:1"));
        assert!(streamed_titles
            .iter()
            .any(|title| title == "streaming_model_message"));
        assert!(streamed_titles
            .iter()
            .any(|title| title == "llm_response:1"));
        assert!(streamed_titles
            .iter()
            .any(|title| title == "final_response"));
    }

    #[test]
    fn run_tool_call_test_reuses_openai_loop() {
        let (base_url, server_thread) = start_mock_openai_server(vec![
            tool_call_response_with_name(CALCULATOR_ADD_TOOL_NAME),
            final_message_response("The result is 2."),
        ]);
        let project = test_project();
        let settings = test_settings(&base_url);
        let mut streamed_titles = Vec::new();

        let run = tauri::async_runtime::block_on(run_tool_call_test(
            &project,
            &settings,
            Some("provider"),
            Some("default"),
            Some("test-model"),
            |event| streamed_titles.push(event.title.clone()),
        ))
        .unwrap();
        let requests = server_thread.join().unwrap();

        assert_eq!(requests.len(), 2);
        assert!(streamed_titles.iter().any(|title| title == "tool_call"));
        assert!(run.traces.iter().any(|event| {
            matches!(&event.event_type, TraceEventType::ToolResult)
                && event.tool_name.as_deref() == Some(CALCULATOR_ADD_TOOL_NAME)
        }));
        assert!(run.traces.iter().any(|event| {
            event.title == "final_response"
                && event.output_summary.as_deref() == Some("The result is 2.")
        }));
    }

    #[test]
    fn run_agent_openai_loop_records_unknown_tool_failure() {
        let (base_url, server_thread) = start_mock_openai_server(vec![
            tool_call_response_with_name("missing.tool"),
            final_message_response("The requested tool is not available."),
        ]);
        let project = test_project();
        let settings = test_settings(&base_url);
        let input = AgentRunInput {
            project_id: project.id.clone(),
            session_id: None,
            task_id: None,
            user_prompt: "请调用 missing.tool".to_string(),
            messages: None,
            provider_id: Some("provider".to_string()),
            credential_id: Some("default".to_string()),
            model_id: Some("test-model".to_string()),
            reasoning_effort: None,
            allow_shell: false,
            assume_yes: false,
            cli_mode: false,
            goal: None,
            parent_task_id: None,
            agent_name: None,
            task_name: None,
            read_only: false,
            subagent_depth: 0,
            goal_slot: None,
        };

        let run =
            tauri::async_runtime::block_on(run_agent(&project, &settings, input, |_event| {}))
                .unwrap();
        let requests = server_thread.join().unwrap();

        assert_eq!(requests.len(), 2);
        assert!(requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"].as_str() == Some("tool")
                    && message["tool_call_id"].as_str() == Some("call_1")
                    && message["name"].as_str() == Some("missing.tool")
                    && message["content"]
                        .as_str()
                        .is_some_and(|content| content.contains("Unknown tool: missing.tool"))
            }));
        assert!(run.traces.iter().any(|event| {
            event.title == "tool_result"
                && matches!(&event.event_type, TraceEventType::Error)
                && matches!(&event.status, TraceStatus::Failed)
                && event
                    .output_summary
                    .as_deref()
                    .is_some_and(|summary| summary.contains("Unknown tool: missing.tool"))
        }));
        assert!(run.traces.iter().any(|event| {
            event.title == "final_response"
                && event.output_summary.as_deref() == Some("The requested tool is not available.")
        }));
    }

    #[test]
    fn pptx_image_analysis_request_message_uses_image_url_parts() {
        let attachments = vec![json!({
            "name": "slide-1-image1.png",
            "mimeType": "image/png",
            "dataUrl": "data:image/png;base64,AAAA",
            "slideIndex": 1,
            "target": "ppt/media/image1.png",
        })];
        let message = build_pptx_image_analysis_request_message(&attachments, 1, 1);

        assert_eq!(message["role"], json!("user"));
        assert_eq!(message["content"][0]["type"], json!("text"));
        assert_eq!(message["content"][1]["type"], json!("image_url"));
        assert_eq!(
            message["content"][1]["image_url"]["url"],
            json!("data:image/png;base64,AAAA")
        );
    }

    #[test]
    fn pptx_image_analysis_followup_message_uses_text_analysis() {
        let message = build_pptx_image_analysis_followup_message(
            2,
            1,
            &[json!({
                "batchIndex": 1,
                "imageCount": 1,
                "images": [{"slideIndex": 1, "target": "ppt/media/image1.png"}],
                "analysis": "{\"images\":[{\"description\":\"chart\"}]}",
            })],
        );

        assert_eq!(message["role"], json!("user"));
        let content = message["content"].as_str().unwrap();
        assert!(content.contains("PPT embedded image understanding is complete"));
        assert!(content.contains("imageAnalyses"));
        assert!(!content.contains("data:image/"));
    }

    #[test]
    fn selected_model_supports_vision_uses_explicit_flag() {
        let mut selected = test_selected_model("openai-compatible");
        selected.model_id = "text-only-model".to_string();
        selected.model = Some(selected.provider.models[0].clone());
        selected.model.as_mut().unwrap().supports_vision = Some(false);
        assert!(!selected_model_supports_vision(&selected));

        selected.model.as_mut().unwrap().supports_vision = Some(true);
        assert!(selected_model_supports_vision(&selected));
    }

    fn tool_call_response() -> Value {
        tool_call_response_with_name(CALCULATOR_ADD_TOOL_NAME)
    }

    fn tool_call_response_with_name(tool_name: &str) -> Value {
        tool_call_response_with_args(tool_name, json!({ "a": 1, "b": 1 }))
    }

    fn tool_call_response_with_args(tool_name: &str, arguments: Value) -> Value {
        let arguments = serde_json::to_string(&arguments).unwrap();
        json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": tool_name,
                            "arguments": arguments
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })
    }

    fn tool_call_response_with_calls(calls: Vec<(&str, Value)>) -> Value {
        let tool_calls = calls
            .into_iter()
            .enumerate()
            .map(|(index, (tool_name, arguments))| {
                json!({
                    "id": format!("call_{}", index + 1),
                    "type": "function",
                    "function": {
                        "name": tool_name,
                        "arguments": serde_json::to_string(&arguments).unwrap(),
                    }
                })
            })
            .collect::<Vec<_>>();
        json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": tool_calls,
                },
                "finish_reason": "tool_calls"
            }]
        })
    }

    fn parsed_tool_call(tool_name: &str) -> ParsedToolCall {
        ParsedToolCall {
            tool_call: OpenAiToolCall {
                id: "call_1".to_string(),
                call_type: "function".to_string(),
                function: OpenAiFunctionCall {
                    name: tool_name.to_string(),
                    arguments: "{}".to_string(),
                },
            },
            tool_name: tool_name.to_string(),
            arguments: json!({}),
        }
    }

    fn tool_definition_names(tools: &[Value]) -> Vec<String> {
        tools
            .iter()
            .filter_map(|tool| {
                tool.get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect()
    }

    fn empty_tool_call_finish_response() -> Value {
        json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "reasoning_content": "I should inspect the workspace first."
                },
                "finish_reason": "tool_calls"
            }]
        })
    }

    fn request_tool_names(request: &Value) -> Vec<String> {
        request
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|tool| tool.get("function")?.get("name")?.as_str())
            .map(str::to_string)
            .collect()
    }

    fn final_message_response(message: &str) -> Value {
        json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": message
                },
                "finish_reason": "stop"
            }]
        })
    }

    fn test_selected_model(provider_type: &str) -> SelectedModel {
        SelectedModel {
            provider: ProviderConfig {
                id: "provider".to_string(),
                name: "Provider".to_string(),
                provider_type: provider_type.to_string(),
                base_url: "https://example.test/v1".to_string(),
                base_url_locked: false,
                api_key: String::new(),
                is_default: true,
                default_credential_id: "default".to_string(),
                default_model: "test-model".to_string(),
                enabled: true,
                supports_tool_call: Some(true),
                credentials: vec![ProviderCredential {
                    id: "default".to_string(),
                    name: "Default Key".to_string(),
                    enabled: true,
                    api_key: "test-key".to_string(),
                }],
                models: vec![ProviderModel {
                    id: "test-model".to_string(),
                    name: "test-model".to_string(),
                    enabled: true,
                    credential_id: String::new(),
                    reasoning_mode: String::new(),
                    default_reasoning: String::new(),
                    reasoning: None,
                    supports_vision: Some(true),
                    supports_developer_role: None,
                    owned_by: None,
                    created: None,
                }],
                temperature: 0.0,
                env_key: String::new(),
                wire_api: "responses".to_string(),
                requires_openai_auth: false,
            },
            credential: Some(ProviderCredential {
                id: "default".to_string(),
                name: "Default Key".to_string(),
                enabled: true,
                api_key: "test-key".to_string(),
            }),
            model_id: "test-model".to_string(),
            model: None,
            reasoning_effort: None,
        }
    }

    fn test_project() -> ProjectSession {
        test_project_with_root("D:\\code\\snowAgents")
    }

    fn test_project_with_root(repo_root: &str) -> ProjectSession {
        ProjectSession {
            id: "project".to_string(),
            name: "Project".to_string(),
            repo_root: repo_root.to_string(),
            solution_path: Some(format!("{repo_root}\\Project.sln")),
            uproject_path: None,
            build_command: None,
            vs_process_id: None,
            vs_bridge_endpoint: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn test_workspace() -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("codeforge-agent-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn test_settings(base_url: &str) -> AppSettings {
        let mut settings = AppSettings::default();
        let mut provider = test_selected_model("openai").provider;
        provider.base_url = base_url.to_string();
        settings.providers = vec![provider];
        settings
    }

    fn start_mock_openai_server(responses: Vec<Value>) -> (String, thread::JoinHandle<Vec<Value>>) {
        let server = Server::http("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", server.server_addr());
        let handle = thread::spawn(move || {
            responses
                .into_iter()
                .map(|response_body| {
                    let mut request = server
                        .recv_timeout(Duration::from_secs(10))
                        .unwrap()
                        .expect("expected chat completion request");
                    let request_body = read_request_body(&mut request);
                    let parsed_request = serde_json::from_str::<Value>(&request_body).unwrap();
                    let wants_stream = parsed_request
                        .get("stream")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    request
                        .respond(if wants_stream {
                            sse_response(response_body)
                        } else {
                            json_response(response_body)
                        })
                        .expect("mock response should be sent");
                    parsed_request
                })
                .collect::<Vec<_>>()
        });
        (base_url, handle)
    }

    fn read_request_body(request: &mut Request) -> String {
        let mut body = String::new();
        request.as_reader().read_to_string(&mut body).unwrap();
        body
    }

    fn json_response(body: Value) -> Response<std::io::Cursor<Vec<u8>>> {
        Response::from_string(body.to_string())
            .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
    }

    fn sse_response(body: Value) -> Response<std::io::Cursor<Vec<u8>>> {
        let mut stream_body = String::new();
        for chunk in chat_completion_stream_chunks(&body) {
            stream_body.push_str("data: ");
            stream_body.push_str(&chunk.to_string());
            stream_body.push_str("\n\n");
        }
        stream_body.push_str("data: [DONE]\n\n");
        Response::from_string(stream_body)
            .with_header(Header::from_bytes("Content-Type", "text/event-stream").unwrap())
    }

    fn chat_completion_stream_chunks(body: &Value) -> Vec<Value> {
        let choice = body
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .cloned()
            .unwrap_or_else(|| json!({}));
        let message = choice.get("message").cloned().unwrap_or_else(|| json!({}));
        let finish_reason = choice
            .get("finish_reason")
            .cloned()
            .unwrap_or_else(|| json!("stop"));
        let mut chunks = vec![json!({
            "choices": [{
                "delta": { "role": "assistant" },
                "finish_reason": null
            }]
        })];

        if let Some(reasoning) = message.get("reasoning_content").and_then(Value::as_str) {
            chunks.push(json!({
                "choices": [{
                    "delta": { "reasoning_content": reasoning },
                    "finish_reason": null
                }]
            }));
        }

        if let Some(content) = message.get("content").and_then(Value::as_str) {
            if !content.is_empty() {
                chunks.push(json!({
                    "choices": [{
                        "delta": { "content": content },
                        "finish_reason": null
                    }]
                }));
            }
        }

        if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
            for (index, tool_call) in tool_calls.iter().enumerate() {
                let function = tool_call
                    .get("function")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                chunks.push(json!({
                    "choices": [{
                        "delta": {
                            "tool_calls": [{
                                "index": index,
                                "id": tool_call.get("id").cloned().unwrap_or_else(|| json!(format!("call_{}", index + 1))),
                                "type": tool_call.get("type").cloned().unwrap_or_else(|| json!("function")),
                                "function": {
                                    "name": function.get("name").cloned().unwrap_or_else(|| json!("")),
                                    "arguments": function.get("arguments").cloned().unwrap_or_else(|| json!("")),
                                }
                            }]
                        },
                        "finish_reason": null
                    }]
                }));
            }
        }

        chunks.push(json!({
            "choices": [{
                "delta": {},
                "finish_reason": finish_reason
            }]
        }));
        chunks
    }
}
