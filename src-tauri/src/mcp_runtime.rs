use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use rmcp::model::{CallToolRequestParams, CallToolResult, Tool};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::child_process::TokioChildProcess;
use rmcp::transport::StreamableHttpClientTransport;
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};
use tokio::process::Command;

use crate::tool_interface::ToolOutput;

pub const MCP_LIST_SERVERS_TOOL_NAME: &str = "mcp_list_servers";
pub const MCP_CONNECT_SERVER_TOOL_NAME: &str = "mcp_connect_server";
pub const MCP_DISCONNECT_SERVER_TOOL_NAME: &str = "mcp_disconnect_server";
pub const MCP_LIST_TOOLS_TOOL_NAME: &str = "mcp_list_tools";

const MCP_TOOL_PREFIX: &str = "mcp__";
const MAX_TOOL_NAME_BYTES: usize = 64;
const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(60);
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Clone)]
pub struct McpRuntime {
    config_path: Option<PathBuf>,
    config_error: Option<String>,
    servers: HashMap<String, McpServerState>,
}

impl std::fmt::Debug for McpRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpRuntime")
            .field("config_path", &self.config_path)
            .field("config_error", &self.config_error)
            .field("server_count", &self.servers.len())
            .field("tool_count", &self.tool_count())
            .finish()
    }
}

#[derive(Clone)]
struct McpServerState {
    config: McpServerConfig,
    status: McpServerStatus,
    tools: Vec<McpToolDefinition>,
    client: Option<Arc<McpClient>>,
    last_error: Option<String>,
    connected_at: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpServerStatus {
    Configured,
    Connecting,
    Connected,
    Failed,
    Disabled,
    Disconnected,
}

impl McpServerStatus {
    fn as_str(self) -> &'static str {
        match self {
            McpServerStatus::Configured => "configured",
            McpServerStatus::Connecting => "connecting",
            McpServerStatus::Connected => "connected",
            McpServerStatus::Failed => "failed",
            McpServerStatus::Disabled => "disabled",
            McpServerStatus::Disconnected => "disconnected",
        }
    }
}

