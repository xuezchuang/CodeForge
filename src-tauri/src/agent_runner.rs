use std::time::{Duration, Instant};

use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE, USER_AGENT,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::codex_cli_runner::{self, CODEX_CLI_PROVIDER_TYPE, CODEX_CLI_TOOL_NAME};
use crate::goal_state::GoalState;
use crate::project_registry::ProjectSession;
use crate::tool_registry::{self, ToolExecutionContext, CALCULATOR_ADD_TOOL_NAME};
use crate::tool_trace::{self, MockAgentRun, ToolTraceEvent, TraceEventType, TraceStatus};
use crate::vs_registry::{AppSettings, ProviderConfig, ProviderCredential, ProviderModel};

pub const TOOL_CALL_TEST_PROMPT: &str = "请必须调用 calculator.add 工具计算 1+1，然后告诉我结果。";
const DEFAULT_MAX_TOOL_ROUNDS: usize = 8;
const MODEL_REQUEST_TIMEOUT_SECONDS: u64 = 120;

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

#[derive(Debug, Default)]
struct StreamingToolCall {
    id: Option<String>,
    call_type: Option<String>,
    name: Option<String>,
    arguments: String,
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
    mut on_trace: impl FnMut(&ToolTraceEvent),
) -> Result<MockAgentRun, String> {
    let task_id = Uuid::new_v4().to_string();
    let conversation_messages =
        normalize_conversation_messages(input.messages.as_deref(), &input.user_prompt);
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
            return Ok(MockAgentRun { task_id, traces });
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
    if codex_cli_runner::is_codex_cli_provider(&selected.provider.provider_type) {
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
        return Ok(MockAgentRun { task_id, traces });
    }

    if supports_openai_tool_calls(&selected) {
        let initial_messages =
            build_openai_messages(project, &conversation_messages, input.cli_mode);
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

    Ok(MockAgentRun { task_id, traces })
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
    mut on_trace: impl FnMut(&ToolTraceEvent),
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
            return Ok(MockAgentRun { task_id, traces });
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
        return Ok(MockAgentRun { task_id, traces });
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

    Ok(MockAgentRun { task_id, traces })
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
    on_trace: &mut impl FnMut(&ToolTraceEvent),
) -> Result<(), String> {
    for round_index in 0..=max_tool_rounds {
        let request =
            build_chat_completion_request(selected, messages.clone(), Some(tools.clone()));
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

        let completion = match send_chat_completion(selected, &request).await {
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
                        "requestedToolCalls": tool_calls,
                    })),
                    None,
                    Some("Tool round budget reached; asking the model to answer with available evidence.".to_string()),
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

            let final_completion = match send_chat_completion(selected, &final_request).await {
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

            let result = tool_registry::execute_tool_result(
                tool_context,
                &tool_call.function.name,
                &arguments,
            )
            .await;
            let tool_result = result.to_model_value();
            let trace_status = if result.is_ok() {
                TraceStatus::Success
            } else {
                TraceStatus::Failed
            };
            let event_type = if result.is_ok() {
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
                    Some(&tool_call.function.name),
                    "tool_result",
                    Some(json!({
                        "toolName": tool_call.function.name.clone(),
                        "arguments": arguments.clone(),
                    })),
                    Some(tool_result.clone()),
                    Some(tool_result_summary(&tool_result)),
                    trace_status,
                    result.elapsed_ms,
                ),
                on_trace,
            );
            *step_index += 1;

            messages.push(build_tool_result_message(&tool_call, &tool_result));
        }
    }

    Ok(())
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

fn push_assistant_model_message_trace(
    task_id: &str,
    traces: &mut Vec<ToolTraceEvent>,
    step_index: &mut u32,
    response_body: &Value,
    on_trace: &mut impl FnMut(&ToolTraceEvent),
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

fn push_final_response_trace(
    task_id: &str,
    traces: &mut Vec<ToolTraceEvent>,
    step_index: &mut u32,
    completion: &ChatCompletionResult,
    warn_if_no_tool_call: bool,
    on_trace: &mut impl FnMut(&ToolTraceEvent),
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
    on_trace: &mut impl FnMut(&ToolTraceEvent),
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
    on_trace: &mut impl FnMut(&ToolTraceEvent),
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
    let uses_streaming = is_codebuddy_provider(selected);
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
        request_body["tool_choice"] = json!("auto");
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
    let response = model_http_client()?
        .post(&url)
        .headers(headers)
        .json(request_body)
        .send()
        .await
        .map_err(|error| format!("Model request failed: {error}"))?;
    let duration_ms = started.elapsed().as_millis() as u64;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "Model request failed. status={}; body={}",
            status.as_u16(),
            body
        ));
    }

    let response_body = if is_codebuddy_provider(selected) {
        parse_streaming_chat_completion(&body)?
    } else {
        serde_json::from_str::<Value>(&body)
            .map_err(|error| format!("Model response parse failed: {error}; body={}", body))?
    };

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

