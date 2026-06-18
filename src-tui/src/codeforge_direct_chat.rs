use crate::legacy_core::config::Config;
use codex_model_provider_info::WireApi;
use codex_protocol::openai_models::InputModality;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::openai_models::ReasoningEffortPreset;
use reqwest::header::AUTHORIZATION;
use reqwest::header::CONTENT_TYPE;
use reqwest::header::HeaderMap;
use reqwest::header::HeaderName;
use reqwest::header::HeaderValue;
use reqwest::header::USER_AGENT;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::path::Path;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppSettings {
    #[serde(default)]
    providers: Vec<ProviderConfig>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderConfig {
    #[serde(default)]
    id: String,
    #[serde(default, rename = "type")]
    provider_type: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    default_credential_id: String,
    #[serde(default)]
    default_model: String,
    #[serde(default)]
    credentials: Vec<ProviderCredential>,
    #[serde(default)]
    models: Vec<ProviderModel>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderCredential {
    #[serde(default)]
    id: String,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    api_key: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderModel {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    credential_id: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct MidasModelsFile {
    #[serde(default)]
    models: Vec<MidasModel>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct MidasModel {
    #[serde(default)]
    slug: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    default_reasoning_level: String,
    #[serde(default)]
    supported_reasoning_levels: Vec<MidasReasoningLevel>,
    #[serde(default)]
    visibility: String,
    #[serde(default = "default_supported_in_api")]
    supported_in_api: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct MidasReasoningLevel {
    #[serde(default)]
    effort: String,
    #[serde(default)]
    description: String,
}

#[derive(Clone, Debug)]
struct SelectedModel {
    provider_id: String,
    provider_type: String,
    provider_name: String,
    base_url: String,
    api_key: Option<String>,
    model_id: String,
    preferred_wire_api: DirectWireApi,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirectWireApi {
    Responses,
    ChatCompletions,
}

#[derive(Clone, Debug)]
pub(crate) struct DirectToolCall {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) arguments: String,
}

#[derive(Clone, Debug)]
pub(crate) struct DirectChatRound {
    pub(crate) text: String,
    pub(crate) tool_calls: Vec<DirectToolCall>,
    pub(crate) provider_name: String,
    pub(crate) model_id: String,
    pub(crate) usage: Option<Value>,
}

#[derive(Default)]
struct StreamingToolCall {
    id: Option<String>,
    call_type: Option<String>,
    name: Option<String>,
    arguments: String,
}

pub(crate) fn model_presets(config: &Config, default_model: &str) -> Vec<ModelPreset> {
    let midas_presets = read_midas_model_presets(config.codex_home.as_path());
    let mut presets = Vec::new();

    push_model_preset(&mut presets, default_model, &midas_presets);
    for preset in midas_presets {
        push_unique_preset(&mut presets, preset);
    }

    if let Some(settings) = read_settings(config.codex_home.as_path()) {
        for provider in settings
            .providers
            .iter()
            .filter(|provider| provider_usable(provider))
        {
            push_model_preset(&mut presets, provider.default_model.as_str(), &[]);
            for model in provider.models.iter().filter(|model| model.enabled) {
                push_model_preset(&mut presets, model.id.as_str(), &[]);
            }
        }
    }

    if presets.is_empty() {
        push_model_preset(&mut presets, "MiniMax-M3", &[]);
    }

    for (index, preset) in presets.iter_mut().enumerate() {
        preset.is_default = index == 0;
    }
    presets
}

/// Streams an assistant response for `user_message`, invoking `on_delta` for each
/// incremental piece of text the model returns. Returns the concatenated final
/// text. The same wire API fallback chain as `complete` is used.
pub(crate) async fn stream<F>(
    config: &Config,
    model: &str,
    user_message: &str,
    mut on_delta: F,
) -> Result<String, String>
where
    F: FnMut(&str) + Send,
{
    let user_message = user_message.trim();
    if user_message.is_empty() {
        return Ok(String::new());
    }

    let selected = select_model(config, model)?;
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Some(api_key) = selected
        .api_key
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        let auth = format!("Bearer {api_key}");
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&auth).map_err(|err| format!("invalid API key header: {err}"))?,
        );
    }
    if selected.provider_type == "codebuddy" || selected.provider_id == "codebuddy" {
        add_codebuddy_headers(&mut headers);
    }

    let mut attempts = vec![selected.preferred_wire_api];
    for fallback in [DirectWireApi::Responses, DirectWireApi::ChatCompletions] {
        if !attempts.contains(&fallback) {
            attempts.push(fallback);
        }
    }

    let client = reqwest::Client::new();
    let mut last_error = None;
    for wire_api in attempts {
        match stream_direct_chat(
            &client,
            &headers,
            &selected,
            wire_api,
            user_message,
            &mut on_delta,
        )
        .await
        {
            Ok(text) => return Ok(text),
            Err(err) => last_error = Some(err),
        }
    }

    Err(last_error.unwrap_or_else(|| "model request failed".to_string()))
}

pub(crate) async fn chat_completion_round<F>(
    config: &Config,
    model: &str,
    messages: Vec<Value>,
    tools: Option<Vec<Value>>,
    mut on_delta: F,
) -> Result<DirectChatRound, String>
where
    F: FnMut(&str) + Send,
{
    let selected = select_model(config, model)?;
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Some(api_key) = selected
        .api_key
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        let auth = format!("Bearer {api_key}");
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&auth).map_err(|err| format!("invalid API key header: {err}"))?,
        );
    }
    if selected.provider_type == "codebuddy" || selected.provider_id == "codebuddy" {
        add_codebuddy_headers(&mut headers);
    }

    let (url, body) = build_chat_completion_messages_body(&selected, messages, tools, true)?;
    let response = reqwest::Client::new()
        .post(url)
        .headers(headers)
        .json(&body)
        .send()
        .await
        .map_err(|err| format!("model request failed: {err}"))?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(format!(
            "model request failed for {} via {:?}. status={}; body={}",
            selected.provider_name,
            DirectWireApi::ChatCompletions,
            status.as_u16(),
            truncate_error_body(&text)
        ));
    }

    let mut round = parse_streaming_chat_round(response, &mut on_delta).await?;
    round.provider_name.clone_from(&selected.provider_name);
    round.model_id.clone_from(&selected.model_id);
    Ok(round)
}

