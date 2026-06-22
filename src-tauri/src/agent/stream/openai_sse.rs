use serde_json::{json, Value};

use super::tool_call_merge::StreamingToolCall;

#[derive(Default)]
pub struct StreamingChatCompletionAccumulator {
    saw_data: bool,
    role: String,
    content: String,
    reasoning_content: String,
    finish_reason: Option<String>,
    usage: Option<Value>,
    tool_calls: Vec<StreamingToolCall>,
    error_message: Option<String>,
}

impl StreamingChatCompletionAccumulator {
    pub fn accept_line(&mut self, line: &str) -> Result<(), String> {
        let line = line.trim();
        if !line.starts_with("data:") {
            return Ok(());
        }
        let data = line.trim_start_matches("data:").trim();
        if data.is_empty() || data == "[DONE]" {
            return Ok(());
        }

        self.saw_data = true;
        let chunk = serde_json::from_str::<Value>(data)
            .map_err(|error| format!("Streaming chunk parse failed: {error}; chunk={data}"))?;
        self.accept_chunk(&chunk);
        Ok(())
    }

    pub fn accept_chunk(&mut self, chunk: &Value) {
        self.saw_data = true;
        if let Some(error) = chunk.get("error") {
            self.error_message = Some(stream_error_message(error));
            return;
        }
        if chunk.get("usage").is_some_and(|value| !value.is_null()) {
            self.usage = chunk.get("usage").cloned();
        }

        let Some(choices) = chunk.get("choices").and_then(Value::as_array) else {
            return;
        };
        for choice in choices {
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                self.finish_reason = Some(reason.to_string());
            }
            let Some(delta) = choice.get("delta").and_then(Value::as_object) else {
                continue;
            };
            if let Some(delta_role) = delta.get("role").and_then(Value::as_str) {
                self.role = delta_role.to_string();
            }
            if let Some(delta_content) = delta.get("content").and_then(Value::as_str) {
                self.content.push_str(delta_content);
            }
            if let Some(delta_reasoning) = delta
                .get("reasoning_content")
                .or_else(|| delta.get("reasoningContent"))
                .or_else(|| delta.get("reasoning"))
                .and_then(Value::as_str)
            {
                self.reasoning_content.push_str(delta_reasoning);
            }
            if let Some(delta_tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for delta_tool_call in delta_tool_calls {
                    crate::agent::stream::tool_call_merge::merge_streaming_tool_call(
                        &mut self.tool_calls,
                        delta_tool_call,
                    );
                }
            }
        }
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn reasoning_content(&self) -> &str {
        &self.reasoning_content
    }

    pub fn into_response(self) -> Result<Value, String> {
        if let Some(error_message) = self.error_message {
            return Err(format!("Model stream failed: {error_message}"));
        }
        if !self.saw_data {
            return Err("Streaming response had no data chunks".to_string());
        }

        let tool_calls =
            crate::agent::stream::tool_call_merge::streaming_tool_calls_json(&self.tool_calls);
        let mut message = if tool_calls.is_empty() {
            json!({
                "role": if self.role.is_empty() { "assistant" } else { self.role.as_str() },
                "content": self.content,
            })
        } else {
            json!({
                "role": if self.role.is_empty() { "assistant" } else { self.role.as_str() },
                "content": if self.content.is_empty() { Value::Null } else { Value::String(self.content) },
                "tool_calls": tool_calls,
            })
        };
        if !self.reasoning_content.is_empty() {
            message["reasoning_content"] = Value::String(self.reasoning_content);
        }

        let mut response_body = json!({
            "choices": [{
                "message": message,
                "finish_reason": self.finish_reason.unwrap_or_else(|| "stop".to_string()),
            }],
        });
        if let Some(usage) = self.usage {
            response_body["usage"] = usage;
        }
        Ok(response_body)
    }
}

pub fn stream_error_message(error: &Value) -> String {
    error
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response_from_lines(lines: &[&str]) -> Value {
        let mut accumulator = StreamingChatCompletionAccumulator::default();
        for line in lines {
            accumulator.accept_line(line).unwrap();
        }
        accumulator.into_response().unwrap()
    }

    #[test]
    fn content_delta_accumulates_from_sse_lines() {
        let response = response_from_lines(&[
            r#"data: {"choices":[{"delta":{"role":"assistant"},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{"content":"hel"},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{"content":"lo"},"finish_reason":"stop"}]}"#,
            "data: [DONE]",
        ]);

        assert_eq!(
            response["choices"][0]["message"]["role"],
            json!("assistant")
        );
        assert_eq!(response["choices"][0]["message"]["content"], json!("hello"));
        assert_eq!(response["choices"][0]["finish_reason"], json!("stop"));
    }

    #[test]
    fn reasoning_delta_accumulates_from_provider_aliases() {
        let response = response_from_lines(&[
            r#"data: {"choices":[{"delta":{"reasoning_content":"think "},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{"reasoningContent":"hard "},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{"reasoning":"now"},"finish_reason":"stop"}]}"#,
        ]);

        assert_eq!(
            response["choices"][0]["message"]["reasoning_content"],
            json!("think hard now")
        );
    }

    #[test]
    fn stream_usage_frame_is_preserved() {
        let response = response_from_lines(&[
            r#"data: {"choices":[{"delta":{"content":"done"},"finish_reason":"stop"}]}"#,
            r#"data: {"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#,
        ]);

        assert_eq!(response["usage"]["total_tokens"], json!(5));
    }

    #[test]
    fn provider_error_frame_fails_response() {
        let mut accumulator = StreamingChatCompletionAccumulator::default();
        accumulator
            .accept_line(r#"data: {"error":{"message":"bad gateway chunk"}}"#)
            .unwrap();

        let error = accumulator.into_response().unwrap_err();
        assert!(error.contains("bad gateway chunk"), "{error}");
    }
}