fn parse_streaming_chat_completion(body: &str) -> Result<Value, String> {
    let mut saw_data = false;
    let mut role = "assistant".to_string();
    let mut content = String::new();
    let mut reasoning_content = String::new();
    let mut finish_reason: Option<String> = None;
    let mut usage: Option<Value> = None;
    let mut tool_calls = Vec::<StreamingToolCall>::new();

    for line in body.lines() {
        let line = line.trim();
        if !line.starts_with("data:") {
            continue;
        }
        let data = line.trim_start_matches("data:").trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }

        saw_data = true;
        let chunk = serde_json::from_str::<Value>(data)
            .map_err(|error| format!("Streaming chunk parse failed: {error}; chunk={data}"))?;
        if chunk.get("usage").is_some_and(|value| !value.is_null()) {
            usage = chunk.get("usage").cloned();
        }

        let Some(choices) = chunk.get("choices").and_then(Value::as_array) else {
            continue;
        };
        for choice in choices {
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                finish_reason = Some(reason.to_string());
            }
            let Some(delta) = choice.get("delta").and_then(Value::as_object) else {
                continue;
            };
            if let Some(delta_role) = delta.get("role").and_then(Value::as_str) {
                role = delta_role.to_string();
            }
            if let Some(delta_content) = delta.get("content").and_then(Value::as_str) {
                content.push_str(delta_content);
            }
            if let Some(delta_reasoning) = delta
                .get("reasoning_content")
                .or_else(|| delta.get("reasoningContent"))
                .or_else(|| delta.get("reasoning"))
                .and_then(Value::as_str)
            {
                reasoning_content.push_str(delta_reasoning);
            }
            if let Some(delta_tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for delta_tool_call in delta_tool_calls {
                    merge_streaming_tool_call(&mut tool_calls, delta_tool_call);
                }
            }
        }
    }

    if !saw_data {
        return Err(format!(
            "Streaming response had no data chunks. body={body}"
        ));
    }

    let tool_calls = streaming_tool_calls_json(&tool_calls);
    let mut message = if tool_calls.is_empty() {
        json!({
            "role": role,
            "content": content,
        })
    } else {
        json!({
            "role": role,
            "content": if content.is_empty() { Value::Null } else { Value::String(content) },
            "tool_calls": tool_calls,
        })
    };
    if !reasoning_content.is_empty() {
        message["reasoning_content"] = Value::String(reasoning_content);
    }

    let mut response_body = json!({
        "choices": [{
            "message": message,
            "finish_reason": finish_reason.unwrap_or_else(|| "stop".to_string()),
        }],
    });
    if let Some(usage) = usage {
        response_body["usage"] = usage;
    }
    Ok(response_body)
}

fn merge_streaming_tool_call(tool_calls: &mut Vec<StreamingToolCall>, delta_tool_call: &Value) {
    let index = delta_tool_call
        .get("index")
        .and_then(Value::as_u64)
        .unwrap_or(tool_calls.len() as u64) as usize;
    while tool_calls.len() <= index {
        tool_calls.push(StreamingToolCall::default());
    }

    let tool_call = &mut tool_calls[index];
    if let Some(id) = delta_tool_call.get("id").and_then(Value::as_str) {
        tool_call.id = Some(id.to_string());
    }
    if let Some(call_type) = delta_tool_call.get("type").and_then(Value::as_str) {
        tool_call.call_type = Some(call_type.to_string());
    }
    let Some(function) = delta_tool_call.get("function").and_then(Value::as_object) else {
        return;
    };
    if let Some(name) = function.get("name").and_then(Value::as_str) {
        tool_call.name = Some(name.to_string());
    }
    if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
        tool_call.arguments.push_str(arguments);
    }
}

