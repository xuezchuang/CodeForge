use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::codex_cli_runner::{CODEX_CLI_DEFAULT_MODEL, CODEX_CLI_PROVIDER_TYPE};
use crate::path_utils::normalize_display_path;

pub const MINIMAX_OPENAI_BASE_URL: &str = "https://api.minimaxi.com/v1";
pub const CODEBUDDY_OPENAI_BASE_URL: &str = "https://copilot.tencent.com/v2";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub devenv_path: Option<String>,
    pub data_dir: String,
    #[serde(default = "default_provider_notes")]
    pub provider_notes: String,
    #[serde(default = "default_ui_preferences")]
    pub ui_preferences: UiPreferences,
    #[serde(default = "default_providers")]
    pub providers: Vec<ProviderConfig>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            devenv_path: None,
            data_dir: String::new(),
            provider_notes: default_provider_notes(),
            ui_preferences: default_ui_preferences(),
            providers: default_providers(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsInput {
    pub devenv_path: Option<String>,
    pub provider_notes: Option<String>,
    pub ui_preferences: Option<UiPreferences>,
    pub providers: Option<Vec<ProviderConfig>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiPreferences {
    pub show_trace_button: bool,
    pub auto_open_trace_on_errors: bool,
    pub default_workspace_layout: String,
    #[serde(default = "default_visual_style")]
    pub visual_style: String,
    #[serde(default = "default_workspace_history_days")]
    pub workspace_history_days: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    pub id: String,
    #[serde(rename = "type")]
    pub provider_type: String,
    pub name: String,
    pub enabled: bool,
    pub base_url: String,
    #[serde(default)]
    pub base_url_locked: bool,
    #[serde(default)]
    pub supports_tool_call: Option<bool>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_key: String,
    #[serde(default)]
    pub default_credential_id: String,
    pub default_model: String,
    pub temperature: f64,
    #[serde(default)]
    pub credentials: Vec<ProviderCredential>,
    #[serde(default)]
    pub models: Vec<ProviderModel>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCredential {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub api_key: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModel {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub credential_id: String,
    #[serde(default)]
    pub owned_by: Option<String>,
    #[serde(default)]
    pub created: Option<i64>,
}

pub struct SettingsStore {
    path: PathBuf,
    settings: AppSettings,
}

impl SettingsStore {
    pub fn load(path: PathBuf, data_dir: String) -> Result<Self, String> {
        let mut settings = if path.exists() {
            let text = fs::read_to_string(&path)
                .map_err(|error| format!("JSON 设置读取失败 {}: {error}", path.display()))?;
            serde_json::from_str::<AppSettings>(&text)
                .map_err(|error| format!("JSON 设置解析失败 {}: {error}", path.display()))?
        } else {
            AppSettings::default()
        };
        settings.data_dir = normalize_display_path(&data_dir);
        let normalized_providers = normalize_providers(settings.providers.clone());
        let should_save_providers = normalized_providers != settings.providers;
        settings.providers = normalized_providers;

        let mut store = Self { path, settings };
        if let Some(devenv_path) = store.settings.devenv_path.as_mut() {
            let normalized = normalize_display_path(devenv_path);
            if normalized != *devenv_path {
                *devenv_path = normalized;
                store.save()?;
            }
        }
        if should_save_providers {
            store.save()?;
        }
        Ok(store)
    }

    pub fn current(&self) -> AppSettings {
        self.settings.clone()
    }

    pub fn update(&mut self, input: SettingsInput) -> Result<AppSettings, String> {
        let devenv_path = match input.devenv_path {
            Some(path) if !path.trim().is_empty() => {
                let trimmed = path.trim();
                if !Path::new(trimmed).is_file() {
                    return Err(format!("devenv.exe 不存在: {trimmed}"));
                }
                Some(normalize_display_path(trimmed))
            }
            _ => None,
        };

        self.settings.devenv_path = devenv_path;
        if let Some(notes) = input.provider_notes {
            self.settings.provider_notes = notes;
        }
        if let Some(preferences) = input.ui_preferences {
            self.settings.ui_preferences = normalize_ui_preferences(preferences);
        }
        if let Some(providers) = input.providers {
            self.settings.providers = normalize_providers(providers);
        }
        self.save()?;
        Ok(self.current())
    }

    fn save(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("JSON 设置目录创建失败 {}: {error}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(&self.settings)
            .map_err(|error| format!("JSON 设置序列化失败: {error}"))?;
        fs::write(&self.path, text)
            .map_err(|error| format!("JSON 设置写入失败 {}: {error}", self.path.display()))
    }
}

fn default_provider_notes() -> String {
    "Configure provider Base URL, API key, and model selection for real chat calls.".to_string()
}

fn default_ui_preferences() -> UiPreferences {
    UiPreferences {
        show_trace_button: true,
        auto_open_trace_on_errors: true,
        default_workspace_layout: "chat-only".to_string(),
        visual_style: default_visual_style(),
        workspace_history_days: default_workspace_history_days(),
    }
}

fn default_visual_style() -> String {
    "codex".to_string()
}

fn default_workspace_history_days() -> u32 {
    7
}

fn default_providers() -> Vec<ProviderConfig> {
    vec![
        codex_cli_provider(),
        provider(
            "openai-compatible",
            "openai-compatible",
            "OpenAI-Compatible",
            "gpt-4.1",
        ),
        ProviderConfig {
            id: "codebuddy".to_string(),
            provider_type: "codebuddy".to_string(),
            name: "CodeBuddy VSCode".to_string(),
            enabled: false,
            base_url: CODEBUDDY_OPENAI_BASE_URL.to_string(),
            base_url_locked: true,
            supports_tool_call: None,
            api_key: String::new(),
            default_credential_id: String::new(),
            default_model: "glm-5.1".to_string(),
            temperature: 1.0,
            credentials: Vec::new(),
            models: codebuddy_models(),
        },
        provider("claude", "claude", "Claude", "Claude 4.1 Sonnet"),
        provider("deepseek", "deepseek", "DeepSeek", "deepseek-chat"),
        provider("minimax", "minimax", "MiniMax", "MiniMax-M2.7"),
        ProviderConfig {
            id: "ollama".to_string(),
            provider_type: "ollama".to_string(),
            name: "Ollama".to_string(),
            enabled: false,
            base_url: "http://127.0.0.1:11434".to_string(),
            base_url_locked: false,
            supports_tool_call: None,
            api_key: String::new(),
            default_credential_id: String::new(),
            default_model: "llama3.1".to_string(),
            temperature: 0.2,
            credentials: Vec::new(),
            models: Vec::new(),
        },
        provider(
            "local-gateway",
            "local-gateway",
            "Local Gateway",
            "local-default",
        ),
    ]
}

fn codex_cli_provider() -> ProviderConfig {
    ProviderConfig {
        id: CODEX_CLI_PROVIDER_TYPE.to_string(),
        provider_type: CODEX_CLI_PROVIDER_TYPE.to_string(),
        name: "Codex CLI".to_string(),
        enabled: true,
        base_url: String::new(),
        base_url_locked: true,
        supports_tool_call: None,
        api_key: String::new(),
        default_credential_id: String::new(),
        default_model: CODEX_CLI_DEFAULT_MODEL.to_string(),
        temperature: 0.2,
        credentials: Vec::new(),
        models: Vec::new(),
    }
}

fn provider(id: &str, provider_type: &str, name: &str, default_model: &str) -> ProviderConfig {
    ProviderConfig {
        id: id.to_string(),
        provider_type: provider_type.to_string(),
        name: name.to_string(),
        enabled: false,
        base_url: if id == "minimax" {
            MINIMAX_OPENAI_BASE_URL.to_string()
        } else if id == "codebuddy" {
            CODEBUDDY_OPENAI_BASE_URL.to_string()
        } else {
            String::new()
        },
        base_url_locked: id == "minimax" || id == "codebuddy",
        supports_tool_call: None,
        api_key: String::new(),
        default_credential_id: String::new(),
        default_model: default_model.to_string(),
        temperature: 0.2,
        credentials: Vec::new(),
        models: Vec::new(),
    }
}

fn codebuddy_models() -> Vec<ProviderModel> {
    vec![
        provider_model("glm-5.1", "GLM-5.1"),
        provider_model("glm-5.0-turbo", "GLM-5.0-Turbo"),
        provider_model("glm-5v-turbo", "GLM-5v-Turbo"),
        provider_model("kimi-k2.6", "Kimi-K2.6"),
        provider_model("hy3-preview", "Hy3 preview"),
        provider_model("deepseek-v4-pro", "Deepseek-V4-Pro"),
        provider_model("deepseek-v4-flash", "Deepseek-V4-Flash"),
        provider_model("deepseek-v3-2-volc", "DeepSeek V3.2"),
    ]
}

fn provider_model(id: &str, name: &str) -> ProviderModel {
    ProviderModel {
        id: id.to_string(),
        name: name.to_string(),
        enabled: false,
        credential_id: String::new(),
        owned_by: None,
        created: None,
    }
}

fn normalize_ui_preferences(preferences: UiPreferences) -> UiPreferences {
    let default_workspace_layout = match preferences.default_workspace_layout.as_str() {
        "split-chat-trace" => "split-chat-trace",
        _ => "chat-only",
    }
    .to_string();
    let visual_style = match preferences.visual_style.as_str() {
        "snowagent" => "snowagent",
        _ => "codex",
    }
    .to_string();

    UiPreferences {
        default_workspace_layout,
        visual_style,
        workspace_history_days: preferences.workspace_history_days.clamp(1, 365),
        ..preferences
    }
}

fn normalize_providers(providers: Vec<ProviderConfig>) -> Vec<ProviderConfig> {
    let normalized = providers
        .into_iter()
        .map(|provider| {
            let id = provider.id.trim().to_string();
            let provider_type = provider.provider_type.trim().to_string();
            if id == CODEX_CLI_PROVIDER_TYPE || provider_type == CODEX_CLI_PROVIDER_TYPE {
                return ProviderConfig {
                    id: CODEX_CLI_PROVIDER_TYPE.to_string(),
                    provider_type: CODEX_CLI_PROVIDER_TYPE.to_string(),
                    name: "Codex CLI".to_string(),
                    enabled: provider.enabled,
                    base_url: String::new(),
                    base_url_locked: true,
                    supports_tool_call: provider.supports_tool_call,
                    api_key: String::new(),
                    default_credential_id: String::new(),
                    default_model: if provider.default_model.trim().is_empty() {
                        CODEX_CLI_DEFAULT_MODEL.to_string()
                    } else {
                        provider.default_model.trim().to_string()
                    },
                    temperature: provider.temperature.clamp(0.0, 2.0),
                    credentials: Vec::new(),
                    models: provider
                        .models
                        .into_iter()
                        .map(|model| ProviderModel {
                            id: model.id.trim().to_string(),
                            name: if model.name.trim().is_empty() {
                                model.id.trim().to_string()
                            } else {
                                model.name.trim().to_string()
                            },
                            enabled: model.enabled,
                            credential_id: String::new(),
                            owned_by: model.owned_by,
                            created: model.created,
                        })
                        .filter(|model| !model.id.is_empty())
                        .collect(),
                };
            }
            let legacy_api_key = provider.api_key.trim().to_string();
            let credentials = normalize_credentials(provider.credentials, &legacy_api_key);
            let default_credential_id =
                normalize_default_credential_id(&provider.default_credential_id, &credentials);
            let models = provider
                .models
                .into_iter()
                .map(|model| ProviderModel {
                    id: model.id.trim().to_string(),
                    name: if model.name.trim().is_empty() {
                        model.id.trim().to_string()
                    } else {
                        model.name.trim().to_string()
                    },
                    enabled: model.enabled,
                    credential_id: normalize_model_credential_id(
                        &model.credential_id,
                        &default_credential_id,
                        &credentials,
                    ),
                    owned_by: model.owned_by,
                    created: model.created,
                })
                .filter(|model| !model.id.is_empty())
                .collect();
            ProviderConfig {
                id: id.clone(),
                provider_type: provider_type.clone(),
                name: provider.name.trim().to_string(),
                enabled: provider.enabled,
                base_url: if id == "minimax" || provider_type == "minimax" {
                    MINIMAX_OPENAI_BASE_URL.to_string()
                } else if id == "codebuddy" || provider_type == "codebuddy" {
                    CODEBUDDY_OPENAI_BASE_URL.to_string()
                } else {
                    provider.base_url.trim().to_string()
                },
                base_url_locked: id == "minimax"
                    || provider_type == "minimax"
                    || id == "codebuddy"
                    || provider_type == "codebuddy",
                supports_tool_call: provider.supports_tool_call,
                api_key: String::new(),
                default_credential_id,
                default_model: provider.default_model.trim().to_string(),
                temperature: provider.temperature.clamp(0.0, 2.0),
                credentials,
                models,
            }
        })
        .filter(|provider| !provider.id.is_empty() && !provider.name.is_empty())
        .collect();
    merge_default_providers(normalized)
}

fn normalize_model_credential_id(
    credential_id: &str,
    default_credential_id: &str,
    credentials: &[ProviderCredential],
) -> String {
    if credentials.is_empty() {
        return String::new();
    }
    let credential_id = credential_id.trim();
    if !credential_id.is_empty()
        && credentials
            .iter()
            .any(|credential| credential.id == credential_id)
    {
        return credential_id.to_string();
    }
    if !default_credential_id.trim().is_empty() {
        return default_credential_id.trim().to_string();
    }
    credentials
        .first()
        .map(|credential| credential.id.clone())
        .unwrap_or_default()
}

fn normalize_credentials(
    credentials: Vec<ProviderCredential>,
    legacy_api_key: &str,
) -> Vec<ProviderCredential> {
    let source = if credentials.is_empty() && !legacy_api_key.is_empty() {
        vec![ProviderCredential {
            id: "default".to_string(),
            name: "key-1".to_string(),
            enabled: true,
            api_key: legacy_api_key.to_string(),
        }]
    } else {
        credentials
    };

    source
        .into_iter()
        .enumerate()
        .map(|(index, credential)| ProviderCredential {
            id: if credential.id.trim().is_empty() {
                format!("key-{}", index + 1)
            } else {
                credential.id.trim().to_string()
            },
            name: if credential.name.trim().is_empty() {
                format!("key-{}", index + 1)
            } else {
                credential.name.trim().to_string()
            },
            enabled: credential.enabled,
            api_key: credential.api_key.trim().to_string(),
        })
        .filter(|credential| !credential.id.is_empty())
        .collect()
}

fn normalize_default_credential_id(
    default_credential_id: &str,
    credentials: &[ProviderCredential],
) -> String {
    let requested = default_credential_id.trim();
    if !requested.is_empty()
        && credentials
            .iter()
            .any(|credential| credential.id == requested)
    {
        return requested.to_string();
    }
    credentials
        .iter()
        .find(|credential| credential.enabled)
        .or_else(|| credentials.first())
        .map(|credential| credential.id.clone())
        .unwrap_or_default()
}

fn merge_default_providers(mut providers: Vec<ProviderConfig>) -> Vec<ProviderConfig> {
    let mut merged = Vec::new();
    for default_provider in default_providers() {
        if let Some(index) = providers
            .iter()
            .position(|provider| provider.id == default_provider.id)
        {
            let mut provider = providers.remove(index);
            if provider.default_model.is_empty() {
                provider.default_model = default_provider.default_model;
            }
            if provider.default_credential_id.is_empty() {
                provider.default_credential_id =
                    normalize_default_credential_id("", &provider.credentials);
            }
            if provider.models.is_empty() {
                provider.models = default_provider.models;
            }
            merged.push(provider);
        } else {
            merged.push(default_provider);
        }
    }
    merged.extend(providers);
    merged
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VSInstance {
    pub instance_id: String,
    pub project_id: Option<String>,
    pub process_id: u32,
    pub solution_path: String,
    pub endpoint: String,
    pub connected_at: String,
    pub last_heartbeat_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VSRegisterPayload {
    pub instance_id: String,
    pub process_id: u32,
    pub solution_path: String,
    pub endpoint: String,
}

#[derive(Default)]
pub struct VsRegistry {
    instances: HashMap<String, VSInstance>,
}

impl VsRegistry {
    pub fn register(
        &mut self,
        payload: VSRegisterPayload,
        project_id: Option<String>,
    ) -> Result<VSInstance, String> {
        if payload.instance_id.trim().is_empty() {
            return Err("VS instanceId 不能为空".to_string());
        }
        if payload.endpoint.trim().is_empty() {
            return Err("VS endpoint 不能为空".to_string());
        }

        let now = Utc::now().to_rfc3339();
        let instance = VSInstance {
            instance_id: payload.instance_id,
            project_id,
            process_id: payload.process_id,
            solution_path: normalize_display_path(&payload.solution_path),
            endpoint: payload.endpoint,
            connected_at: now.clone(),
            last_heartbeat_at: now,
        };
        self.instances
            .insert(instance.instance_id.clone(), instance.clone());
        Ok(instance)
    }

    pub fn unregister(&mut self, instance_id: &str) -> Result<VSInstance, String> {
        self.instances
            .remove(instance_id)
            .ok_or_else(|| format!("VS instance 不存在: {instance_id}"))
    }

    pub fn heartbeat(&mut self, instance_id: &str) -> Result<VSInstance, String> {
        let instance = self
            .instances
            .get_mut(instance_id)
            .ok_or_else(|| format!("VS instance 不存在: {instance_id}"))?;
        instance.last_heartbeat_at = Utc::now().to_rfc3339();
        Ok(instance.clone())
    }

    pub fn list(&self) -> Vec<VSInstance> {
        let mut instances = self.instances.values().cloned().collect::<Vec<_>>();
        instances.sort_by(|left, right| left.connected_at.cmp(&right.connected_at));
        instances
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_providers_backfills_default_models_when_saved_list_is_empty() {
        let mut stored = provider("codebuddy", "codebuddy", "CodeBuddy", "glm-5.1");
        stored.models = Vec::new();

        let normalized = normalize_providers(vec![stored]);
        let provider = normalized
            .iter()
            .find(|provider| provider.id == "codebuddy")
            .expect("codebuddy provider should be present");

        assert!(!provider.models.is_empty());
        assert!(provider.models.iter().any(|model| model.id == "glm-5.1"));
    }
}