async fn stream_direct_chat<F>(
    client: &reqwest::Client,
    headers: &HeaderMap,
    selected: &SelectedModel,
    wire_api: DirectWireApi,
    user_message: &str,
    on_delta: &mut F,
) -> Result<String, String>
where
    F: FnMut(&str) + Send,
{
    use futures::TryStreamExt;

    let (url, body) = build_request_body(selected, wire_api, user_message, true)?;

    let response = client
        .post(url)
        .headers(headers.clone())
        .json(&body)
        .send()
        .await
        .map_err(|err| format!("model request failed: {err}"))?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(format!(
            "model request failed for {} via {:?}. status={}; body={}",
            selected.provider_name,
            wire_api,
            status.as_u16(),
            truncate_error_body(&text)
        ));
    }

    let mut buffer = String::new();
    let mut text = String::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.try_next().await.map_err(|err| {
        format!(
            "model stream read failed for {}: {err}",
            selected.provider_name
        )
    })? {
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(idx) = buffer.find('\n') {
            let line: String = buffer.drain(..=idx).collect();
            let line = line.trim_end_matches(['\r', '\n']);
            if let Some(delta) = parse_sse_delta(line) {
                on_delta(&delta);
                text.push_str(&delta);
            }
        }
    }
    let tail = buffer.trim();
    if !tail.is_empty()
        && let Some(delta) = parse_sse_delta(tail)
    {
        on_delta(&delta);
        text.push_str(&delta);
    }

    Ok(text)
}