fn streaming_tool_calls_json(tool_calls: &[StreamingToolCall]) -> Vec<Value> {
    tool_calls
        .iter()
        .enumerate()
        .filter(|(_, tool_call)| {
            tool_call.id.is_some() || tool_call.name.is_some() || !tool_call.arguments.is_empty()
        })
        .map(|(index, tool_call)| {
            json!({
                "id": tool_call
                    .id
                    .clone()
                    .unwrap_or_else(|| format!("call_{}", index + 1)),
                "type": tool_call
                    .call_type
                    .clone()
                    .unwrap_or_else(|| "function".to_string()),
                "function": {
                    "name": tool_call.name.clone().unwrap_or_default(),
                    "arguments": tool_call.arguments,
                },
            })
        })
        .collect()
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
    let messages = build_openai_messages(project, conversation_messages, cli_mode);
    let request_body = build_chat_completion_request(selected, messages, None);
    let completion = send_chat_completion(selected, &request_body).await?;
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
        "system": system_prompt(project, cli_mode),
        "messages": conversation_messages,
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
    let mut messages = vec![ChatMessage {
        role: "system".to_string(),
        content: system_prompt(project, cli_mode),
        attachments: Vec::new(),
    }];
    messages.extend(conversation_messages.iter().cloned());
    messages
}

fn build_openai_messages(
    project: &ProjectSession,
    conversation_messages: &[ChatMessage],
    cli_mode: bool,
) -> Vec<Value> {
    build_messages(project, conversation_messages, cli_mode)
        .into_iter()
        .map(openai_chat_message_value)
        .collect()
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
    let mut prompt = String::new();
    prompt.push_str(
        "You are CodeForge CLI, the command-line coding assistant for the active workspace.\n",
    );
    prompt.push_str("Do not identify yourself as a desktop app when running in CLI mode.\n");
    prompt.push_str("Use a plain terminal style: no emoji, no marketing copy, and no generic capability list.\n");
    prompt
        .push_str("Do not advertise tools or demo capabilities unless the user asks about them.\n");
    prompt.push_str("Never mention calculator or arithmetic demo tools unless directly relevant to the user's request.\n");
    prompt.push_str("For a simple greeting, reply with one short sentence asking what task to work on; do not include examples or bullet lists.\n");
    prompt.push_str(
        "Follow this repository's AGENTS.md and keep all work inside the active workspace.\n",
    );
    prompt.push_str("Do not run package managers, installers, deploy commands, or broad build/test scripts unless the user explicitly asks for that exact command. If verification would require one of those commands, report the command instead.\n");
    prompt.push_str("Keep trace-relevant behavior explicit in your final response: summarize file changes, validation, and any skipped verification.\n\n");
    prompt.push_str(&format!("Workspace name: {}\n", project.name));
    prompt.push_str(&format!("Workspace path: {}\n\n", project.repo_root));
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

fn system_prompt(project: &ProjectSession, cli_mode: bool) -> String {
    if cli_mode {
        return format!(
            "You are CodeForge CLI, a command-line coding assistant for the active workspace. Workspace name: \"{}\". Workspace path: {}. Do not identify yourself as a desktop app in CLI mode. Do not infer a specific project name from unrelated prior context; use only the current workspace and the user's request. Use a plain terminal style: no emoji, no marketing copy, and no generic capability list. Do not advertise tools or demo capabilities unless the user asks about them. Never mention calculator or arithmetic demo tools unless directly relevant to the user's request. For a simple greeting, reply with one short sentence asking what task to work on; do not include examples or bullet lists. Internal SnowAgent class or path names may remain unchanged. Prefer Visual Studio context tools when the bridge is connected, and use repository tools when VS context is unavailable or insufficient. Do not claim rg or text search is precise semantic analysis. Do not execute arbitrary shell commands. Answer concisely and use clickable file:line references when relevant.",
            project.name, project.repo_root
        );
    }

    format!(
        "You are CodeForge Desktop, a coding assistant for the project \"{}\". Repo root: {}. Internal SnowAgent class or path names may remain unchanged. Prefer Visual Studio context tools when the bridge is connected, and use repository tools when VS context is unavailable or insufficient. Do not claim rg or text search is precise semantic analysis. Do not execute arbitrary shell commands. Answer concisely and use clickable file:line references when relevant.",
        project.name, project.repo_root
    )
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
    on_trace: &mut impl FnMut(&ToolTraceEvent),
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
                    request
                        .respond(json_response(response_body))
                        .expect("mock response should be sent");
                    serde_json::from_str::<Value>(&request_body).unwrap()
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
}