#[derive(Clone, Debug)]
pub struct McpToolDefinition {
    pub server_name: String,
    pub raw_tool_name: String,
    pub model_tool_name: String,
    pub definition: Value,
    pub supports_parallel_tool_calls: bool,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolSummary {
    pub server_id: String,
    pub server_name: String,
    pub name: String,
    pub raw_name: String,
    pub description: String,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerSummary {
    pub id: String,
    pub name: String,
    pub status: String,
    pub enabled: bool,
    pub auto_connect: bool,
    pub required: bool,
    pub tool_count: usize,
    pub error: Option<String>,
    pub transport: String,
    pub url: Option<String>,
    pub connected_at: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpInventorySummary {
    pub config_path: Option<String>,
    pub config_error: Option<String>,
    pub servers: Vec<McpServerSummary>,
    pub tools: Vec<McpToolSummary>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerActionResult {
    pub server_id: String,
    pub name: String,
    pub status: String,
    pub tool_count: usize,
    pub message: String,
    pub error: Option<String>,
}

#[derive(Clone)]
struct McpClient {
    service: Arc<RunningService<RoleClient, ()>>,
    tool_timeout: Duration,
}

#[derive(Clone, Debug, Deserialize)]
pub struct McpConfigFile {
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct McpServerConfig {
    pub name: Option<String>,
    pub transport: Option<String>,
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub env_vars: Vec<McpServerEnvVar>,
    pub cwd: Option<PathBuf>,
    pub url: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub auto_connect: bool,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub supports_parallel_tool_calls: bool,
    pub approval_mode: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_duration_secs")]
    pub startup_timeout_sec: Option<Duration>,
    pub startup_timeout_ms: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_duration_secs")]
    pub tool_timeout_sec: Option<Duration>,
    pub enabled_tools: Option<Vec<String>>,
    pub disabled_tools: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum McpServerEnvVar {
    Name(String),
    Config { name: String },
}

impl McpServerEnvVar {
    fn name(&self) -> &str {
        match self {
            McpServerEnvVar::Name(name) => name,
            McpServerEnvVar::Config { name } => name,
        }
    }
}

impl McpRuntime {
    pub fn load_from_default_config() -> Self {
        let config_path = default_config_path();
        let Some(path) = config_path else {
            return empty_runtime(None);
        };
        if !path.exists() {
            return empty_runtime(Some(path));
        }
        match read_config_file(&path) {
            Ok(config) => Self::from_config_with_path(Some(path), config),
            Err(error) => Self {
                config_path: Some(path),
                config_error: Some(error),
                servers: HashMap::new(),
            },
        }
    }

    pub fn from_config(config: McpConfigFile) -> Self {
        Self::from_config_with_path(None, config)
    }

    fn from_config_with_path(config_path: Option<PathBuf>, config: McpConfigFile) -> Self {
        let servers = config
            .mcp_servers
            .into_iter()
            .map(|(server_id, server)| {
                let status = if server.enabled {
                    McpServerStatus::Configured
                } else {
                    McpServerStatus::Disabled
                };
                (
                    server_id,
                    McpServerState {
                        config: server,
                        status,
                        tools: Vec::new(),
                        client: None,
                        last_error: None,
                        connected_at: None,
                    },
                )
            })
            .collect();

        Self {
            config_path,
            config_error: None,
            servers,
        }
    }

    pub fn inventory(&self) -> McpInventorySummary {
        McpInventorySummary {
            config_path: self
                .config_path
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            config_error: self.config_error.clone(),
            servers: self.server_summaries(),
            tools: self.tool_summaries(),
        }
    }

    pub fn server_summaries(&self) -> Vec<McpServerSummary> {
        self.sorted_server_ids()
            .into_iter()
            .filter_map(|server_id| {
                let state = self.servers.get(&server_id)?;
                Some(McpServerSummary {
                    id: server_id.clone(),
                    name: server_display_name(&server_id, &state.config),
                    status: state.status.as_str().to_string(),
                    enabled: state.config.enabled,
                    auto_connect: state.config.auto_connect,
                    required: state.config.required,
                    tool_count: state.tools.len(),
                    error: state.last_error.clone(),
                    transport: server_transport(&state.config),
                    url: state.config.url.clone(),
                    connected_at: state.connected_at.clone(),
                })
            })
            .collect()
    }

    pub fn tool_definitions(&self) -> Vec<Value> {
        self.sorted_connected_tools()
            .into_iter()
            .map(|tool| tool.definition.clone())
            .collect()
    }

    pub fn tool_count(&self) -> usize {
        self.servers.values().map(|server| server.tools.len()).sum()
    }

    pub fn tool_summaries(&self) -> Vec<McpToolSummary> {
        self.sorted_connected_tools()
            .into_iter()
            .map(|tool| {
                let server_name = self
                    .servers
                    .get(&tool.server_name)
                    .map(|state| server_display_name(&tool.server_name, &state.config))
                    .unwrap_or_else(|| tool.server_name.clone());
                McpToolSummary {
                    server_id: tool.server_name.clone(),
                    server_name,
                    name: tool.model_tool_name.clone(),
                    raw_name: tool.raw_tool_name.clone(),
                    description: tool
                        .definition
                        .get("function")
                        .and_then(|function| function.get("description"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                }
            })
            .collect()
    }

    pub fn server_names(&self) -> Vec<String> {
        self.sorted_server_ids()
            .into_iter()
            .filter(|server_id| {
                self.servers
                    .get(server_id)
                    .is_some_and(|server| server.status == McpServerStatus::Connected)
            })
            .collect()
    }

    pub fn is_mcp_tool(&self, name: &str) -> bool {
        self.servers
            .values()
            .flat_map(|server| server.tools.iter())
            .any(|tool| tool.model_tool_name == name)
    }

    pub fn supports_parallel_tool_calls(&self, name: &str) -> bool {
        self.servers
            .values()
            .flat_map(|server| server.tools.iter())
            .find(|tool| tool.model_tool_name == name)
            .is_some_and(|tool| tool.supports_parallel_tool_calls)
    }

    pub async fn connect_auto_connect_servers(&mut self) -> Vec<McpServerActionResult> {
        let server_ids = self
            .sorted_server_ids()
            .into_iter()
            .filter(|server_id| {
                self.servers.get(server_id).is_some_and(|server| {
                    server.config.enabled
                        && server.config.auto_connect
                        && server.status != McpServerStatus::Connected
                })
            })
            .collect::<Vec<_>>();
        let mut results = Vec::with_capacity(server_ids.len());
        for server_id in server_ids {
            results.push(self.connect_server_by_id(&server_id).await);
        }
        results
    }

    pub async fn connect_server_by_id(&mut self, server_id: &str) -> McpServerActionResult {
        let Some(state) = self.servers.get_mut(server_id) else {
            return action_result(
                server_id,
                server_id,
                McpServerStatus::Failed,
                0,
                format!("Unknown MCP server `{server_id}`"),
                Some(format!("Unknown MCP server `{server_id}`")),
            );
        };
        let display_name = server_display_name(server_id, &state.config);

        if !state.config.enabled {
            state.status = McpServerStatus::Disabled;
            state.last_error = Some(format!("MCP server `{server_id}` is disabled"));
            return action_result(
                server_id,
                &display_name,
                McpServerStatus::Disabled,
                0,
                format!("MCP server `{server_id}` is disabled."),
                state.last_error.clone(),
            );
        }

        if state.status == McpServerStatus::Connected {
            return action_result(
                server_id,
                &display_name,
                McpServerStatus::Connected,
                state.tools.len(),
                "MCP server is already connected. Tools are available in model requests."
                    .to_string(),
                None,
            );
        }

        let config = state.config.clone();
        state.status = McpServerStatus::Connecting;
        state.last_error = None;
        state.tools.clear();
        state.client = None;
        state.connected_at = None;

        let client = match connect_server(server_id, &config).await {
            Ok(client) => client,
            Err(error) => {
                self.mark_server_failed(server_id, &error);
                return action_result(
                    server_id,
                    &display_name,
                    McpServerStatus::Failed,
                    0,
                    format!("MCP server `{server_id}` failed to connect."),
                    Some(error),
                );
            }
        };

        let server_tools = match list_server_tools(server_id, &config, &client).await {
            Ok(tools) => tools,
            Err(error) => {
                self.mark_server_failed(server_id, &error);
                return action_result(
                    server_id,
                    &display_name,
                    McpServerStatus::Failed,
                    0,
                    format!("MCP server `{server_id}` connected but tools/list failed."),
                    Some(error),
                );
            }
        };

        let mut used_model_tool_names = self.used_model_tool_names(Some(server_id));
        let definitions = server_tools
            .into_iter()
            .map(|tool| {
                let raw_tool_name = tool.name.to_string();
                let model_tool_name =
                    unique_model_tool_name(server_id, &raw_tool_name, &mut used_model_tool_names);
                let definition = openai_tool_definition(&model_tool_name, &tool);
                McpToolDefinition {
                    server_name: server_id.to_string(),
                    raw_tool_name,
                    model_tool_name,
                    definition,
                    supports_parallel_tool_calls: config.supports_parallel_tool_calls,
                }
            })
            .collect::<Vec<_>>();
        let tool_count = definitions.len();

        if let Some(state) = self.servers.get_mut(server_id) {
            state.status = McpServerStatus::Connected;
            state.tools = definitions;
            state.client = Some(Arc::new(client));
            state.last_error = None;
            state.connected_at = Some(Utc::now().to_rfc3339());
        }

        action_result(
            server_id,
            &display_name,
            McpServerStatus::Connected,
            tool_count,
            "MCP server connected. Tools will be available from the next model request."
                .to_string(),
            None,
        )
    }

    pub fn disconnect_server_by_id(&mut self, server_id: &str) -> McpServerActionResult {
        let Some(state) = self.servers.get_mut(server_id) else {
            return action_result(
                server_id,
                server_id,
                McpServerStatus::Failed,
                0,
                format!("Unknown MCP server `{server_id}`"),
                Some(format!("Unknown MCP server `{server_id}`")),
            );
        };
        let display_name = server_display_name(server_id, &state.config);
        state.tools.clear();
        state.client = None;
        state.connected_at = None;
        state.last_error = None;
        state.status = if state.config.enabled {
            McpServerStatus::Disconnected
        } else {
            McpServerStatus::Disabled
        };
        action_result(
            server_id,
            &display_name,
            state.status,
            0,
            "MCP server disconnected. Tools have been removed from future model requests."
                .to_string(),
            None,
        )
    }

    pub async fn execute_management_tool(&mut self, name: &str, arguments: &Value) -> ToolOutput {
        let started = Instant::now();
        match name {
            MCP_LIST_SERVERS_TOOL_NAME => {
                let output = json!({ "servers": self.server_summaries() });
                ToolOutput::ok_with_summary(
                    output,
                    started.elapsed().as_millis() as u64,
                    format!("Listed {} MCP server(s)", self.servers.len()),
                )
            }
            MCP_CONNECT_SERVER_TOOL_NAME => {
                let Some(server_id) = server_id_argument(arguments) else {
                    return ToolOutput::error(
                        "mcp_connect_server requires server_id".to_string(),
                        started.elapsed().as_millis() as u64,
                    );
                };
                let result = self.connect_server_by_id(server_id).await;
                let summary = result.message.clone();
                let output = serde_json::to_value(result).unwrap_or_else(|_| json!({}));
                ToolOutput::ok_with_summary(output, started.elapsed().as_millis() as u64, summary)
            }
            MCP_DISCONNECT_SERVER_TOOL_NAME => {
                let Some(server_id) = server_id_argument(arguments) else {
                    return ToolOutput::error(
                        "mcp_disconnect_server requires server_id".to_string(),
                        started.elapsed().as_millis() as u64,
                    );
                };
                let result = self.disconnect_server_by_id(server_id);
                let summary = result.message.clone();
                let output = serde_json::to_value(result).unwrap_or_else(|_| json!({}));
                ToolOutput::ok_with_summary(output, started.elapsed().as_millis() as u64, summary)
            }
            MCP_LIST_TOOLS_TOOL_NAME => {
                let Some(server_id) = server_id_argument(arguments) else {
                    return ToolOutput::error(
                        "mcp_list_tools requires server_id".to_string(),
                        started.elapsed().as_millis() as u64,
                    );
                };
                let Some(state) = self.servers.get(server_id) else {
                    return ToolOutput::error(
                        format!("Unknown MCP server `{server_id}`"),
                        started.elapsed().as_millis() as u64,
                    );
                };
                let tools = state
                    .tools
                    .iter()
                    .map(|tool| {
                        json!({
                            "name": tool.model_tool_name,
                            "rawName": tool.raw_tool_name,
                            "description": tool.definition
                                .get("function")
                                .and_then(|function| function.get("description"))
                                .and_then(Value::as_str)
                                .unwrap_or(""),
                        })
                    })
                    .collect::<Vec<_>>();
                let output = json!({
                    "server_id": server_id,
                    "status": state.status.as_str(),
                    "tools": tools,
                });
                ToolOutput::ok_with_summary(
                    output,
                    started.elapsed().as_millis() as u64,
                    format!(
                        "MCP server `{server_id}` has {} registered tool(s)",
                        state.tools.len()
                    ),
                )
            }
            _ => ToolOutput::error(format!("Unknown MCP management tool: {name}"), 0),
        }
    }

    pub async fn call_tool(&self, model_tool_name: &str, arguments: &Value) -> ToolOutput {
        let started = Instant::now();
        let Some((tool, client)) = self.lookup_tool_and_client(model_tool_name) else {
            return ToolOutput::error(format!("Unknown MCP tool: {model_tool_name}"), 0);
        };

        let arguments = match arguments {
            Value::Object(map) => Some(map.clone()),
            Value::Null => None,
            _ => {
                return ToolOutput::error(
                    "MCP tool arguments must be a JSON object".to_string(),
                    started.elapsed().as_millis() as u64,
                );
            }
        };
        let mut params = CallToolRequestParams::new(tool.raw_tool_name.clone());
        if let Some(arguments) = arguments {
            params = params.with_arguments(arguments);
        }
        let result =
            tokio::time::timeout(client.tool_timeout, client.service.peer().call_tool(params))
                .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;

        match result {
            Ok(Ok(result)) => call_tool_result_to_output(
                &tool.server_name,
                &tool.raw_tool_name,
                result,
                elapsed_ms,
            ),
            Ok(Err(error)) => ToolOutput::error(
                format!(
                    "MCP tool call failed for `{}/{}`: {error}",
                    tool.server_name, tool.raw_tool_name
                ),
                elapsed_ms,
            ),
            Err(_) => ToolOutput::timeout(elapsed_ms),
        }
    }

    fn lookup_tool_and_client(
        &self,
        model_tool_name: &str,
    ) -> Option<(McpToolDefinition, Arc<McpClient>)> {
        for server in self.servers.values() {
            let Some(client) = server.client.as_ref() else {
                continue;
            };
            if let Some(tool) = server
                .tools
                .iter()
                .find(|tool| tool.model_tool_name == model_tool_name)
            {
                return Some((tool.clone(), client.clone()));
            }
        }
        None
    }

    fn mark_server_failed(&mut self, server_id: &str, error: &str) {
        if let Some(state) = self.servers.get_mut(server_id) {
            state.status = McpServerStatus::Failed;
            state.tools.clear();
            state.client = None;
            state.connected_at = None;
            state.last_error = Some(error.to_string());
        }
    }

    fn used_model_tool_names(&self, except_server_id: Option<&str>) -> HashSet<String> {
        self.servers
            .iter()
            .filter(|(server_id, _)| except_server_id != Some(server_id.as_str()))
            .flat_map(|(_, server)| server.tools.iter())
            .map(|tool| tool.model_tool_name.clone())
            .collect()
    }

    fn sorted_server_ids(&self) -> Vec<String> {
        let mut ids = self.servers.keys().cloned().collect::<Vec<_>>();
        ids.sort();
        ids
    }

    fn sorted_connected_tools(&self) -> Vec<&McpToolDefinition> {
        let mut tools = self
            .sorted_server_ids()
            .into_iter()
            .filter_map(|server_id| self.servers.get(&server_id))
            .filter(|server| server.status == McpServerStatus::Connected)
            .flat_map(|server| server.tools.iter())
            .collect::<Vec<_>>();
        tools.sort_by(|left, right| {
            left.model_tool_name
                .cmp(&right.model_tool_name)
                .then(left.raw_tool_name.cmp(&right.raw_tool_name))
        });
        tools
    }
}

pub fn is_mcp_management_tool(name: &str) -> bool {
    matches!(
        name,
        MCP_LIST_SERVERS_TOOL_NAME
            | MCP_CONNECT_SERVER_TOOL_NAME
            | MCP_DISCONNECT_SERVER_TOOL_NAME
            | MCP_LIST_TOOLS_TOOL_NAME
    )
}

async fn connect_server(server_name: &str, server: &McpServerConfig) -> Result<McpClient, String> {
    if let Some(url) = server.url.as_deref().filter(|url| !url.trim().is_empty()) {
        return connect_http_server(server_name, server, url).await;
    }
    connect_stdio_server(server_name, server).await
}

async fn connect_http_server(
    server_name: &str,
    server: &McpServerConfig,
    url: &str,
) -> Result<McpClient, String> {
    let startup_timeout = startup_timeout(server);
    let transport = StreamableHttpClientTransport::from_uri(url.to_string());
    let service = tokio::time::timeout(startup_timeout, rmcp::serve_client((), transport))
        .await
        .map_err(|_| {
            format!("MCP server `{server_name}` timed out during startup after {startup_timeout:?}")
        })?
        .map_err(|error| format!("MCP server `{server_name}` handshake failed: {error}"))?;

    Ok(McpClient {
        service: Arc::new(service),
        tool_timeout: tool_timeout(server),
    })
}

async fn connect_stdio_server(
    server_name: &str,
    server: &McpServerConfig,
) -> Result<McpClient, String> {
    let command = server
        .command
        .as_deref()
        .filter(|command| !command.trim().is_empty())
        .ok_or_else(|| format!("MCP server `{server_name}` is missing command"))?;
    let mut process = Command::new(command);
    hide_child_console(&mut process);
    process.args(&server.args);
    if let Some(cwd) = &server.cwd {
        process.current_dir(cwd);
    }
    process.envs(&server.env);
    for env_var in &server.env_vars {
        if let Ok(value) = std::env::var(env_var.name()) {
            process.env(env_var.name(), value);
        }
    }
    let (transport, _stderr) = TokioChildProcess::builder(process)
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("MCP server `{server_name}` failed to start: {error}"))?;

    let startup_timeout = startup_timeout(server);
    let service = tokio::time::timeout(startup_timeout, rmcp::serve_client((), transport))
        .await
        .map_err(|_| {
            format!("MCP server `{server_name}` timed out during startup after {startup_timeout:?}")
        })?
        .map_err(|error| format!("MCP server `{server_name}` handshake failed: {error}"))?;

    Ok(McpClient {
        service: Arc::new(service),
        tool_timeout: tool_timeout(server),
    })
}

fn hide_child_console(command: &mut Command) {
    #[cfg(windows)]
    {
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}

fn startup_timeout(server: &McpServerConfig) -> Duration {
    server
        .startup_timeout_sec
        .or_else(|| server.startup_timeout_ms.map(Duration::from_millis))
        .unwrap_or(DEFAULT_STARTUP_TIMEOUT)
}

fn tool_timeout(server: &McpServerConfig) -> Duration {
    server.tool_timeout_sec.unwrap_or(DEFAULT_TOOL_TIMEOUT)
}

async fn list_server_tools(
    server_name: &str,
    server: &McpServerConfig,
    client: &McpClient,
) -> Result<Vec<Tool>, String> {
    let enabled = server
        .enabled_tools
        .as_ref()
        .map(|tools| tools.iter().cloned().collect::<HashSet<_>>());
    let disabled = server
        .disabled_tools
        .as_ref()
        .map(|tools| tools.iter().cloned().collect::<HashSet<_>>())
        .unwrap_or_default();
    let tools = tokio::time::timeout(client.tool_timeout, client.service.peer().list_all_tools())
        .await
        .map_err(|_| format!("MCP server `{server_name}` tools/list timed out"))?
        .map_err(|error| format!("MCP server `{server_name}` tools/list failed: {error}"))?;

    Ok(tools
        .into_iter()
        .filter(|tool| {
            let name = tool.name.as_ref();
            enabled
                .as_ref()
                .is_none_or(|enabled| enabled.contains(name))
                && !disabled.contains(name)
        })
        .collect())
}

fn openai_tool_definition(model_tool_name: &str, tool: &Tool) -> Value {
    let mut parameters = Value::Object(tool.input_schema.as_ref().clone());
    if !parameters.is_object() {
        parameters = json!({ "type": "object", "properties": {} });
    }
    json!({
        "type": "function",
        "function": {
            "name": model_tool_name,
            "description": tool.description.as_deref().unwrap_or("MCP tool"),
            "parameters": parameters,
        }
    })
}

fn unique_model_tool_name(
    server_name: &str,
    raw_tool_name: &str,
    used_model_tool_names: &mut HashSet<String>,
) -> String {
    let base = sanitize_tool_name(&format!("{MCP_TOOL_PREFIX}{server_name}__{raw_tool_name}"));
    if used_model_tool_names.insert(base.clone()) {
        return base;
    }

    let mut suffix = 2usize;
    loop {
        let suffix_text = format!("_{suffix}");
        let prefix_limit = MAX_TOOL_NAME_BYTES.saturating_sub(suffix_text.len());
        let prefix = truncate_tool_name(&base, prefix_limit);
        let candidate = format!("{prefix}{suffix_text}");
        if used_model_tool_names.insert(candidate.clone()) {
            return candidate;
        }
        suffix += 1;
    }
}

fn sanitize_tool_name(name: &str) -> String {
    let mut sanitized = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            sanitized.push(ch);
        } else {
            sanitized.push('_');
        }
    }
    let sanitized = sanitized.trim_matches('_');
    let sanitized = if sanitized.is_empty() {
        "mcp_tool"
    } else {
        sanitized
    };
    truncate_tool_name(sanitized, MAX_TOOL_NAME_BYTES)
}

fn truncate_tool_name(name: &str, max_bytes: usize) -> String {
    if name.len() <= max_bytes {
        return name.to_string();
    }
    name.chars().take(max_bytes).collect()
}

fn call_tool_result_to_output(
    server_name: &str,
    raw_tool_name: &str,
    result: CallToolResult,
    elapsed_ms: u64,
) -> ToolOutput {
    let output = json!({
        "server": server_name,
        "tool": raw_tool_name,
        "content": result.content,
        "structuredContent": result.structured_content,
        "isError": result.is_error,
        "meta": result.meta,
    });
    let summary = mcp_result_summary(server_name, raw_tool_name, &output);
    if result.is_error.unwrap_or(false) {
        ToolOutput::error(summary, elapsed_ms)
    } else {
        ToolOutput::ok_with_summary(output, elapsed_ms, summary)
    }
}

fn mcp_result_summary(server_name: &str, raw_tool_name: &str, output: &Value) -> String {
    let content_count = output
        .get("content")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_default();
    format!("MCP {server_name}/{raw_tool_name} returned {content_count} content item(s)")
}

fn action_result(
    server_id: &str,
    name: &str,
    status: McpServerStatus,
    tool_count: usize,
    message: String,
    error: Option<String>,
) -> McpServerActionResult {
    McpServerActionResult {
        server_id: server_id.to_string(),
        name: name.to_string(),
        status: status.as_str().to_string(),
        tool_count,
        message,
        error,
    }
}

fn server_id_argument(arguments: &Value) -> Option<&str> {
    arguments
        .get("server_id")
        .or_else(|| arguments.get("serverId"))
        .or_else(|| arguments.get("id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn server_display_name(server_id: &str, config: &McpServerConfig) -> String {
    config
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(server_id)
        .to_string()
}

fn server_transport(config: &McpServerConfig) -> String {
    if let Some(transport) = config
        .transport
        .as_deref()
        .map(str::trim)
        .filter(|transport| !transport.is_empty())
    {
        return transport.to_string();
    }
    if config
        .url
        .as_deref()
        .is_some_and(|url| !url.trim().is_empty())
    {
        "http".to_string()
    } else {
        "stdio".to_string()
    }
}

fn empty_runtime(config_path: Option<PathBuf>) -> McpRuntime {
    McpRuntime {
        config_path,
        config_error: None,
        servers: HashMap::new(),
    }
}

fn read_config_file(config_path: &PathBuf) -> Result<McpConfigFile, String> {
    let contents = std::fs::read_to_string(config_path).map_err(|error| {
        format!(
            "MCP config read failed {}: {error}",
            config_path.to_string_lossy()
        )
    })?;
    toml_edit::de::from_str(&contents).map_err(|error| {
        format!(
            "MCP config parse failed {}: {error}",
            config_path.to_string_lossy()
        )
    })
}

fn default_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".codeforge").join("config.toml"))
}

fn default_enabled() -> bool {
    true
}

fn deserialize_optional_duration_secs<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
where
    D: Deserializer<'de>,
{
    let seconds = Option::<f64>::deserialize(deserializer)?;
    seconds
        .map(|seconds| Duration::try_from_secs_f64(seconds).map_err(serde::de::Error::custom))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_config(command: Option<&str>, url: Option<String>) -> McpServerConfig {
        McpServerConfig {
            name: None,
            transport: None,
            command: command.map(str::to_string),
            args: Vec::new(),
            env: HashMap::new(),
            env_vars: Vec::new(),
            cwd: None,
            url,
            enabled: true,
            auto_connect: false,
            required: false,
            supports_parallel_tool_calls: false,
            approval_mode: None,
            startup_timeout_sec: None,
            startup_timeout_ms: None,
            tool_timeout_sec: None,
            enabled_tools: None,
            disabled_tools: None,
        }
    }

    #[test]
    fn sanitize_tool_names_use_mcp_prefix_and_safe_chars() {
        let mut used = HashSet::new();
        let name = unique_model_tool_name("ue server", "Actor.Spawn", &mut used);

        assert_eq!(name, "mcp__ue_server__Actor_Spawn");
    }

    #[test]
    fn duplicate_truncated_tool_names_keep_unique_suffix() {
        let mut used = HashSet::new();
        let raw_name = "x".repeat(90);
        let first = unique_model_tool_name("ue", &raw_name, &mut used);
        let second = unique_model_tool_name("ue", &raw_name, &mut used);

        assert_ne!(first, second);
        assert_eq!(first.len(), MAX_TOOL_NAME_BYTES);
        assert_eq!(second.len(), MAX_TOOL_NAME_BYTES);
        assert!(second.ends_with("_2"));
    }

    #[test]
    fn disabled_tools_are_not_enabled_by_filter() {
        let config = McpServerConfig {
            enabled_tools: Some(vec!["allowed".to_string()]),
            disabled_tools: Some(vec!["blocked".to_string()]),
            ..server_config(Some("dummy"), None)
        };

        assert!(config
            .enabled_tools
            .as_ref()
            .unwrap()
            .contains(&"allowed".to_string()));
        assert!(config
            .disabled_tools
            .as_ref()
            .unwrap()
            .contains(&"blocked".to_string()));
    }

    #[test]
    fn parses_stdio_mcp_server_config() {
        let config: McpConfigFile = toml_edit::de::from_str(
            r#"
            [mcp_servers.ue]
            name = "Unreal Engine MCP"
            transport = "stdio"
            command = "node"
            args = ["ue-mcp.js"]
            startup_timeout_sec = 0.25
            env_vars = ["UE_PROJECT_ROOT", { name = "UE_PLUGIN_PATH", source = "local" }]
            startup_timeout_ms = 500
            tool_timeout_sec = 120.5
            auto_connect = false
            approval_mode = "prompt"
            supports_parallel_tool_calls = false
            "#,
        )
        .unwrap();

        let ue = config.mcp_servers.get("ue").unwrap();
        assert_eq!(ue.name.as_deref(), Some("Unreal Engine MCP"));
        assert_eq!(ue.transport.as_deref(), Some("stdio"));
        assert_eq!(ue.command.as_deref(), Some("node"));
        assert_eq!(ue.args, vec!["ue-mcp.js"]);
        assert_eq!(ue.env_vars.len(), 2);
        assert_eq!(ue.startup_timeout_sec, Some(Duration::from_millis(250)));
        assert_eq!(ue.startup_timeout_ms, Some(500));
        assert_eq!(ue.tool_timeout_sec, Some(Duration::from_millis(120_500)));
        assert_eq!(ue.approval_mode.as_deref(), Some("prompt"));
        assert!(!ue.auto_connect);
        assert!(!ue.supports_parallel_tool_calls);
    }

    #[test]
    fn parses_http_mcp_server_config() {
        let config: McpConfigFile = toml_edit::de::from_str(
            r#"
            [mcp_servers.unreal-mcp]
            transport = "http"
            url = "http://127.0.0.1:8000/mcp"
            startup_timeout_sec = 5
            tool_timeout_sec = 30
            "#,
        )
        .unwrap();

        let ue = config.mcp_servers.get("unreal-mcp").unwrap();
        assert_eq!(ue.transport.as_deref(), Some("http"));
        assert_eq!(ue.url.as_deref(), Some("http://127.0.0.1:8000/mcp"));
        assert!(ue.command.is_none());
        assert_eq!(ue.startup_timeout_sec, Some(Duration::from_secs(5)));
        assert_eq!(ue.tool_timeout_sec, Some(Duration::from_secs(30)));
    }

    #[test]
    fn inventory_reports_configured_disabled_and_no_eager_connect() {
        let config = McpConfigFile {
            mcp_servers: HashMap::from([
                (
                    "disabled".to_string(),
                    McpServerConfig {
                        enabled: false,
                        ..server_config(Some("dummy"), None)
                    },
                ),
                (
                    "configured".to_string(),
                    McpServerConfig {
                        required: true,
                        ..server_config(None, None)
                    },
                ),
            ]),
        };

        let runtime = McpRuntime::from_config(config);
        let inventory = runtime.inventory();
        assert_eq!(inventory.tools.len(), 0);
        assert_eq!(inventory.servers[0].id, "configured");
        assert_eq!(inventory.servers[0].status, "configured");
        assert_eq!(inventory.servers[1].id, "disabled");
        assert_eq!(inventory.servers[1].status, "disabled");
    }

    #[test]
    fn stdio_smoke_server_connects_lists_and_calls_tool() {
        if std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let smoke_server = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("tools")
            .join("mcp_smoke_server.mjs");
        if !smoke_server.exists() {
            panic!("missing MCP smoke server: {}", smoke_server.display());
        }
        let config = McpConfigFile {
            mcp_servers: HashMap::from([(
                "smoke".to_string(),
                McpServerConfig {
                    command: Some("node".to_string()),
                    args: vec![smoke_server.to_string_lossy().to_string()],
                    required: true,
                    startup_timeout_sec: Some(Duration::from_secs(5)),
                    tool_timeout_sec: Some(Duration::from_secs(5)),
                    ..server_config(None, None)
                },
            )]),
        };

        let mut runtime = McpRuntime::from_config(config);
        assert_eq!(runtime.tool_count(), 0);
        let connect = tauri::async_runtime::block_on(runtime.connect_server_by_id("smoke"));
        assert_eq!(connect.status, "connected");
        assert_eq!(runtime.server_names(), vec!["smoke".to_string()]);
        assert_eq!(runtime.tool_count(), 1);
        let tool_name = runtime.tool_summaries()[0].name.clone();
        assert_eq!(tool_name, "mcp__smoke__echo");

        let output = tauri::async_runtime::block_on(
            runtime.call_tool(&tool_name, &json!({ "message": "pong" })),
        );
        assert!(output.is_ok(), "{output:?}");
        let output_value = output.output.expect("smoke tool output");
        assert!(output_value
            .get("structuredContent")
            .and_then(|value| value.get("echoed"))
            .and_then(Value::as_str)
            .is_some_and(|value| value == "pong"));
    }

    #[test]
    fn http_smoke_server_connects_lists_and_calls_tool() {
        if std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let smoke_server = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("tools")
            .join("mcp_smoke_server.mjs");
        if !smoke_server.exists() {
            panic!("missing MCP smoke server: {}", smoke_server.display());
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let mut child = std::process::Command::new("node")
            .arg(smoke_server)
            .arg("--http")
            .arg(port.to_string())
            .spawn()
            .expect("start HTTP MCP smoke server");
        wait_for_tcp_port(port);

        let config = McpConfigFile {
            mcp_servers: HashMap::from([(
                "ue".to_string(),
                McpServerConfig {
                    url: Some(format!("http://127.0.0.1:{port}/mcp")),
                    required: true,
                    startup_timeout_sec: Some(Duration::from_secs(5)),
                    tool_timeout_sec: Some(Duration::from_secs(5)),
                    ..server_config(None, None)
                },
            )]),
        };

        let mut runtime = McpRuntime::from_config(config);
        assert_eq!(runtime.tool_count(), 0);
        let connect = tauri::async_runtime::block_on(runtime.connect_server_by_id("ue"));
        assert_eq!(connect.status, "connected");
        assert_eq!(runtime.server_names(), vec!["ue".to_string()]);
        assert_eq!(runtime.tool_count(), 1);
        let tool_name = runtime.tool_summaries()[0].name.clone();
        assert_eq!(tool_name, "mcp__ue__echo");

        let output = tauri::async_runtime::block_on(
            runtime.call_tool(&tool_name, &json!({ "message": "ue" })),
        );
        let _ = child.kill();
        let _ = child.wait();

        assert!(output.is_ok(), "{output:?}");
        let output_value = output.output.expect("HTTP smoke tool output");
        assert!(output_value
            .get("structuredContent")
            .and_then(|value| value.get("echoed"))
            .and_then(Value::as_str)
            .is_some_and(|value| value == "ue"));
    }

    #[test]
    fn management_tools_connect_and_list_next_round_message() {
        let config = McpConfigFile {
            mcp_servers: HashMap::from([("missing".to_string(), server_config(None, None))]),
        };
        let mut runtime = McpRuntime::from_config(config);
        let output = tauri::async_runtime::block_on(runtime.execute_management_tool(
            MCP_CONNECT_SERVER_TOOL_NAME,
            &json!({ "server_id": "missing" }),
        ));
        assert!(output.is_ok());
        let value = output.output.unwrap();
        assert_eq!(value["status"], json!("failed"));
        assert!(value["message"]
            .as_str()
            .unwrap()
            .contains("failed to connect"));
        assert_eq!(runtime.inventory().servers[0].status, "failed");
    }

    fn wait_for_tcp_port(port: u16) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return;
            }
            if Instant::now() >= deadline {
                panic!("HTTP MCP smoke server did not open port {port}");
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}