fn build_chat_completion_messages_body(
    selected: &SelectedModel,
    messages: Vec<Value>,
    tools: Option<Vec<Value>>,
    stream: bool,
) -> Result<(String, Value), String> {
    let mut body = json!({
        "model": selected.model_id,
        "messages": messages,
        "stream": stream,
    });
    if let Some(tools) = tools.filter(|tools| !tools.is_empty()) {
        body["tools"] = json!(tools);
        body["tool_choice"] = json!("auto");
    }
    if stream {
        body["stream_options"] = json!({ "include_usage": true });
    }
    Ok((chat_completions_url(&selected.base_url)?, body))
}

fn build_request_body(
    selected: &SelectedModel,
    wire_api: DirectWireApi,
    user_message: &str,
    stream: bool,
) -> Result<(String, Value), String> {
    let (url, body) = match wire_api {
        DirectWireApi::Responses => (
            responses_url(&selected.base_url)?,
            json!({
                "model": selected.model_id,
                "input": [
                    {
                        "role": "user",
                        "content": [
                            {
                                "type": "input_text",
                                "text": user_message,
                            }
                        ],
                    }
                ],
                "stream": stream,
            }),
        ),
        DirectWireApi::ChatCompletions => (
            chat_completions_url(&selected.base_url)?,
            json!({
                "model": selected.model_id,
                "messages": [
                    {
                        "role": "user",
                        "content": user_message,
                    }
                ],
                "stream": stream,
            }),
        ),
    };
    Ok((url, body))
}

async fn parse_streaming_chat_round<F>(
    response: reqwest::Response,
    on_delta: &mut F,
) -> Result<DirectChatRound, String>
where
    F: FnMut(&str) + Send,
{
    use futures::TryStreamExt;

    let mut saw_data = false;
    let mut raw_body = String::new();
    let mut buffer = String::new();
    let mut role = "assistant".to_string();
    let mut content = String::new();
    let mut finish_reason: Option<String> = None;
    let mut usage: Option<Value> = None;
    let mut tool_calls = Vec::<StreamingToolCall>::new();

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream
        .try_next()
        .await
        .map_err(|err| format!("model stream read failed: {err}"))?
    {
        let chunk_text = String::from_utf8_lossy(&chunk);
        raw_body.push_str(&chunk_text);
        buffer.push_str(&chunk_text);
        while let Some(idx) = buffer.find('\n') {
            let line: String = buffer.drain(..=idx).collect();
            parse_chat_sse_line(
                line.trim_end_matches(['\r', '\n']),
                &mut saw_data,
                &mut role,
                &mut content,
                &mut finish_reason,
                &mut usage,
                &mut tool_calls,
                on_delta,
            )?;
        }
    }

    let tail = buffer.trim();
    if !tail.is_empty() {
        parse_chat_sse_line(
            tail,
            &mut saw_data,
            &mut role,
            &mut content,
            &mut finish_reason,
            &mut usage,
            &mut tool_calls,
            on_delta,
        )?;
    }

    if !saw_data {
        let value: Value = serde_json::from_str(&raw_body).map_err(|err| {
            format!(
                "model response parse failed: {err}; body={}",
                truncate_error_body(&raw_body)
            )
        })?;
        return Ok(chat_round_from_response(value));
    }

    let raw_response = streaming_chat_response(
        role,
        content.clone(),
        finish_reason,
        usage.clone(),
        &tool_calls,
    );
    Ok(DirectChatRound {
        text: content,
        tool_calls: tool_calls_from_response(&raw_response),
        provider_name: String::new(),
        model_id: String::new(),
        usage,
    })
}

