use std::time::Instant;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::project_registry::ProjectSession;
use crate::tool_registry::{self, CALCULATOR_ADD_TOOL_NAME};
use crate::tool_trace::{self, MockAgentRun, ToolTraceEvent, TraceEventType, TraceStatus};
use crate::vs_registry::{AppSettings, ProviderConfig};

pub const TOOL_CALL_TEST_PROMPT: &str = "请必须调用 calculator.add 工具计算 1+1，然后告诉我结果。";

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRunInput {
    pub project_id: String,
    pub user_prompt: String,
    pub messages: Option<Vec<AgentConversationMessage>>,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConversationMessage {
    pub role: String,
    pub content: String,
}

#[derive(Clone)]
struct SelectedModel {
    provider: ProviderConfig,
    model_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatMessage {
    role: String,
    content: String,
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
    input: AgentRunInput,
) -> Result<MockAgentRun, String> {
    let task_id = Uuid::new_v4().to_string();
    let conversation_messages =
        normalize_conversation_messages(input.messages.as_deref(), &input.user_prompt);
    let mut traces = Vec::new();
    traces.push(trace(
        &task_id,
        1,
        TraceEventType::SystemEvent,
        None,
        "Start task",
        Some(json!({
            "projectId": project.id,
            "projectName": project.name,
            "prompt": input.user_prompt,
        })),
        None,
        Some("Task accepted".to_string()),
        TraceStatus::Success,
        0,
    ));

    let selected = match select_model(
        settings,
        input.provider_id.as_deref(),
        input.model_id.as_deref(),
    ) {
        Ok(selected) => selected,
        Err(error) => {
            traces.push(error_trace(
                &task_id,
                2,
                "select_model failed",
                Some(json!({
                    "providerId": input.provider_id,
                    "modelId": input.model_id,
                })),
                &error,
            ));
            return Ok(MockAgentRun { task_id, traces });
        }
    };

    traces.push(trace(
        &task_id,
        2,
        TraceEventType::SystemEvent,
        None,
        "select_model",
        Some(json!({
            "providerId": selected.provider.id,
            "modelId": selected.model_id,
        })),
        Some(json!({
            "provider": selected.provider.name,
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
    ));

    match call_provider(project, &selected, &conversation_messages).await {
        Ok(completion) => {
            let message = completion.message;
            let message_chars = message.chars().count();
            traces.push(trace(
                &task_id,
                3,
                TraceEventType::ToolResult,
                Some("chat_completion"),
                "chat_completion",
                Some(json!({
                    "provider": selected.provider.name,
                    "type": selected.provider.provider_type,
                    "baseUrl": selected.provider.base_url,
                    "request": completion.request_body,
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
            ));
            traces.push(trace(
                &task_id,
                4,
                TraceEventType::ModelMessage,
                None,
                "model_message",
                None,
                Some(json!({ "message": message.clone() })),
                Some(message),
                TraceStatus::Success,
                0,
            ));
        }
        Err(error) => {
            traces.push(error_trace(
                &task_id,
                3,
                "chat_completion failed",
                Some(json!({
                    "provider": selected.provider.name,
                    "type": selected.provider.provider_type,
                    "baseUrl": selected.provider.base_url,
                    "model": selected.model_id,
                    "messages": &conversation_messages,
                    "apiKey": mask_secret(&selected.provider.api_key),
                })),
                &error,
            ));
        }
    }

    Ok(MockAgentRun { task_id, traces })
}

pub async fn run_tool_call_test(
    project: &ProjectSession,
    settings: &AppSettings,
    provider_id: Option<&str>,
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

    let selected = match select_model(settings, provider_id, model_id) {
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
        "claude" | "ollama"
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

    let base_messages = build_tool_call_test_messages(project);
    let first_request = build_chat_completion_request(
        &selected,
        base_messages.clone(),
        Some(tool_registry::tool_definitions()),
    );
    push_trace(
        &mut traces,
        trace(
            &task_id,
            step_index,
            TraceEventType::LlmRequest,
            None,
            "llm_request:first",
            Some(first_request.clone()),
            None,
            Some(request_summary(&first_request)),
            TraceStatus::Success,
            0,
        ),
        &mut on_trace,
    );
    step_index += 1;

    let first_completion = match send_chat_completion(&selected, &first_request).await {
        Ok(completion) => completion,
        Err(error) => {
            push_trace(
                &mut traces,
                error_trace(
                    &task_id,
                    step_index,
                    "llm_request:first failed",
                    Some(first_request),
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
            step_index,
            TraceEventType::LlmResponse,
            None,
            "llm_response:first",
            Some(json!({
                "request": first_completion.request_body.clone(),
            })),
            Some(first_completion.response_body.clone()),
            Some(response_summary(&first_completion.response_body)),
            TraceStatus::Success,
            first_completion.duration_ms,
        ),
        &mut on_trace,
    );
    step_index += 1;

    let tool_calls = match parse_tool_calls(&first_completion.response_body) {
        Ok(tool_calls) => tool_calls,
        Err(error) => {
            push_trace(
                &mut traces,
                error_trace(
                    &task_id,
                    step_index,
                    "parse_tool_calls failed",
                    Some(first_completion.response_body),
                    &error,
                ),
                &mut on_trace,
            );
            return Ok(MockAgentRun { task_id, traces });
        }
    };

    if tool_calls.is_empty() {
        let warning = "模型没有触发工具调用";
        push_trace(
            &mut traces,
            trace(
                &task_id,
                step_index,
                TraceEventType::FinalResponse,
                None,
                "model_did_not_call_tool",
                Some(first_completion.response_body),
                Some(json!({ "warning": "model_did_not_call_tool" })),
                Some(warning.to_string()),
                TraceStatus::Warning,
                0,
            ),
            &mut on_trace,
        );
        return Ok(MockAgentRun { task_id, traces });
    }

    let mut second_messages = base_messages;
    second_messages.push(build_assistant_tool_call_message(
        &first_completion.response_body,
    )?);

    for tool_call in tool_calls {
        let arguments = match parse_tool_arguments(&tool_call.function.arguments) {
            Ok(arguments) => arguments,
            Err(error) => {
                push_trace(
                    &mut traces,
                    error_trace(
                        &task_id,
                        step_index,
                        "tool_arguments parse failed",
                        Some(json!({ "toolCall": tool_call.clone() })),
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
                step_index,
                TraceEventType::ToolCall,
                Some(&tool_call.function.name),
                "tool_call",
                Some(json!({ "toolCall": tool_call.clone(), "arguments": arguments.clone() })),
                None,
                Some(tool_call_summary(&tool_call.function.name, &arguments)),
                TraceStatus::Success,
                0,
            ),
            &mut on_trace,
        );
        step_index += 1;

        let started = Instant::now();
        let tool_result = match tool_registry::execute_tool(&tool_call.function.name, &arguments) {
            Ok(result) => result,
            Err(error) => {
                push_trace(
                    &mut traces,
                    error_trace(
                        &task_id,
                        step_index,
                        "tool execution failed",
                        Some(json!({
                            "toolName": tool_call.function.name.clone(),
                            "arguments": arguments.clone(),
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
                step_index,
                TraceEventType::ToolResult,
                Some(&tool_call.function.name),
                "tool_result",
                Some(json!({
                    "toolName": tool_call.function.name.clone(),
                    "arguments": arguments.clone(),
                })),
                Some(tool_result.clone()),
                Some(tool_result_summary(&tool_result)),
                TraceStatus::Success,
                started.elapsed().as_millis() as u64,
            ),
            &mut on_trace,
        );
        step_index += 1;

        second_messages.push(build_tool_result_message(&tool_call, &tool_result));
    }

    let second_request = build_chat_completion_request(
        &selected,
        second_messages,
        Some(tool_registry::tool_definitions()),
    );
    push_trace(
        &mut traces,
        trace(
            &task_id,
            step_index,
            TraceEventType::LlmRequest,
            None,
            "llm_request:second",
            Some(second_request.clone()),
            None,
            Some(request_summary(&second_request)),
            TraceStatus::Success,
            0,
        ),
        &mut on_trace,
    );
    step_index += 1;

    let final_completion = match send_chat_completion(&selected, &second_request).await {
        Ok(completion) => completion,
        Err(error) => {
            push_trace(
                &mut traces,
                error_trace(
                    &task_id,
                    step_index,
                    "llm_request:second failed",
                    Some(second_request),
                    &error,
                ),
                &mut on_trace,
            );
            return Ok(MockAgentRun { task_id, traces });
        }
    };
    let final_message = extract_message_from_response(&final_completion.response_body)
        .unwrap_or_default()
        .trim()
        .to_string();
    let final_summary = if final_message.is_empty() {
        "Final response was empty".to_string()
    } else {
        final_message.clone()
    };

    push_trace(
        &mut traces,
        trace(
            &task_id,
            step_index,
            TraceEventType::FinalResponse,
            None,
            "final_response",
            Some(json!({
                "request": final_completion.request_body.clone(),
            })),
            Some(json!({
                "response": final_completion.response_body.clone(),
                "message": final_message.clone(),
            })),
            Some(final_summary),
            if final_message.is_empty() {
                TraceStatus::Warning
            } else {
                TraceStatus::Success
            },
            final_completion.duration_ms,
        ),
        &mut on_trace,
    );

    Ok(MockAgentRun { task_id, traces })
}

fn select_model(
    settings: &AppSettings,
    provider_id: Option<&str>,
    model_id: Option<&str>,
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
            settings
                .providers
                .iter()
                .find(|provider| is_provider_usable(provider))
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "Provider is disabled: {}. Enable a provider or model in Settings first.",
                        requested_provider.name
                    )
                })?
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

    let model_id = model_id
        .filter(|value| {
            !value.trim().is_empty()
                && ((provider.models.is_empty())
                    || provider
                        .models
                        .iter()
                        .any(|model| model.enabled && model.id == *value))
        })
        .map(str::to_string)
        .or_else(|| {
            provider
                .models
                .iter()
                .find(|model| model.enabled)
                .map(|model| model.id.clone())
        })
        .unwrap_or_else(|| provider.default_model.clone());

    if model_id.trim().is_empty() {
        return Err(format!("Model is empty for provider {}", provider.name));
    }

    Ok(SelectedModel { provider, model_id })
}

fn is_provider_usable(provider: &ProviderConfig) -> bool {
    provider.enabled || provider.models.iter().any(|model| model.enabled)
}

async fn call_provider(
    project: &ProjectSession,
    selected: &SelectedModel,
    conversation_messages: &[ChatMessage],
) -> Result<ProviderCompletion, String> {
    let provider_type = selected.provider.provider_type.as_str();
    if provider_type == "claude" {
        return call_claude(project, selected, conversation_messages).await;
    }
    if provider_type == "ollama" {
        return call_ollama(project, selected, conversation_messages).await;
    }
    call_openai_compatible(project, selected, conversation_messages).await
}

fn build_chat_completion_request(
    selected: &SelectedModel,
    messages: Vec<Value>,
    tools: Option<Vec<Value>>,
) -> Value {
    let mut request_body = json!({
        "model": selected.model_id,
        "messages": messages,
        "temperature": selected.provider.temperature,
        "stream": false,
    });

    if let Some(tools) = tools {
        request_body["tools"] = json!(tools);
        request_body["tool_choice"] = json!("auto");
    }

    request_body
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
    if selected.provider.api_key.trim().is_empty() {
        return Err(format!(
            "API key is empty for provider {}",
            selected.provider.name
        ));
    }

    let url = format!("{base_url}/chat/completions");
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let auth = format!("Bearer {}", selected.provider.api_key.trim());
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&auth).map_err(|error| format!("Invalid API key header: {error}"))?,
    );

    let started = Instant::now();
    let response = reqwest::Client::new()
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

    let response_body = serde_json::from_str::<Value>(&body)
        .map_err(|error| format!("Model response parse failed: {error}; body={}", body))?;

    Ok(ChatCompletionResult {
        duration_ms,
        request_body: request_body.clone(),
        response_body,
    })
}

async fn call_openai_compatible(
    project: &ProjectSession,
    selected: &SelectedModel,
    conversation_messages: &[ChatMessage],
) -> Result<ProviderCompletion, String> {
    let messages = build_messages(project, conversation_messages)
        .into_iter()
        .map(|message| json!(message))
        .collect::<Vec<_>>();
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
    if selected.provider.api_key.trim().is_empty() {
        return Err(format!(
            "API key is empty for provider {}",
            selected.provider.name
        ));
    }

    let url = format!("{base_url}/messages");
    let request_body = json!({
        "model": selected.model_id,
        "max_tokens": 4096,
        "temperature": selected.provider.temperature,
        "system": system_prompt(project),
        "messages": conversation_messages,
    });

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        HeaderName::from_static("x-api-key"),
        HeaderValue::from_str(selected.provider.api_key.trim())
            .map_err(|error| format!("Invalid Claude API key header: {error}"))?,
    );
    headers.insert(
        HeaderName::from_static("anthropic-version"),
        HeaderValue::from_static("2023-06-01"),
    );

    let started = Instant::now();
    let response = reqwest::Client::new()
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
) -> Result<ProviderCompletion, String> {
    let base_url = selected.provider.base_url.trim().trim_end_matches('/');
    if base_url.is_empty() {
        return Err("Ollama Base URL is empty.".to_string());
    }

    let url = format!("{base_url}/api/chat");
    let request_body = json!({
        "model": selected.model_id,
        "messages": build_messages(project, conversation_messages),
        "stream": false,
        "options": {
            "temperature": selected.provider.temperature,
        },
    });

    let started = Instant::now();
    let response = reqwest::Client::new()
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
            let role = match message.role.as_str() {
                "assistant" => "assistant",
                "user" => "user",
                _ => return None,
            };
            if message.content.trim().is_empty() {
                return None;
            }
            Some(ChatMessage {
                role: role.to_string(),
                content: message.content.clone(),
            })
        })
        .collect::<Vec<_>>();

    if normalized.is_empty() {
        return vec![ChatMessage {
            role: "user".to_string(),
            content: user_prompt.to_string(),
        }];
    }

    normalized
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
) -> Vec<ChatMessage> {
    let mut messages = vec![ChatMessage {
        role: "system".to_string(),
        content: system_prompt(project),
    }];
    messages.extend(conversation_messages.iter().cloned());
    messages
}

fn system_prompt(project: &ProjectSession) -> String {
    format!(
        "You are SnowAgent Desktop, a coding assistant for the project \"{}\". Repo root: {}. Answer concisely and use clickable file:line references when relevant.",
        project.name, project.repo_root
    )
}

fn build_tool_call_test_messages(project: &ProjectSession) -> Vec<Value> {
    vec![
        json!({
            "role": "system",
            "content": format!(
                "You are SnowAgent Desktop. For this test you must call the {CALCULATOR_ADD_TOOL_NAME} tool before answering. Project: {}.",
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
