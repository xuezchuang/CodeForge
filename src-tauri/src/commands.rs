use tauri::{AppHandle, Emitter, State};

use crate::agent_runner::{self, AgentRunInput};
use crate::app_state::{current_settings, lock_error, AppState};
use crate::code_link::{self, OpenCodeLinkResult, OpenFilePayload};
use crate::process_manager;
use crate::project_registry::{ProjectInput, ProjectSession};
use crate::tool_trace::{self, MockAgentRun, ToolTraceEvent, TraceEventType, TraceStatus};
use crate::vs_bridge_service;
use crate::vs_registry::{
    AppSettings, ProviderModel, SettingsInput, VSInstance, VSRegisterPayload,
    MINIMAX_OPENAI_BASE_URL,
};

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenVisualStudioResult {
    pub project: ProjectSession,
    pub process_id: u32,
    pub devenv_path: String,
    pub message: String,
}

#[tauri::command]
pub fn list_projects(state: State<'_, AppState>) -> Result<Vec<ProjectSession>, String> {
    let projects = state.projects.lock().map_err(|_| lock_error())?;
    Ok(projects.list())
}

#[tauri::command]
pub fn add_project(
    state: State<'_, AppState>,
    project_input: ProjectInput,
) -> Result<ProjectSession, String> {
    let mut projects = state.projects.lock().map_err(|_| lock_error())?;
    projects.add(project_input)
}

#[tauri::command]
pub fn update_project(
    state: State<'_, AppState>,
    project_id: String,
    project_input: ProjectInput,
) -> Result<ProjectSession, String> {
    let mut projects = state.projects.lock().map_err(|_| lock_error())?;
    projects.update(&project_id, project_input)
}

#[tauri::command]
pub fn delete_project(state: State<'_, AppState>, project_id: String) -> Result<(), String> {
    let mut projects = state.projects.lock().map_err(|_| lock_error())?;
    projects.delete(&project_id)
}

