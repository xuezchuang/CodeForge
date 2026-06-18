//! CodeForge-owned Visual Studio bridge client for the standalone TUI.
//!
//! Mirrors `src-tauri/src/vs_bridge_client.rs` so the TUI carries the
//! same set of VS tools (`currentSolution`, `currentDocument`,
//! `currentSelection`, `listProjects`, `findDefinition`,
//! `findReferences`, `getErrorList`) that the Tauri desktop uses.
//! The TUI does not depend on the Tauri desktop crate, so the TUI
//! ships its own copy.
//!
//! The bridge endpoint is read from the `CODEFORGE_VS_BRIDGE_URL`
//! environment variable. When it is empty or unset, every tool
//! returns an honest `bridge_not_connected` response so the model
//! can react to that signal instead of being told the bridge is
//! silently working. This matches the Tauri behavior.

use std::time::Duration;

use serde_json::{Map, Value, json};

const VS_BRIDGE_TIMEOUT_SECONDS: u64 = 5;

/// Read the bridge endpoint from the environment. Returns `None` when
/// unset or empty so callers can surface `bridge_not_connected`.
pub fn endpoint() -> Option<String> {
    std::env::var("CODEFORGE_VS_BRIDGE_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Resolve the bridge endpoint from an explicit override first, then
/// from the environment. Both `None` and empty strings are treated as
/// "not configured".
pub fn resolve_endpoint(explicit: Option<&str>) -> Option<String> {
    if let Some(value) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        return Some(value.to_string());
    }
    endpoint()
}

pub async fn call_current_solution(endpoint: Option<&str>) -> Result<Value, String> {
    call_endpoint(endpoint, "currentSolution", json!({})).await
}

pub async fn call_current_document(endpoint: Option<&str>) -> Result<Value, String> {
    call_endpoint(endpoint, "currentDocument", json!({})).await
}

pub async fn call_current_selection(endpoint: Option<&str>) -> Result<Value, String> {
    call_endpoint(endpoint, "currentSelection", json!({})).await
}

pub async fn call_list_projects(endpoint: Option<&str>) -> Result<Value, String> {
    call_endpoint(endpoint, "listProjects", json!({})).await
}

pub async fn call_find_definition(
    endpoint: Option<&str>,
    arguments: &Value,
) -> Result<Value, String> {
    let mut payload = Map::new();
    if let Some(symbol) = string_argument(arguments, "symbol") {
        payload.insert("symbol".to_string(), Value::String(symbol));
    }
    call_endpoint(endpoint, "findDefinition", Value::Object(payload)).await
}

pub async fn call_find_references(
    endpoint: Option<&str>,
    arguments: &Value,
) -> Result<Value, String> {
    let mut payload = Map::new();
    if let Some(symbol) = string_argument(arguments, "symbol") {
        payload.insert("symbol".to_string(), Value::String(symbol));
    }
    if let Some(max_results) = integer_argument(arguments, "maxResults", "max_results") {
        payload.insert("maxResults".to_string(), json!(max_results));
    }
    call_endpoint(endpoint, "findReferences", Value::Object(payload)).await
}

pub async fn call_get_error_list(endpoint: Option<&str>) -> Result<Value, String> {
    call_endpoint(endpoint, "getErrorList", json!({})).await
}

async fn call_endpoint(
    endpoint: Option<&str>,
    route: &str,
    payload: Value,
) -> Result<Value, String> {
    let Some(endpoint) = resolve_endpoint(endpoint) else {
        return Ok(json!({
            "ok": false,
            "status": "bridge_not_connected",
            "message": "VS Bridge not connected. Set CODEFORGE_VS_BRIDGE_URL to enable VS tools.",
            "source": "vsix",
            "endpoint": Value::Null,
            "route": route,
        }));
    };

    let url = format!("{}/{}", endpoint.trim_end_matches('/'), route);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(VS_BRIDGE_TIMEOUT_SECONDS))
        .build()
        .map_err(|error| format!("VS Bridge client creation failed: {error}"))?;

    let response = match client.post(&url).json(&payload).send().await {
        Ok(response) => response,
        Err(error) => {
            return Ok(json!({
                "ok": false,
                "status": "network_error",
                "message": format!("VS Bridge request failed: {error}"),
                "source": "vsix",
                "endpoint": endpoint,
                "route": route,
            }));
        }
    };
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let mut output = match serde_json::from_str::<Value>(&body) {
        Ok(Value::Object(object)) => Value::Object(object),
        Ok(value) => json!({
            "ok": status.is_success(),
            "message": "VS Bridge returned a non-object JSON response",
            "body": value,
        }),
        Err(error) => json!({
            "ok": false,
            "status": "response_parse_error",
            "message": format!("VS Bridge response parse failed: {error}"),
            "body": body,
        }),
    };

    augment_output(&mut output, &endpoint, route, status.as_u16());
    Ok(output)
}

fn augment_output(output: &mut Value, endpoint: &str, route: &str, http_status: u16) {
    let Some(object) = output.as_object_mut() else {
        return;
    };

    object
        .entry("source".to_string())
        .or_insert_with(|| json!("vsix"));
    object
        .entry("endpoint".to_string())
        .or_insert_with(|| json!(endpoint));
    object
        .entry("route".to_string())
        .or_insert_with(|| json!(route));
    object
        .entry("httpStatus".to_string())
        .or_insert_with(|| json!(http_status));

    if http_status >= 400 {
        object
            .entry("ok".to_string())
            .or_insert_with(|| json!(false));
        object
            .entry("status".to_string())
            .or_insert_with(|| json!("http_error"));
    }

    if !object.contains_key("count") {
        if let Some(count) = array_count(object, "projects")
            .or_else(|| array_count(object, "files"))
            .or_else(|| array_count(object, "diagnostics"))
            .or_else(|| array_count(object, "references"))
        {
            object.insert("count".to_string(), json!(count));
        }
    }
}

fn array_count(object: &Map<String, Value>, key: &str) -> Option<usize> {
    object.get(key).and_then(Value::as_array).map(Vec::len)
}

fn string_argument(arguments: &Value, key: &str) -> Option<String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn integer_argument(arguments: &Value, camel_key: &str, snake_key: &str) -> Option<u64> {
    arguments
        .get(camel_key)
        .or_else(|| arguments.get(snake_key))
        .and_then(Value::as_u64)
}
