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
use crate::project_registry::ProjectSession;
use crate::tool_interface::ToolOutput;
use crate::tool_registry::{
    self, ToolExecutionContext, CALCULATOR_ADD_TOOL_NAME, PRESENTATION_READ_PPTX_TOOL_NAME,
};
use crate::tool_trace::{
    self, ContextCompactionResult, MockAgentRun, ToolTraceEvent, TraceEventType, TraceStatus,
};
use crate::vs_registry::{
    infer_model_supports_vision, AppSettings, ProviderConfig, ProviderCredential, ProviderModel,
};

pub const TOOL_CALL_TEST_PROMPT: &str = "请必须调用 calculator.add 工具计算 1+1，然后告诉我结果。";
const DEFAULT_MAX_TOOL_ROUNDS: usize = 32;
const EMPTY_TOOL_CALL_RESPONSE_RETRY_LIMIT: usize = 1;
const MODEL_REQUEST_TIMEOUT_SECONDS: u64 = 360;
const STREAMING_TRACE_INTERVAL_MS: u64 = 750;
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
    arguments: Value,
}

#[derive(Clone, Debug)]
struct CompletedToolCall {
    tool_call: OpenAiToolCall,
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

fn tool_call_names(tool_calls: &[OpenAiToolCall]) -> Vec<String> {
    tool_calls
        .iter()
        .map(|tool_call| tool_call.function.name.clone())
        .collect()
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
    let task_id = Uuid::new_v4().to_string();
    let init_command = is_init_command(&input.user_prompt);
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
                    2,
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
            return Ok(mock_agent_run(task_id, traces, context_compaction));
        }
    };

    push_trace(
        &mut traces,
        trace(
            &task_id,
            2,
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

    let mut step_index = 3;
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
            return Ok(mock_agent_run(task_id, traces, context_compaction));
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
        return Ok(mock_agent_run(task_id, traces, context_compaction));
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
        return Ok(mock_agent_run(task_id, traces, context_compaction));
    }

    if supports_openai_tool_calls(&selected) {
        let initial_messages =
            build_openai_messages(project, &conversation_messages, input.cli_mode, &selected);
        let mut tool_context = ToolExecutionContext {
            workspace_root: &project.repo_root,
            vs_bridge_endpoint: project.vs_bridge_endpoint.as_deref(),
            allow_shell: input.allow_shell,
            assume_yes: input.assume_yes,
            cli_mode: input.cli_mode,
            goal: input.goal_slot.as_deref_mut(),
        };
        let tools = openai_tool_definitions(&selected, &tool_context);
        run_openai_tool_agent_loop(
            &task_id,
            &selected,
            &mut tool_context,
            initial_messages,
            tools,
            &mut traces,
            &mut step_index,
            DEFAULT_MAX_TOOL_ROUNDS,
            false,
            &mut on_trace,
        )
        .await?;
    } else {
        record_plain_provider_completion(
            project,
            &selected,
            &conversation_messages,
            input.cli_mode,
            &task_id,
            &mut traces,
            step_index,
            &mut on_trace,
        )
        .await;
    }

    Ok(mock_agent_run(task_id, traces, context_compaction))
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
            return Ok(mock_agent_run(task_id, traces, None));
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
        return Ok(mock_agent_run(task_id, traces, None));
    }

    let initial_messages = build_tool_call_test_messages(project);
    let mut tool_context = ToolExecutionContext {
        workspace_root: &project.repo_root,
        vs_bridge_endpoint: project.vs_bridge_endpoint.as_deref(),
        allow_shell: false,
        assume_yes: false,
        cli_mode: false,
        goal: None,
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
        &mut on_trace,
    )
    .await?;

    Ok(mock_agent_run(task_id, traces, None))
}