#[tauri::command]
pub fn get_project(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<ProjectSession, String> {
    let projects = state.projects.lock().map_err(|_| lock_error())?;
    projects.get(&project_id)
}

#[tauri::command]
pub fn open_visual_studio(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<OpenVisualStudioResult, String> {
    let settings = {
        let settings_store = state.settings.lock().map_err(|_| lock_error())?;
        current_settings(&settings_store)
    };

    let mut projects = state.projects.lock().map_err(|_| lock_error())?;
    let project = projects.get(&project_id)?;
    let opened = process_manager::open_visual_studio_process(&project, &settings)?;
    let updated_project = projects.set_vs_process(&project_id, opened.process_id)?;

    Ok(OpenVisualStudioResult {
        project: updated_project,
        process_id: opened.process_id,
        devenv_path: opened.devenv_path,
        message: "Visual Studio process started".to_string(),
    })
}

#[tauri::command]
pub fn register_vs_instance(
    state: State<'_, AppState>,
    payload: VSRegisterPayload,
) -> Result<VSInstance, String> {
    vs_bridge_service::register_vs_instance(&state, payload)
}

#[tauri::command]
pub fn unregister_vs_instance(
    state: State<'_, AppState>,
    instance_id: String,
) -> Result<VSInstance, String> {
    vs_bridge_service::unregister_vs_instance(&state, &instance_id)
}

#[tauri::command]
pub fn heartbeat_vs_instance(
    state: State<'_, AppState>,
    instance_id: String,
) -> Result<VSInstance, String> {
    vs_bridge_service::heartbeat_vs_instance(&state, &instance_id)
}

#[tauri::command]
pub fn list_vs_instances(state: State<'_, AppState>) -> Result<Vec<VSInstance>, String> {
    let registry = state.vs_registry.lock().map_err(|_| lock_error())?;
    Ok(registry.list())
}

#[tauri::command]
pub fn run_mock_agent(
    state: State<'_, AppState>,
    project_id: String,
    user_prompt: String,
) -> Result<MockAgentRun, String> {
    let project = {
        let projects = state.projects.lock().map_err(|_| lock_error())?;
        projects.get(&project_id)?
    };

    let run = tool_trace::create_mock_agent_run(&project, &user_prompt);
    let mut traces = state.traces.lock().map_err(|_| lock_error())?;
    traces.insert_task(run.task_id.clone(), run.traces.clone());
    Ok(run)
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallTestInput {
    pub project_id: String,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
}

#[tauri::command]
pub async fn run_agent(
    state: State<'_, AppState>,
    input: AgentRunInput,
) -> Result<MockAgentRun, String> {
    let project = {
        let projects = state.projects.lock().map_err(|_| lock_error())?;
        projects.get(&input.project_id)?
    };
    let settings = {
        let settings_store = state.settings.lock().map_err(|_| lock_error())?;
        current_settings(&settings_store)
    };

    let run = agent_runner::run_agent(&project, &settings, input).await?;
    let mut traces = state.traces.lock().map_err(|_| lock_error())?;
    traces.insert_task(run.task_id.clone(), run.traces.clone());
    Ok(run)
}

#[tauri::command]
pub async fn run_tool_call_test(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    input: ToolCallTestInput,
) -> Result<MockAgentRun, String> {
    let project = {
        let projects = state.projects.lock().map_err(|_| lock_error())?;
        projects.get(&input.project_id)?
    };
    let settings = {
        let settings_store = state.settings.lock().map_err(|_| lock_error())?;
        current_settings(&settings_store)
    };
    let trace_state = state.inner().clone();

    let run = agent_runner::run_tool_call_test(
        &project,
        &settings,
        input.provider_id.as_deref(),
        input.model_id.as_deref(),
        move |event| {
            if let Ok(mut traces) = trace_state.traces.lock() {
                traces.append_event(&event.task_id, event.clone());
            }
            let _ = app_handle.emit("agent_trace_event", event.clone());
        },
    )
    .await?;

    let mut traces = state.traces.lock().map_err(|_| lock_error())?;
    traces.insert_task(run.task_id.clone(), run.traces.clone());
    Ok(run)
}

#[tauri::command]
pub fn list_traces(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Vec<ToolTraceEvent>, String> {
    let traces = state.traces.lock().map_err(|_| lock_error())?;
    Ok(traces.list(&task_id))
}

#[tauri::command]
pub async fn open_code_link(
    state: State<'_, AppState>,
    project_id: String,
    raw_link: String,
    task_id: Option<String>,
) -> Result<OpenCodeLinkResult, String> {
    let project = {
        let projects = state.projects.lock().map_err(|_| lock_error())?;
        projects.get(&project_id)?
    };
    let payload = match code_link::parse_code_link(&project, &raw_link) {
        Ok(payload) => payload,
        Err(error) => {
            record_open_code_link_trace(
                &state,
                task_id.as_deref(),
                &project_id,
                &raw_link,
                None,
                project.vs_bridge_endpoint.as_deref(),
                TraceStatus::Failed,
                &error,
            )?;
            return Err(error);
        }
    };

    let endpoint = match project
        .vs_bridge_endpoint
        .as_deref()
        .filter(|endpoint| !endpoint.trim().is_empty())
    {
        Some(endpoint) => endpoint,
        None => {
            let error = "VS Bridge not connected. Open Visual Studio and wait for Bridge Connected before using code links.".to_string();
            record_open_code_link_trace(
                &state,
                task_id.as_deref(),
                &project_id,
                &raw_link,
                Some(&payload),
                None,
                TraceStatus::Failed,
                &error,
            )?;
            return Err(error);
        }
    };

    if let Err(error) = code_link::call_vs_open_file(endpoint, &payload).await {
        if error.contains("status=network_error") {
            let mut projects = state.projects.lock().map_err(|_| lock_error())?;
            let _ = projects.clear_vs_bridge(&project_id);
        }
        record_open_code_link_trace(
            &state,
            task_id.as_deref(),
            &project_id,
            &raw_link,
            Some(&payload),
            Some(endpoint),
            TraceStatus::Failed,
            &error,
        )?;
        return Err(error);
    }

    let message = "Opened in Visual Studio".to_string();
    let trace_event = record_open_code_link_trace(
        &state,
        task_id.as_deref(),
        &project_id,
        &raw_link,
        Some(&payload),
        Some(endpoint),
        TraceStatus::Success,
        &message,
    )?;

    Ok(OpenCodeLinkResult {
        resolved_path: payload.path,
        line: payload.line,
        column: payload.column,
        bridge_called: true,
        fallback_started_vs: false,
        message,
        trace_event,
    })
}

fn record_open_code_link_trace(
    state: &AppState,
    task_id: Option<&str>,
    project_id: &str,
    raw_link: &str,
    payload: Option<&OpenFilePayload>,
    endpoint: Option<&str>,
    status: TraceStatus,
    summary: &str,
) -> Result<Option<ToolTraceEvent>, String> {
    let Some(task_id) = task_id.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };

    let mut traces = state.traces.lock().map_err(|_| lock_error())?;
    let step_index = traces.next_step_index(task_id);
    let is_success = matches!(&status, TraceStatus::Success);
    let event = tool_trace::tool_event(
        task_id,
        step_index,
        if is_success {
            TraceEventType::ToolResult
        } else {
            TraceEventType::Error
        },
        Some("open_code_link".to_string()),
        if is_success {
            "open_code_link success".to_string()
        } else {
            "open_code_link failed".to_string()
        },
        Some(serde_json::json!({
            "projectId": project_id,
            "rawLink": raw_link,
            "endpoint": endpoint,
        })),
        Some(serde_json::json!({
            "path": payload.map(|payload| payload.path.clone()),
            "line": payload.map(|payload| payload.line),
            "column": payload.and_then(|payload| payload.column),
            "endpoint": endpoint,
            "message": summary,
        })),
        Some(summary.to_string()),
        status,
        0,
    );
    traces.append_event(task_id, event.clone());
    Ok(Some(event))
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<AppSettings, String> {
    let settings = state.settings.lock().map_err(|_| lock_error())?;
    Ok(settings.current())
}

#[tauri::command]
pub fn update_settings(
    state: State<'_, AppState>,
    settings: SettingsInput,
) -> Result<AppSettings, String> {
    let mut settings_store = state.settings.lock().map_err(|_| lock_error())?;
    settings_store.update(settings)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct MiniMaxModelListResponse {
    data: Vec<MiniMaxModel>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct MiniMaxModel {
    id: String,
    created: Option<i64>,
    owned_by: Option<String>,
}

#[tauri::command]
pub async fn fetch_minimax_models(api_key: String) -> Result<Vec<ProviderModel>, String> {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err("MiniMax API key is required.".to_string());
    }

    let url = format!("{MINIMAX_OPENAI_BASE_URL}/models");
    let response = reqwest::Client::new()
        .get(&url)
        .bearer_auth(api_key)
        .send()
        .await
        .map_err(|error| format!("MiniMax model list request failed: {error}"))?;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "MiniMax model list request failed. status={}; body={}",
            status.as_u16(),
            body
        ));
    }

    let parsed = serde_json::from_str::<MiniMaxModelListResponse>(&body)
        .map_err(|error| format!("MiniMax model list response parse failed: {error}"))?;

    Ok(parsed
        .data
        .into_iter()
        .map(|model| ProviderModel {
            name: model.id.clone(),
            id: model.id,
            enabled: false,
            owned_by: model.owned_by,
            created: model.created,
        })
        .collect())
}