#[allow(clippy::too_many_arguments)]
fn parse_chat_sse_line<F>(
    line: &str,
    saw_data: &mut bool,
    role: &mut String,
    content: &mut String,
    finish_reason: &mut Option<String>,
    usage: &mut Option<Value>,
    tool_calls: &mut Vec<StreamingToolCall>,
    on_delta: &mut F,
) -> Result<(), String>
where
    F: FnMut(&str) + Send,
{
    let line = line.trim();
    if line.is_empty() || line.starts_with(':') || !line.starts_with("data:") {
        return Ok(());
    }
    let data = line.trim_start_matches("data:").trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(());
    }
    *saw_data = true;
    let chunk: Value = serde_json::from_str(data)
        .map_err(|err| format!("streaming chunk parse failed: {err}; chunk={data}"))?;
    if chunk.get("usage").is_some_and(|value| !value.is_null()) {
        *usage = chunk.get("usage").cloned();
    }
    let Some(choices) = chunk.get("choices").and_then(Value::as_array) else {
        return Ok(());
    };
    for choice in choices {
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            *finish_reason = Some(reason.to_string());
        }
        let Some(delta) = choice.get("delta").and_then(Value::as_object) else {
            continue;
        };
        if let Some(delta_role) = delta.get("role").and_then(Value::as_str) {
            *role = delta_role.to_string();
        }
        if let Some(delta_content) = delta.get("content").and_then(Value::as_str) {
            if !delta_content.is_empty() {
                content.push_str(delta_content);
                on_delta(delta_content);
            }
        }
        if let Some(delta_tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for delta_tool_call in delta_tool_calls {
                merge_streaming_tool_call(tool_calls, delta_tool_call);
            }
        }
    }
    Ok(())
}

