use serde_json::{json, Value};

#[derive(Default)]
pub struct StreamingToolCall {
    pub id: Option<String>,
    pub call_type: Option<String>,
    pub name: Option<String>,
    pub arguments: String,
}

pub fn merge_streaming_tool_call(tool_calls: &mut Vec<StreamingToolCall>, delta_tool_call: &Value) {
    let index = delta_tool_call
        .get("index")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
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
        if let Some(name) = delta_tool_call.get("name").and_then(Value::as_str) {
            tool_call.name = Some(name.to_string());
        }
        if let Some(arguments) = delta_tool_call.get("arguments").and_then(Value::as_str) {
            tool_call.arguments.push_str(arguments);
        }
        return;
    };
    if let Some(name) = function.get("name").and_then(Value::as_str) {
        tool_call.name = Some(name.to_string());
    }
    if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
        tool_call.arguments.push_str(arguments);
    }
}

pub fn streaming_tool_calls_json(tool_calls: &[StreamingToolCall]) -> Vec<Value> {
    tool_calls
        .iter()
        .enumerate()
        .filter(|(_, tool_call)| {
            tool_call
                .name
                .as_deref()
                .is_some_and(|name| !name.is_empty())
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_call_chunks_with_explicit_index_are_merged_independently() {
        let mut calls = Vec::new();
        merge_streaming_tool_call(
            &mut calls,
            &json!({
                "index": 1,
                "id": "call_2",
                "type": "function",
                "function": { "name": "workspace/search", "arguments": "{\"query\":" }
            }),
        );
        merge_streaming_tool_call(
            &mut calls,
            &json!({
                "index": 0,
                "id": "call_1",
                "type": "function",
                "function": { "name": "workspace/read_file", "arguments": "{\"path\":\"a.rs\"}" }
            }),
        );
        merge_streaming_tool_call(
            &mut calls,
            &json!({
                "index": 1,
                "function": { "arguments": "\"needle\"}" }
            }),
        );

        let serialized = streaming_tool_calls_json(&calls);
        assert_eq!(serialized[0]["id"], json!("call_1"));
        assert_eq!(
            serialized[0]["function"]["arguments"],
            json!("{\"path\":\"a.rs\"}")
        );
        assert_eq!(serialized[1]["id"], json!("call_2"));
        assert_eq!(
            serialized[1]["function"]["arguments"],
            json!("{\"query\":\"needle\"}")
        );
    }

    #[test]
    fn tool_call_chunks_without_index_default_to_first_call() {
        let mut calls = Vec::new();
        merge_streaming_tool_call(
            &mut calls,
            &json!({
                "id": "call_1",
                "type": "function",
                "function": { "name": "workspace/read_file" }
            }),
        );
        merge_streaming_tool_call(
            &mut calls,
            &json!({
                "function": { "arguments": "{\"path\":\"sample.txt\"}" }
            }),
        );

        let serialized = streaming_tool_calls_json(&calls);
        assert_eq!(serialized[0]["id"], json!("call_1"));
        assert_eq!(
            serialized[0]["function"]["name"],
            json!("workspace/read_file")
        );
        assert_eq!(
            serialized[0]["function"]["arguments"],
            json!("{\"path\":\"sample.txt\"}")
        );
    }

    #[test]
    fn split_function_name_and_arguments_are_merged() {
        let mut calls = Vec::new();
        merge_streaming_tool_call(
            &mut calls,
            &json!({
                "index": 0,
                "function": { "name": "workspace/apply_patch" }
            }),
        );
        merge_streaming_tool_call(
            &mut calls,
            &json!({
                "index": 0,
                "function": { "arguments": "{\"patch\":" }
            }),
        );
        merge_streaming_tool_call(
            &mut calls,
            &json!({
                "index": 0,
                "function": { "arguments": "\"*** Begin Patch\"}" }
            }),
        );

        let serialized = streaming_tool_calls_json(&calls);
        assert_eq!(
            serialized[0]["function"]["name"],
            json!("workspace/apply_patch")
        );
        assert_eq!(
            serialized[0]["function"]["arguments"],
            json!("{\"patch\":\"*** Begin Patch\"}")
        );
    }
}
