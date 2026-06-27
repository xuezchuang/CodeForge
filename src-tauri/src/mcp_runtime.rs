use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rmcp::model::{CallToolRequestParams, CallToolResult, Tool};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::child_process::TokioChildProcess;
use rmcp::transport::StreamableHttpClientTransport;
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};
use tokio::process::Command;

use crate::tool_interface::ToolOutput;

const MCP_TOOL_PREFIX: &str = "mcp__";
const MAX_TOOL_NAME_BYTES: usize = 64;
const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct McpRuntime {
    tools: Vec<McpToolDefinition>,
    clients: HashMap<String, Arc<McpClient>>,
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
    pub server_name: String,
    pub name: String,
    pub raw_name: String,
    pub description: String,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerSummary {
    pub name: String,
    pub status: String,
    pub enabled: bool,
    pub required: bool,
    pub tool_count: usize,
    pub error: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpInventorySummary {
    pub config_path: Option<String>,
    pub servers: Vec<McpServerSummary>,
    pub tools: Vec<McpToolSummary>,
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
    pub required: bool,
    #[serde(default)]
    pub supports_parallel_tool_calls: bool,
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
    pub async fn load_from_default_config() -> Result<Option<Self>, String> {
        let Some(config_path) = default_config_path() else {
            return Ok(None);
        };
        if !config_path.exists() {
            return Ok(None);
        }
        Self::load_from_path(config_path).await
    }

    pub async fn load_from_path(config_path: PathBuf) -> Result<Option<Self>, String> {
        let config = read_config_file(&config_path)?;
        Self::from_config(config).await
    }

    pub async fn inspect_default_config() -> Result<McpInventorySummary, String> {
        let config_path = default_config_path();
        let Some(config_path) = config_path else {
            return Ok(empty_inventory(None));
        };
        if !config_path.exists() {
            return Ok(empty_inventory(Some(
                config_path.to_string_lossy().to_string(),
            )));
        }
        let config = read_config_file(&config_path)?;
        inspect_config(Some(config_path), config).await
    }

    pub async fn from_config(config: McpConfigFile) -> Result<Option<Self>, String> {
        if config.mcp_servers.is_empty() {
            return Ok(None);
        }

        let mut clients = HashMap::new();
        let mut tools = Vec::new();
        let mut used_model_tool_names = HashSet::new();

        for (server_name, server) in config.mcp_servers {
            if !server.enabled {
                continue;
            }
            match connect_server(&server_name, &server).await {
                Ok(client) => {
                    let server_tools = match list_server_tools(&server_name, &server, &client).await
                    {
                        Ok(tools) => tools,
                        Err(error) if server.required => return Err(error),
                        Err(_error) => continue,
                    };
                    for tool in server_tools {
                        let raw_tool_name = tool.name.to_string();
                        let model_tool_name = unique_model_tool_name(
                            &server_name,
                            &raw_tool_name,
                            &mut used_model_tool_names,
                        );
                        let definition = openai_tool_definition(&model_tool_name, &tool);
                        tools.push(McpToolDefinition {
                            server_name: server_name.clone(),
                            raw_tool_name,
                            model_tool_name,
                            definition,
                            supports_parallel_tool_calls: server.supports_parallel_tool_calls,
                        });
                    }
                    clients.insert(server_name, Arc::new(client));
                }
                Err(error) if server.required => return Err(error),
                Err(_error) => {}
            }
        }

        if tools.is_empty() {
            return Ok(None);
        }

        Ok(Some(Self { tools, clients }))
    }

    pub fn tool_definitions(&self) -> Vec<Value> {
        self.tools
            .iter()
            .map(|tool| tool.definition.clone())
            .collect()
    }

    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }

    pub fn tool_summaries(&self) -> Vec<McpToolSummary> {
        self.tools
            .iter()
            .map(|tool| McpToolSummary {
                server_name: tool.server_name.clone(),
                name: tool.model_tool_name.clone(),
                raw_name: tool.raw_tool_name.clone(),
                description: tool
                    .definition
                    .get("function")
                    .and_then(|function| function.get("description"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            })
            .collect()
    }

    pub fn server_names(&self) -> Vec<String> {
        let mut names = self.clients.keys().cloned().collect::<Vec<_>>();
        names.sort();
        names
    }

    pub fn is_mcp_tool(&self, name: &str) -> bool {
        self.tools.iter().any(|tool| tool.model_tool_name == name)
    }

    pub fn supports_parallel_tool_calls(&self, name: &str) -> bool {
        self.tools
            .iter()
            .find(|tool| tool.model_tool_name == name)
            .is_some_and(|tool| tool.supports_parallel_tool_calls)
    }

    pub async fn call_tool(&self, model_tool_name: &str, arguments: &Value) -> ToolOutput {
        let started = Instant::now();
        let Some(tool) = self
            .tools
            .iter()
            .find(|tool| tool.model_tool_name == model_tool_name)
        else {
            return ToolOutput::error(format!("Unknown MCP tool: {model_tool_name}"), 0);
        };
        let Some(client) = self.clients.get(&tool.server_name).cloned() else {
            return ToolOutput::error(
                format!("MCP server `{}` is not connected", tool.server_name),
                0,
            );
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

async fn inspect_config(
    config_path: Option<PathBuf>,
    config: McpConfigFile,
) -> Result<McpInventorySummary, String> {
    let mut server_entries = config.mcp_servers.into_iter().collect::<Vec<_>>();
    server_entries.sort_by(|left, right| left.0.cmp(&right.0));

    let mut servers = Vec::with_capacity(server_entries.len());
    let mut tools = Vec::new();
    let mut used_model_tool_names = HashSet::new();

    for (server_name, server) in server_entries {
        if !server.enabled {
            servers.push(McpServerSummary {
                name: server_name,
                status: "disabled".to_string(),
                enabled: false,
                required: server.required,
                tool_count: 0,
                error: None,
            });
            continue;
        }

        let client = match connect_server(&server_name, &server).await {
            Ok(client) => client,
            Err(error) => {
                servers.push(McpServerSummary {
                    name: server_name,
                    status: "failed".to_string(),
                    enabled: true,
                    required: server.required,
                    tool_count: 0,
                    error: Some(error),
                });
                continue;
            }
        };

        let server_tools = match list_server_tools(&server_name, &server, &client).await {
            Ok(tools) => tools,
            Err(error) => {
                servers.push(McpServerSummary {
                    name: server_name,
                    status: "failed".to_string(),
                    enabled: true,
                    required: server.required,
                    tool_count: 0,
                    error: Some(error),
                });
                continue;
            }
        };

        let tool_count = server_tools.len();
        for tool in server_tools {
            let raw_tool_name = tool.name.to_string();
            let model_tool_name =
                unique_model_tool_name(&server_name, &raw_tool_name, &mut used_model_tool_names);
            tools.push(McpToolSummary {
                server_name: server_name.clone(),
                name: model_tool_name,
                raw_name: raw_tool_name,
                description: tool.description.as_deref().unwrap_or("").to_string(),
            });
        }
        servers.push(McpServerSummary {
            name: server_name,
            status: "ready".to_string(),
            enabled: true,
            required: server.required,
            tool_count,
            error: None,
        });
    }

    Ok(McpInventorySummary {
        config_path: config_path.map(|path| path.to_string_lossy().to_string()),
        servers,
        tools,
    })
}

fn empty_inventory(config_path: Option<String>) -> McpInventorySummary {
    McpInventorySummary {
        config_path,
        servers: Vec::new(),
        tools: Vec::new(),
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
            command: Some("dummy".to_string()),
            args: Vec::new(),
            env: HashMap::new(),
            env_vars: Vec::new(),
            cwd: None,
            url: None,
            enabled: true,
            required: false,
            supports_parallel_tool_calls: false,
            startup_timeout_sec: None,
            startup_timeout_ms: None,
            tool_timeout_sec: None,
            enabled_tools: Some(vec!["allowed".to_string()]),
            disabled_tools: Some(vec!["blocked".to_string()]),
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
            command = "node"
            args = ["ue-mcp.js"]
            startup_timeout_sec = 0.25
            env_vars = ["UE_PROJECT_ROOT", { name = "UE_PLUGIN_PATH", source = "local" }]
            startup_timeout_ms = 500
            tool_timeout_sec = 120.5
            supports_parallel_tool_calls = false
            "#,
        )
        .unwrap();

        let ue = config.mcp_servers.get("ue").unwrap();
        assert_eq!(ue.command.as_deref(), Some("node"));
        assert_eq!(ue.args, vec!["ue-mcp.js"]);
        assert_eq!(ue.env_vars.len(), 2);
        assert_eq!(ue.startup_timeout_sec, Some(Duration::from_millis(250)));
        assert_eq!(ue.startup_timeout_ms, Some(500));
        assert_eq!(ue.tool_timeout_sec, Some(Duration::from_millis(120_500)));
        assert!(!ue.supports_parallel_tool_calls);
    }

    #[test]
    fn parses_http_mcp_server_config() {
        let config: McpConfigFile = toml_edit::de::from_str(
            r#"
            [mcp_servers.unreal-mcp]
            url = "http://127.0.0.1:8000/mcp"
            startup_timeout_sec = 5
            tool_timeout_sec = 30
            "#,
        )
        .unwrap();

        let ue = config.mcp_servers.get("unreal-mcp").unwrap();
        assert_eq!(ue.url.as_deref(), Some("http://127.0.0.1:8000/mcp"));
        assert!(ue.command.is_none());
        assert_eq!(ue.startup_timeout_sec, Some(Duration::from_secs(5)));
        assert_eq!(ue.tool_timeout_sec, Some(Duration::from_secs(30)));
    }

    #[test]
    fn inspect_config_reports_disabled_and_failed_servers() {
        let config = McpConfigFile {
            mcp_servers: HashMap::from([
                (
                    "disabled".to_string(),
                    McpServerConfig {
                        command: Some("dummy".to_string()),
                        args: Vec::new(),
                        env: HashMap::new(),
                        env_vars: Vec::new(),
                        cwd: None,
                        url: None,
                        enabled: false,
                        required: false,
                        supports_parallel_tool_calls: false,
                        startup_timeout_sec: None,
                        startup_timeout_ms: None,
                        tool_timeout_sec: None,
                        enabled_tools: None,
                        disabled_tools: None,
                    },
                ),
                (
                    "missing".to_string(),
                    McpServerConfig {
                        command: None,
                        args: Vec::new(),
                        env: HashMap::new(),
                        env_vars: Vec::new(),
                        cwd: None,
                        url: None,
                        enabled: true,
                        required: true,
                        supports_parallel_tool_calls: false,
                        startup_timeout_sec: None,
                        startup_timeout_ms: None,
                        tool_timeout_sec: None,
                        enabled_tools: None,
                        disabled_tools: None,
                    },
                ),
            ]),
        };

        let inventory = tauri::async_runtime::block_on(inspect_config(None, config)).unwrap();
        assert_eq!(inventory.tools.len(), 0);
        assert_eq!(inventory.servers[0].name, "disabled");
        assert_eq!(inventory.servers[0].status, "disabled");
        assert_eq!(inventory.servers[1].name, "missing");
        assert_eq!(inventory.servers[1].status, "failed");
        assert!(inventory.servers[1].required);
        assert!(inventory.servers[1]
            .error
            .as_deref()
            .unwrap()
            .contains("missing command"));
    }

    #[test]
    fn stdio_smoke_server_lists_and_calls_tool() {
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
                    env: HashMap::new(),
                    env_vars: Vec::new(),
                    cwd: None,
                    url: None,
                    enabled: true,
                    required: true,
                    supports_parallel_tool_calls: false,
                    startup_timeout_sec: Some(Duration::from_secs(5)),
                    startup_timeout_ms: None,
                    tool_timeout_sec: Some(Duration::from_secs(5)),
                    enabled_tools: None,
                    disabled_tools: None,
                },
            )]),
        };

        let runtime = tauri::async_runtime::block_on(McpRuntime::from_config(config))
            .unwrap()
            .expect("smoke runtime should expose tools");
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
    fn http_smoke_server_lists_and_calls_tool() {
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
                    command: None,
                    args: Vec::new(),
                    env: HashMap::new(),
                    env_vars: Vec::new(),
                    cwd: None,
                    url: Some(format!("http://127.0.0.1:{port}/mcp")),
                    enabled: true,
                    required: true,
                    supports_parallel_tool_calls: false,
                    startup_timeout_sec: Some(Duration::from_secs(5)),
                    startup_timeout_ms: None,
                    tool_timeout_sec: Some(Duration::from_secs(5)),
                    enabled_tools: None,
                    disabled_tools: None,
                },
            )]),
        };

        let runtime = tauri::async_runtime::block_on(McpRuntime::from_config(config))
            .unwrap()
            .expect("HTTP smoke runtime should expose tools");
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
