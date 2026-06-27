use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::history_store::HistoryStore;
use crate::mcp_runtime::McpRuntime;
use crate::project_registry::ProjectRegistry;
use crate::tool_trace::ToolTraceStore;
use crate::vs_registry::{AppSettings, SettingsStore, VsRegistry};

#[derive(Clone)]
pub struct AppState {
    pub projects: Arc<Mutex<ProjectRegistry>>,
    pub settings: Arc<Mutex<SettingsStore>>,
    pub vs_registry: Arc<Mutex<VsRegistry>>,
    pub traces: Arc<Mutex<ToolTraceStore>>,
    pub history: Arc<Mutex<HistoryStore>>,
    pub mcp_runtime: Arc<tokio::sync::Mutex<McpRuntime>>,
}

impl AppState {
    pub fn load() -> Result<Self, String> {
        let data_dir = default_data_dir();
        fs::create_dir_all(&data_dir).map_err(|error| {
            format!(
                "JSON 配置目录创建失败 {}: {error}",
                data_dir.to_string_lossy()
            )
        })?;

        Ok(Self {
            projects: Arc::new(Mutex::new(ProjectRegistry::load(
                data_dir.join("projects.json"),
            )?)),
            settings: Arc::new(Mutex::new(SettingsStore::load(
                default_config_dir().join("config.toml"),
                data_dir.to_string_lossy().to_string(),
                Some(default_config_dir().join("settings.json")),
                codebuddy_models_path(),
            )?)),
            vs_registry: Arc::new(Mutex::new(VsRegistry::default())),
            traces: Arc::new(Mutex::new(ToolTraceStore::default())),
            history: Arc::new(Mutex::new(HistoryStore::load(
                data_dir.join("codeforge.sqlite3"),
            )?)),
            mcp_runtime: Arc::new(tokio::sync::Mutex::new(
                McpRuntime::load_from_default_config(),
            )),
        })
    }
}

pub fn lock_error() -> String {
    "内部状态锁定失败，请重启 SnowAgent Desktop 后重试".to_string()
}

pub fn current_settings(store: &SettingsStore) -> AppSettings {
    store.current()
}

fn default_data_dir() -> PathBuf {
    let base = dirs::data_local_dir()
        .or_else(dirs::data_dir)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    base.join("SnowAgentDesktop")
}

fn default_config_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".codeforge")
}

fn codebuddy_models_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".codebuddy").join("models.json"))
}