fn mock_agent_run(
    task_id: String,
    traces: Vec<ToolTraceEvent>,
    context_compaction: Option<ContextCompactionResult>,
) -> MockAgentRun {
    MockAgentRun {
        task_id,
        traces,
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
    on_trace: &mut (impl FnMut(&ToolTraceEvent) + Send),
) -> Result<(), String> {
    let mut empty_tool_call_response_retries = 0usize;
    let mut next_tool_choice: Option<Value> = None;

    for round_index in 0..=max_tool_rounds {
        let request = build_chat_completion_request_with_tool_choice(
            selected,
            messages.clone(),
            Some(tools.clone()),
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
            if finish_reason.as_deref() == Some("tool_calls") {
                let can_retry = empty_tool_call_response_retries
                    < EMPTY_TOOL_CALL_RESPONSE_RETRY_LIMIT
                    && round_index < max_tool_rounds;
                let retry_tool_choice = can_retry
                    .then(|| empty_tool_call_retry_tool_choice(&completion.response_body, &tools));

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
            let requested_tool_names = tool_call_names(&tool_calls);
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
                            Some(&tool_call.function.name),
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
                    Some(&tool_call.function.name),
                    "tool_call",
                    Some(json!({ "toolCall": tool_call.clone(), "arguments": arguments.clone() })),
                    None,
                    Some(tool_call_summary(&tool_call.function.name, &arguments)),
                    TraceStatus::Success,
                    0,
                ),
                on_trace,
            );
            *step_index += 1;

            parsed_tool_calls.push(ParsedToolCall {
                tool_call,
                arguments,
            });
        }

        let completed_tool_calls = execute_parsed_tool_calls(tool_context, parsed_tool_calls).await;

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
                    Some(&completed.tool_call.function.name),
                    "tool_result",
                    Some(json!({
                        "toolName": completed.tool_call.function.name.clone(),
                        "arguments": completed.arguments.clone(),
                    })),
                    Some(tool_result.clone()),
                    Some(tool_result_summary(&tool_result)),
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
                &completed.tool_call.function.name,
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
    parsed_tool_calls: Vec<ParsedToolCall>,
) -> Vec<CompletedToolCall> {
    if should_execute_tool_calls_in_parallel(&parsed_tool_calls) {
        execute_parallel_readonly_tool_calls(tool_context, parsed_tool_calls).await
    } else {
        execute_tool_calls_sequentially(tool_context, parsed_tool_calls).await
    }
}

fn should_execute_tool_calls_in_parallel(parsed_tool_calls: &[ParsedToolCall]) -> bool {
    parsed_tool_calls.len() > 1
        && parsed_tool_calls
            .iter()
            .all(|call| is_parallel_readonly_tool(&call.tool_call.function.name))
}

async fn execute_tool_calls_sequentially(
    tool_context: &mut ToolExecutionContext<'_>,
    parsed_tool_calls: Vec<ParsedToolCall>,
) -> Vec<CompletedToolCall> {
    let mut completed = Vec::with_capacity(parsed_tool_calls.len());
    for parsed in parsed_tool_calls {
        let result = tool_registry::execute_tool_result(
            tool_context,
            &parsed.tool_call.function.name,
            &parsed.arguments,
        )
        .await;
        completed.push(CompletedToolCall {
            tool_call: parsed.tool_call,
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
            let fallback_arguments = parsed.arguments.clone();
            let handle = tauri::async_runtime::spawn(async move {
                task_context
                    .execute_readonly(parsed.tool_call, parsed.arguments)
                    .await
            });
            handles.push((fallback_tool_call, fallback_arguments, handle));
        }

        for (fallback_tool_call, fallback_arguments, handle) in handles {
            let completed_tool_call = match handle.await {
                Ok(completed_tool_call) => completed_tool_call,
                Err(error) => CompletedToolCall {
                    tool_call: fallback_tool_call,
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
        arguments: Value,
    ) -> CompletedToolCall {
        let mut context = ToolExecutionContext {
            workspace_root: &self.workspace_root,
            vs_bridge_endpoint: self.vs_bridge_endpoint.as_deref(),
            allow_shell: self.allow_shell,
            assume_yes: self.assume_yes,
            cli_mode: self.cli_mode,
            goal: None,
        };
        let result =
            tool_registry::execute_tool_result(&mut context, &tool_call.function.name, &arguments)
                .await;

        CompletedToolCall {
            tool_call,
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
) -> Vec<Value> {
    if tool_context.cli_mode {
        tool_registry::cli_tool_definitions(
            &selected.provider.provider_type,
            &selected.model_id,
            tool_context.allow_shell,
        )
    } else {
        tool_registry::tool_definitions()
    }
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

async fn record_plain_provider_completion(
    project: &ProjectSession,
    selected: &SelectedModel,
    conversation_messages: &[ChatMessage],
    cli_mode: bool,
    task_id: &str,
    traces: &mut Vec<ToolTraceEvent>,
    step_index: u32,
    on_trace: &mut (impl FnMut(&ToolTraceEvent) + Send),
) {
    match call_provider(project, selected, conversation_messages, cli_mode).await {
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
    Some(value.to_ascii_lowercase())
}

fn normalize_model_reasoning_effort(
    reasoning_effort: Option<&str>,
    model: Option<&ProviderModel>,
) -> Option<String> {
    let explicit = normalize_reasoning_effort(reasoning_effort);
    let Some(model) = model else {
        return explicit;
    };
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
    let provider_type = selected.provider.provider_type.as_str();
    if provider_type == "claude" {
        return call_claude(project, selected, conversation_messages, cli_mode).await;
    }
    if provider_type == "ollama" {
        return call_ollama(project, selected, conversation_messages, cli_mode).await;
    }
    if provider_type == CODEX_CLI_PROVIDER_TYPE {
        return Err("Codex CLI provider must be executed through codex exec.".to_string());
    }
    call_openai_compatible(project, selected, conversation_messages, cli_mode).await
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
        "temperature": selected.provider.temperature,
        "stream": uses_streaming,
    });

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

    if let Some(tools) = tools {
        request_body["tools"] = json!(tools);
        request_body["tool_choice"] = tool_choice.unwrap_or_else(|| json!("auto"));
    }

    if uses_streaming {
        request_body["stream_options"] = json!({ "include_usage": true });
    }

    request_body
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
    mut streaming_trace: Option<StreamingTraceSink<'_>>,
    request_started: Instant,
) -> Result<Value, String> {
    let mut accumulator = StreamingChatCompletionAccumulator::default();
    let mut body = String::new();
    let mut line_buffer = String::new();
    let stream_event_id = Uuid::new_v4().to_string();
    let stream_started_at = Utc::now().to_rfc3339();
    let mut last_emit: Option<Instant> = None;
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

        maybe_emit_streaming_thinking(
            streaming_trace.as_mut(),
            request_body,
            &accumulator,
            &stream_event_id,
            &stream_started_at,
            request_started,
            &mut emitted_reasoning_chars,
            &mut last_emit,
            false,
        );
    }

    if !line_buffer.trim().is_empty() {
        accumulator.accept_line(line_buffer.trim_end_matches('\r'))?;
    }

    maybe_emit_streaming_thinking(
        streaming_trace.as_mut(),
        request_body,
        &accumulator,
        &stream_event_id,
        &stream_started_at,
        request_started,
        &mut emitted_reasoning_chars,
        &mut last_emit,
        true,
    );

    accumulator
        .into_response()
        .map_err(|error| format!("{error}; body={body}"))
}

fn maybe_emit_streaming_thinking(
    sink: Option<&mut StreamingTraceSink<'_>>,
    request_body: &Value,
    accumulator: &StreamingChatCompletionAccumulator,
    event_id: &str,
    stream_started_at: &str,
    request_started: Instant,
    emitted_reasoning_chars: &mut usize,
    last_emit: &mut Option<Instant>,
    force: bool,
) {
    let Some(sink) = sink else {
        return;
    };
    let reasoning = accumulator.reasoning_content();
    if reasoning.is_empty() {
        return;
    }

    let reasoning_chars = reasoning.chars().count();
    if reasoning_chars == *emitted_reasoning_chars && !(force && last_emit.is_some()) {
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
    let content = accumulator.content();
    let event = ToolTraceEvent {
        id: event_id.to_string(),
        task_id: sink.task_id.to_string(),
        step_index: sink.step_index,
        event_type: TraceEventType::ModelMessage,
        tool_name: None,
        title: "streaming_thinking".to_string(),
        input: Some(json!({
            "model": request_body.get("model").cloned().unwrap_or(Value::Null),
            "stream": true,
        })),
        output: Some(json!({
            "reasoning_content": reasoning,
            "content": content,
            "model": request_body.get("model").cloned().unwrap_or(Value::Null),
        })),
        output_summary: Some(format!("Streaming reasoning ({reasoning_chars} chars)")),
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
    *emitted_reasoning_chars = reasoning_chars;
    *last_emit = Some(Instant::now());
}

fn model_http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(MODEL_REQUEST_TIMEOUT_SECONDS))
        .build()
        .map_err(|error| format!("Model client build failed: {error}"))
}

async fn call_openai_compatible(
    project: &ProjectSession,
    selected: &SelectedModel,
    conversation_messages: &[ChatMessage],
    cli_mode: bool,
) -> Result<ProviderCompletion, String> {
    let messages = build_openai_messages(project, conversation_messages, cli_mode, selected);
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

async fn call_claude(
    project: &ProjectSession,
    selected: &SelectedModel,
    conversation_messages: &[ChatMessage],
    cli_mode: bool,
) -> Result<ProviderCompletion, String> {
    let layers = prompt_layers(project, cli_mode);
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
        "temperature": selected.provider.temperature,
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

async fn call_ollama(
    project: &ProjectSession,
    selected: &SelectedModel,
    conversation_messages: &[ChatMessage],
    cli_mode: bool,
) -> Result<ProviderCompletion, String> {
    let base_url = selected.provider.base_url.trim().trim_end_matches('/');
    if base_url.is_empty() {
        return Err("Ollama Base URL is empty.".to_string());
    }

    let url = format!("{base_url}/api/chat");
    let request_body = json!({
        "model": selected.model_id,
        "messages": build_messages(project, conversation_messages, cli_mode),
        "stream": false,
        "options": {
            "temperature": selected.provider.temperature,
        },
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
    build_layered_chat_messages(project, conversation_messages, cli_mode, false)
}

fn build_openai_messages(
    project: &ProjectSession,
    conversation_messages: &[ChatMessage],
    cli_mode: bool,
    selected: &SelectedModel,
) -> Vec<Value> {
    build_layered_chat_messages(
        project,
        conversation_messages,
        cli_mode,
        provider_supports_developer_role(selected),
    )
    .into_iter()
    .map(openai_chat_message_value)
    .collect()
}

fn build_layered_chat_messages(
    project: &ProjectSession,
    conversation_messages: &[ChatMessage],
    cli_mode: bool,
    use_developer_role: bool,
) -> Vec<ChatMessage> {
    let layers = prompt_layers(project, cli_mode);
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
    "For code-location answers, do not paste C/C++ source code blocks unless the user explicitly asks for source text. Prefer concise Markdown tables with a short description column and a location column. Use compact visible labels but unique link targets, for example [Foo.cpp:123](src/module/Foo.cpp:123). Link targets are local workspace file paths, not URLs: preserve the exact workspace-relative path returned by tools, keep literal spaces in directory names, and do not URL-encode or percent-encode them. For example write [wzShaderProgram.cpp:163](src/core/wz_render_core/09 shader/wzShaderProgram.cpp:163), not [wzShaderProgram.cpp:163](src/core/wz_render_core/09%20shader/wzShaderProgram.cpp:163). Use single-backtick inline code only for short identifiers, paths, or commands; use fenced code blocks for long snippets, pseudocode, loops, or code with comments. The UI displays the short label while opening the unique target in Visual Studio."
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
        "recoveryHint": "The tool failed. If a path was not found, use list_dir with path='.' or retry search_file/search_content with a valid workspace-relative root. If the requested file is outside the active workspace, explain that limitation."
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
        let names = request_tool_names(&request);
        assert!(names.contains(&tool_registry::WORKSPACE_LIST_DIR_TOOL_NAME.to_string()));
        assert!(names.contains(&tool_registry::WORKSPACE_READ_FILE_TOOL_NAME.to_string()));
        assert!(names.contains(&tool_registry::WORKSPACE_SEARCH_CONTENT_TOOL_NAME.to_string()));
        assert!(names.contains(&tool_registry::WORKSPACE_EDIT_FILE_TOOL_NAME.to_string()));
        assert!(names.contains(&tool_registry::WORKSPACE_WRITE_FILE_TOOL_NAME.to_string()));
        assert!(!names.contains(&CALCULATOR_ADD_TOOL_NAME.to_string()));
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
        );
        let user_context = messages[1]["content"].as_str().unwrap();

        assert!(user_context.contains("<ai_context_index>"));
        assert!(user_context.contains("Read rendering.md only for rendering tasks."));
        assert!(!user_context.contains("Do not inject this full document by default."));
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
        let (base_url, server_thread) = start_mock_openai_server(vec![
            tool_call_response_with_args(
                tool_registry::WORKSPACE_READ_FILE_TOOL_NAME,
                json!({ "path": "sample.txt" }),
            ),
            final_message_response("Read sample.txt."),
        ]);
        let project = test_project_with_root(root.to_str().unwrap());
        let settings = test_settings(&base_url);
        let input = AgentRunInput {
            project_id: project.id.clone(),
            session_id: None,
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
            goal_slot: None,
        };

        let run =
            tauri::async_runtime::block_on(run_agent(&project, &settings, input, |_event| {}))
                .unwrap();
        let requests = server_thread.join().unwrap();

        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["tool_choice"], json!("auto"));
        let names = request_tool_names(&requests[0]);
        assert!(names.contains(&tool_registry::WORKSPACE_READ_FILE_TOOL_NAME.to_string()));
        assert!(!names.contains(&CALCULATOR_ADD_TOOL_NAME.to_string()));
        assert!(requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"].as_str() == Some("tool")
                    && message["tool_call_id"].as_str() == Some("call_1")
                    && message["name"].as_str()
                        == Some(tool_registry::WORKSPACE_READ_FILE_TOOL_NAME)
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
            goal_slot: None,
        };
        let mut streamed_titles = Vec::new();

        let run = tauri::async_runtime::block_on(run_agent(&project, &settings, input, |event| {
            streamed_titles.push(event.title.clone())
        }))
        .unwrap();
        let requests = server_thread.join().unwrap();

        assert_eq!(requests.len(), 1);
        assert_eq!(run.traces.len(), streamed_titles.len());
        assert!(streamed_titles.iter().any(|title| title == "llm_request:1"));
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
            arguments: json!({}),
        }
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