fn streaming_chat_response(
    role: String,
    content: String,
    finish_reason: Option<String>,
    usage: Option<Value>,
    tool_calls: &[StreamingToolCall],
) -> Value {
    let tool_calls = streaming_tool_calls_json(tool_calls);
    let message = if tool_calls.is_empty() {
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
    let mut response = json!({
        "choices": [{
            "message": message,
            "finish_reason": finish_reason.unwrap_or_else(|| "stop".to_string()),
        }],
    });
    if let Some(usage) = usage {
        response["usage"] = usage;
    }
    response
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

fn chat_round_from_response(value: Value) -> DirectChatRound {
    DirectChatRound {
        text: parse_json_response(&value).unwrap_or_default(),
        tool_calls: tool_calls_from_response(&value),
        provider_name: String::new(),
        model_id: String::new(),
        usage: value.get("usage").cloned(),
    }
}

fn tool_calls_from_response(value: &Value) -> Vec<DirectToolCall> {
    value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("tool_calls"))
        .and_then(Value::as_array)
        .map(|tool_calls| {
            tool_calls
                .iter()
                .enumerate()
                .filter_map(|(index, tool_call)| {
                    let function = tool_call.get("function")?;
                    let name = function
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .trim();
                    if name.is_empty() {
                        return None;
                    }
                    let arguments = match function.get("arguments") {
                        Some(Value::String(text)) => text.clone(),
                        Some(value) => value.to_string(),
                        None => "{}".to_string(),
                    };
                    Some(DirectToolCall {
                        id: tool_call
                            .get("id")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("call_{}", index + 1)),
                        name: name.to_string(),
                        arguments,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_sse_delta(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with(':') || !line.starts_with("data:") {
        return None;
    }
    let data = line.trim_start_matches("data:").trim();
    if data.is_empty() || data == "[DONE]" {
        return None;
    }
    let value: Value = match serde_json::from_str(data) {
        Ok(value) => value,
        Err(_) => return None,
    };
    extract_delta_value(&value)
}

fn extract_delta_value(value: &Value) -> Option<String> {
    if value.get("type").and_then(Value::as_str) == Some("response.output_text.delta")
        && let Some(delta) = value.get("delta").and_then(Value::as_str)
    {
        return (!delta.is_empty()).then_some(delta.to_string());
    }

    if let Some(delta) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("delta"))
        .and_then(|delta| delta.get("content"))
        .and_then(Value::as_str)
    {
        return (!delta.is_empty()).then_some(delta.to_string());
    }

    if let Some(text) = parse_json_response(value)
        && !text.is_empty()
    {
        return Some(text);
    }

    None
}

fn parse_json_response(value: &Value) -> Option<String> {
    if let Some(text) = value.get("output_text").and_then(Value::as_str)
        && !text.trim().is_empty()
    {
        return Some(text.to_string());
    }

    if let Some(text) = parse_chat_choices(value)
        && !text.trim().is_empty()
    {
        return Some(text);
    }

    let mut content = String::new();
    if let Some(output) = value.get("output").and_then(Value::as_array) {
        for item in output {
            collect_response_content(item.get("content"), &mut content);
        }
    }

    (!content.trim().is_empty()).then_some(content)
}

fn parse_chat_choices(value: &Value) -> Option<String> {
    value
        .get("choices")?
        .as_array()?
        .first()?
        .get("message")?
        .get("content")
        .and_then(|content| {
            if let Some(text) = content.as_str() {
                return Some(text.to_string());
            }
            let mut collected = String::new();
            collect_response_content(Some(content), &mut collected);
            (!collected.trim().is_empty()).then_some(collected)
        })
}

fn collect_response_content(value: Option<&Value>, out: &mut String) {
    match value {
        Some(Value::String(text)) => out.push_str(text),
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(text) = item
                    .get("text")
                    .or_else(|| item.get("output_text"))
                    .and_then(Value::as_str)
                {
                    out.push_str(text);
                } else {
                    collect_response_content(item.get("content"), out);
                }
            }
        }
        Some(Value::Object(item)) => {
            if let Some(text) = item
                .get("text")
                .or_else(|| item.get("output_text"))
                .and_then(Value::as_str)
            {
                out.push_str(text);
            } else {
                collect_response_content(item.get("content"), out);
            }
        }
        _ => {}
    }
}

fn select_model(config: &Config, model: &str) -> Result<SelectedModel, String> {
    if let Ok(selected) = select_config_model(config, model) {
        return Ok(selected);
    }

    if let Some(settings) = read_settings(config.codex_home.as_path())
        && let Ok(selected) = select_settings_model(&settings, model)
    {
        return Ok(selected);
    }

    Err(format!(
        "no configured provider for direct chat. Configure ~/.codeforge/config.toml model_providers or ~/.codeforge/settings.json for model {model}"
    ))
}

fn select_config_model(config: &Config, model: &str) -> Result<SelectedModel, String> {
    let base_url = config
        .model_provider
        .base_url
        .clone()
        .unwrap_or_default()
        .trim()
        .to_string();
    if base_url.is_empty() {
        return Err("configured provider base URL is empty".to_string());
    }
    let preferred_wire_api = match config.model_provider.wire_api {
        WireApi::Responses => DirectWireApi::Responses,
    };

    let api_key = std::env::var("CODEFORGE_API_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            config
                .model_provider
                .env_key
                .as_deref()
                .and_then(|key| std::env::var(key).ok())
        })
        .or_else(|| config.model_provider.experimental_bearer_token.clone())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    Ok(SelectedModel {
        provider_id: config.model_provider_id.clone(),
        provider_type: config.model_provider_id.clone(),
        provider_name: config.model_provider.name.clone(),
        base_url,
        api_key,
        model_id: model.to_string(),
        preferred_wire_api,
    })
}

fn select_settings_model(settings: &AppSettings, model: &str) -> Result<SelectedModel, String> {
    let requested = model.trim();
    let selected_provider = settings
        .providers
        .iter()
        .filter(|provider| provider_usable(provider))
        .find(|provider| provider_has_model(provider, requested))
        .ok_or_else(|| "no enabled provider in ~/.codeforge/settings.json".to_string())?;

    let credential = select_credential(selected_provider)?;
    let model_id = selected_provider
        .models
        .iter()
        .find(|model| {
            model_matches(model, requested) && model_enabled_for_credential(model, credential)
        })
        .map(|model| model.id.trim())
        .filter(|value| is_selectable_model(value))
        .or_else(|| {
            let default_model = selected_provider.default_model.trim();
            (is_selectable_model(default_model) && default_model.eq_ignore_ascii_case(requested))
                .then_some(default_model)
        })
        .ok_or_else(|| {
            format!(
                "no enabled model {requested} for provider {}",
                selected_provider.name
            )
        })?;

    Ok(SelectedModel {
        provider_id: selected_provider.id.clone(),
        provider_type: selected_provider.provider_type.clone(),
        provider_name: selected_provider.name.clone(),
        base_url: selected_provider.base_url.trim().to_string(),
        api_key: credential.map(|credential| credential.api_key.trim().to_string()),
        model_id: model_id.to_string(),
        preferred_wire_api: DirectWireApi::ChatCompletions,
    })
}

fn read_settings(codeforge_home: &Path) -> Option<AppSettings> {
    let text = std::fs::read_to_string(codeforge_home.join("settings.json")).ok()?;
    serde_json::from_str(&text).ok()
}

fn provider_usable(provider: &ProviderConfig) -> bool {
    if provider.provider_type == "codex-cli" || provider.provider_type == "ollama" {
        return false;
    }
    if provider.base_url.trim().is_empty() {
        return false;
    }
    let model_enabled = provider.models.iter().any(|model| model.enabled);
    (provider.enabled || model_enabled || provider.credentials.iter().any(|item| item.enabled))
        && provider.credentials.iter().any(|item| item.enabled)
}

fn provider_has_model(provider: &ProviderConfig, requested: &str) -> bool {
    provider
        .models
        .iter()
        .any(|model| model.enabled && model_matches(model, requested))
        || (is_selectable_model(provider.default_model.trim())
            && provider.default_model.eq_ignore_ascii_case(requested))
}

fn model_matches(model: &ProviderModel, requested: &str) -> bool {
    model.id.eq_ignore_ascii_case(requested) || model.name.eq_ignore_ascii_case(requested)
}

fn select_credential(provider: &ProviderConfig) -> Result<Option<&ProviderCredential>, String> {
    provider
        .credentials
        .iter()
        .find(|item| {
            item.id == provider.default_credential_id
                && item.enabled
                && !item.api_key.trim().is_empty()
        })
        .or_else(|| {
            provider
                .credentials
                .iter()
                .find(|item| item.enabled && !item.api_key.trim().is_empty())
        })
        .map(Some)
        .ok_or_else(|| format!("no enabled credential for provider {}", provider.name))
}

fn model_enabled_for_credential(
    model: &ProviderModel,
    credential: Option<&ProviderCredential>,
) -> bool {
    if !model.enabled {
        return false;
    }
    let credential_id = model.credential_id.trim();
    credential_id.is_empty()
        || credential
            .map(|credential| credential.id == credential_id)
            .unwrap_or(false)
}

fn chat_completions_url(base_url: &str) -> Result<String, String> {
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.is_empty() {
        return Err("provider base URL is empty".to_string());
    }
    if base_url.ends_with("/chat/completions") {
        Ok(base_url.to_string())
    } else {
        Ok(format!("{base_url}/chat/completions"))
    }
}

fn responses_url(base_url: &str) -> Result<String, String> {
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.is_empty() {
        return Err("provider base URL is empty".to_string());
    }
    if base_url.ends_with("/responses") {
        Ok(base_url.to_string())
    } else {
        Ok(format!("{base_url}/responses"))
    }
}

fn truncate_error_body(text: &str) -> String {
    const MAX_ERROR_BODY_CHARS: usize = 800;
    let mut body = text.trim().to_string();
    if body.len() > MAX_ERROR_BODY_CHARS {
        body.truncate(MAX_ERROR_BODY_CHARS);
        body.push_str("...");
    }
    body
}

fn add_codebuddy_headers(headers: &mut HeaderMap) {
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

fn read_midas_model_presets(codeforge_home: &Path) -> Vec<ModelPreset> {
    let Ok(text) = std::fs::read_to_string(codeforge_home.join("midas-models.json")) else {
        return Vec::new();
    };
    let Ok(file) = serde_json::from_str::<MidasModelsFile>(&text) else {
        return Vec::new();
    };
    file.models
        .into_iter()
        .filter(midas_model_visible)
        .map(midas_model_preset)
        .collect()
}

fn midas_model_visible(model: &MidasModel) -> bool {
    model.supported_in_api
        && is_selectable_model(model.slug.as_str())
        && (model.visibility.trim().is_empty() || model.visibility.eq_ignore_ascii_case("list"))
}

fn midas_model_preset(model: MidasModel) -> ModelPreset {
    let model_id = model.slug.trim().to_string();
    let display_name = if model.display_name.trim().is_empty() {
        model_id.clone()
    } else {
        model.display_name.trim().to_string()
    };
    let default_reasoning_effort =
        parse_reasoning_effort(model.default_reasoning_level.as_str()).unwrap_or_default();
    let supported_reasoning_efforts = {
        let levels = model
            .supported_reasoning_levels
            .into_iter()
            .filter_map(|level| {
                let effort = parse_reasoning_effort(level.effort.as_str())?;
                let description = if level.description.trim().is_empty() {
                    format!("{} reasoning", effort.as_str())
                } else {
                    level.description.trim().to_string()
                };
                Some(ReasoningEffortPreset {
                    effort,
                    description,
                })
            })
            .collect::<Vec<_>>();
        if levels.is_empty() {
            reasoning_efforts()
        } else {
            levels
        }
    };

    ModelPreset {
        id: model_id.clone(),
        model: model_id,
        display_name,
        description: model.description.trim().to_string(),
        default_reasoning_effort,
        supported_reasoning_efforts,
        supports_personality: false,
        additional_speed_tiers: Vec::new(),
        service_tiers: Vec::new(),
        default_service_tier: None,
        is_default: false,
        upgrade: None,
        show_in_picker: true,
        availability_nux: None,
        supported_in_api: model.supported_in_api,
        input_modalities: vec![InputModality::Text, InputModality::Image],
    }
}

fn parse_reasoning_effort(value: &str) -> Option<ReasoningEffort> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        value.parse().ok()
    }
}

fn push_model_preset(presets: &mut Vec<ModelPreset>, model: &str, catalog: &[ModelPreset]) {
    let model = model.trim();
    if !is_selectable_model(model) {
        return;
    }
    if let Some(preset) = catalog
        .iter()
        .find(|preset| preset.model.eq_ignore_ascii_case(model))
    {
        push_unique_preset(presets, preset.clone());
    } else {
        push_unique_preset(presets, generic_model_preset(model));
    }
}

fn push_unique_preset(presets: &mut Vec<ModelPreset>, preset: ModelPreset) {
    if !is_selectable_model(preset.model.as_str()) {
        return;
    }
    if presets
        .iter()
        .any(|existing| existing.model.eq_ignore_ascii_case(&preset.model))
    {
        return;
    }
    presets.push(preset);
}

fn generic_model_preset(model: &str) -> ModelPreset {
    let model = model.trim().to_string();
    ModelPreset {
        id: model.clone(),
        model: model.clone(),
        display_name: model,
        description: "CodeForge configured model".to_string(),
        default_reasoning_effort: ReasoningEffort::Medium,
        supported_reasoning_efforts: reasoning_efforts(),
        supports_personality: false,
        additional_speed_tiers: Vec::new(),
        service_tiers: Vec::new(),
        default_service_tier: None,
        is_default: false,
        upgrade: None,
        show_in_picker: true,
        availability_nux: None,
        supported_in_api: true,
        input_modalities: vec![InputModality::Text, InputModality::Image],
    }
}

fn is_selectable_model(model: &str) -> bool {
    let model = model.trim();
    !model.is_empty() && !model.eq_ignore_ascii_case("default")
}

fn default_supported_in_api() -> bool {
    true
}

fn reasoning_efforts() -> Vec<ReasoningEffortPreset> {
    [
        (ReasoningEffort::Low, "Low reasoning"),
        (ReasoningEffort::Medium, "Medium reasoning"),
        (ReasoningEffort::High, "High reasoning"),
        (ReasoningEffort::XHigh, "Extra high reasoning"),
    ]
    .into_iter()
    .map(|(effort, description)| ReasoningEffortPreset {
        effort,
        description: description.to_string(),
    })
    .collect()
}
