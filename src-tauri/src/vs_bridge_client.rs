use std::time::Duration;

use serde_json::{json, Map, Value};

const VS_BRIDGE_TIMEOUT_SECONDS: u64 = 5;

pub async fn call_vs_current_solution(endpoint: Option<&str>) -> Result<Value, String> {
    call_vs_endpoint(endpoint, "currentSolution", json!({})).await
}

pub async fn call_vs_current_document(endpoint: Option<&str>) -> Result<Value, String> {
    call_vs_endpoint(endpoint, "currentDocument", json!({})).await
}

pub async fn call_vs_current_selection(endpoint: Option<&str>) -> Result<Value, String> {
    call_vs_endpoint(endpoint, "currentSelection", json!({})).await
}

pub async fn call_vs_list_projects(endpoint: Option<&str>) -> Result<Value, String> {
    call_vs_endpoint(endpoint, "listProjects", json!({})).await
}

pub async fn call_vs_list_project_files(
    endpoint: Option<&str>,
    arguments: &Value,
) -> Result<Value, String> {
    let mut payload = Map::new();
    if let Some(project_name) = string_argument(arguments, "projectName", "project_name") {
        payload.insert("projectName".to_string(), Value::String(project_name));
    }
    if let Some(project_unique_name) =
        string_argument(arguments, "projectUniqueName", "project_unique_name")
    {
        payload.insert(
            "projectUniqueName".to_string(),
            Value::String(project_unique_name),
        );
    }
    if let Some(max_files) = integer_argument(arguments, "maxFiles", "max_files") {
        payload.insert("maxFiles".to_string(), json!(max_files));
    }

    call_vs_endpoint(endpoint, "listProjectFiles", Value::Object(payload)).await
}

pub async fn call_vs_get_error_list(endpoint: Option<&str>) -> Result<Value, String> {
    call_vs_endpoint(endpoint, "getErrorList", json!({})).await
}

async fn call_vs_endpoint(
    endpoint: Option<&str>,
    route: &str,
    payload: Value,
) -> Result<Value, String> {
    let Some(endpoint) = endpoint.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(json!({
            "ok": false,
            "status": "bridge_not_connected",
            "message": "VS Bridge not connected",
            "source": "vsix",
            "endpoint": null,
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

    augment_output(&mut output, endpoint, route, status.as_u16());
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
        {
            object.insert("count".to_string(), json!(count));
        }
    }
}

fn array_count(object: &Map<String, Value>, key: &str) -> Option<usize> {
    object.get(key).and_then(Value::as_array).map(Vec::len)
}

fn string_argument(arguments: &Value, camel_key: &str, snake_key: &str) -> Option<String> {
    arguments
        .get(camel_key)
        .or_else(|| arguments.get(snake_key))
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
