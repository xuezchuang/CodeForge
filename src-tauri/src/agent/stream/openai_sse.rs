use serde_json::{json, Value};

use super::tool_call_merge::StreamingToolCall;

const MIN_STREAMING_OVERLAP_CHARS: usize = 4;

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
                merge_streaming_text(&mut self.content, delta_content);
            }
            if let Some(delta_reasoning) = delta
                .get("reasoning_content")
                .or_else(|| delta.get("reasoningContent"))
                .or_else(|| delta.get("reasoning"))
                .and_then(Value::as_str)
            {
                merge_streaming_text(&mut self.reasoning_content, delta_reasoning);
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

fn merge_streaming_text(accumulated: &mut String, fragment: &str) {
    if fragment.is_empty() {
        return;
    }
    if accumulated.is_empty() {
        accumulated.push_str(fragment);
        return;
    }

    let fragment_chars = fragment.chars().count();
    if fragment == accumulated.as_str()
        || (fragment_chars >= MIN_STREAMING_OVERLAP_CHARS && accumulated.ends_with(fragment))
    {
        return;
    }
    if fragment.starts_with(accumulated.as_str()) {
        accumulated.clear();
        accumulated.push_str(fragment);
        return;
    }

    let overlap = largest_suffix_prefix_overlap(accumulated, fragment);
    if overlap_char_count(fragment, overlap) >= MIN_STREAMING_OVERLAP_CHARS {
        accumulated.push_str(&fragment[overlap..]);
    } else {
        accumulated.push_str(fragment);
    }
}

fn largest_suffix_prefix_overlap(accumulated: &str, fragment: &str) -> usize {
    let max_overlap = accumulated.len().min(fragment.len());
    for overlap in (1..=max_overlap).rev() {
        if !fragment.is_char_boundary(overlap) {
            continue;
        }
        if accumulated.ends_with(&fragment[..overlap]) {
            return overlap;
        }
    }
    0
}

fn overlap_char_count(fragment: &str, overlap: usize) -> usize {
    fragment[..overlap].chars().count()
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
    fn content_snapshot_frames_replace_instead_of_duplicate() {
        let response = response_from_lines(&[
            r#"data: {"choices":[{"delta":{"content":"The answer starts."},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{"content":"The answer starts. It continues."},"finish_reason":"stop"}]}"#,
        ]);

        assert_eq!(
            response["choices"][0]["message"]["content"],
            json!("The answer starts. It continues.")
        );
    }

    #[test]
    fn content_overlapping_frames_append_only_new_suffix() {
        let response = response_from_lines(&[
            r#"data: {"choices":[{"delta":{"content":"The available tools were: "},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{"content":"tools were: list_tools"},"finish_reason":"stop"}]}"#,
        ]);

        assert_eq!(
            response["choices"][0]["message"]["content"],
            json!("The available tools were: list_tools")
        );
    }

    #[test]
    fn repeated_content_frames_are_not_duplicated() {
        let response = response_from_lines(&[
            r#"data: {"choices":[{"delta":{"content":"Check the workspace."},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{"content":"Check the workspace."},"finish_reason":"stop"}]}"#,
        ]);

        assert_eq!(
            response["choices"][0]["message"]["content"],
            json!("Check the workspace.")
        );
    }

    #[test]
    fn short_incidental_content_overlap_is_appended() {
        let response = response_from_lines(&[
            r#"data: {"choices":[{"delta":{"content":"hel"},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{"content":"lo"},"finish_reason":"stop"}]}"#,
        ]);

        assert_eq!(response["choices"][0]["message"]["content"], json!("hello"));
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
    fn reasoning_snapshot_frames_replace_instead_of_duplicate() {
        let response = response_from_lines(&[
            r#"data: {"choices":[{"delta":{"reasoning":"所有相关位置都掌握了。"},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{"reasoning":"所有相关位置都掌握了。媒体查询 9470-9617 范围确认。"},"finish_reason":"stop"}]}"#,
        ]);

        assert_eq!(
            response["choices"][0]["message"]["reasoning_content"],
            json!("所有相关位置都掌握了。媒体查询 9470-9617 范围确认。")
        );
    }

    #[test]
    fn reasoning_overlapping_frames_append_only_new_suffix() {
        let response = response_from_lines(&[
            r#"data: {"choices":[{"delta":{"reasoning_content":"媒体查询 9470-"},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{"reasoning_content":"9470-9617 范围确认。"},"finish_reason":"stop"}]}"#,
        ]);

        assert_eq!(
            response["choices"][0]["message"]["reasoning_content"],
            json!("媒体查询 9470-9617 范围确认。")
        );
    }

    #[test]
    fn repeated_reasoning_frames_are_not_duplicated() {
        let response = response_from_lines(&[
            r#"data: {"choices":[{"delta":{"reasoning_content":"检查媒体查询。"},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{"reasoning_content":"检查媒体查询。"},"finish_reason":"stop"}]}"#,
        ]);

        assert_eq!(
            response["choices"][0]["message"]["reasoning_content"],
            json!("检查媒体查询。")
        );
    }

    #[test]
    fn short_incidental_reasoning_overlap_is_appended() {
        let response = response_from_lines(&[
            r#"data: {"choices":[{"delta":{"reasoning_content":"hel"},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{"reasoning_content":"lo"},"finish_reason":"stop"}]}"#,
        ]);

        assert_eq!(
            response["choices"][0]["message"]["reasoning_content"],
            json!("hello")
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
