use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use toml_edit::{value, DocumentMut, Item, Table};

use crate::codex_cli_runner::{CODEX_CLI_DEFAULT_MODEL, CODEX_CLI_PROVIDER_TYPE};
use crate::path_utils::normalize_display_path;

pub const MINIMAX_OPENAI_BASE_URL: &str = "https://api.minimaxi.com/v1";
pub const CODEBUDDY_OPENAI_BASE_URL: &str = "https://copilot.tencent.com/v2";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub devenv_path: Option<String>,
    pub data_dir: String,
    #[serde(default)]
    pub config_path: String,
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
            config_path: String::new(),
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
    #[serde(default)]
    pub is_default: bool,
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
    pub env_key: String,
    #[serde(default = "default_wire_api")]
    pub wire_api: String,
    #[serde(default)]
    pub requires_openai_auth: bool,
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
    #[serde(default = "default_model_reasoning_mode")]
    pub reasoning_mode: String,
    #[serde(default = "default_model_default_reasoning")]
    pub default_reasoning: String,
    #[serde(default)]
    pub supports_vision: Option<bool>,
    #[serde(default)]
    pub owned_by: Option<String>,
    #[serde(default)]
    pub created: Option<i64>,
}

pub struct SettingsStore {
    path: PathBuf,
    settings: AppSettings,
    format: SettingsFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsFormat {
    Toml,
    Json,
}

impl SettingsStore {
    pub fn load(
        path: PathBuf,
        data_dir: String,
        legacy_path: Option<PathBuf>,
        codebuddy_models_path: Option<PathBuf>,
    ) -> Result<Self, String> {
        let format = settings_format(&path);
        let (mut settings, should_save_initial) = if path.exists() {
            (read_app_settings(&path, format)?, false)
        } else if let Some(legacy_path) = legacy_path.filter(|path| path.exists()) {
            (
                read_app_settings(&legacy_path, settings_format(&legacy_path))?,
                true,
            )
        } else if let Some(imported) = codebuddy_models_path
            .as_deref()
            .and_then(import_codebuddy_models)
        {
            let mut settings = AppSettings::default();
            settings.providers = imported;
            (settings, true)
        } else {
            (AppSettings::default(), true)
        };
        settings.data_dir = normalize_display_path(&data_dir);
        settings.config_path = normalize_display_path(&path.to_string_lossy());
        let normalized_providers = normalize_settings_providers(settings.providers.clone(), format);
        let should_save_providers = normalized_providers != settings.providers;
        settings.providers = normalized_providers;

        let mut store = Self {
            path,
            settings,
            format,
        };
        let mut should_save_settings = should_save_initial || should_save_providers;
        if let Some(devenv_path) = store.settings.devenv_path.as_mut() {
            let normalized = normalize_display_path(devenv_path);
            if normalized != *devenv_path {
                *devenv_path = normalized;
                should_save_settings = true;
            }
        }
        if should_save_settings {
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
            self.settings.providers = normalize_settings_providers(providers, self.format);
        }
        self.save()?;
        Ok(self.current())
    }

    fn save(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("设置目录创建失败 {}: {error}", parent.display()))?;
        }
        match self.format {
            SettingsFormat::Toml => write_toml_app_settings(&self.path, &self.settings),
            SettingsFormat::Json => {
                let text = serde_json::to_string_pretty(&self.settings)
                    .map_err(|error| format!("JSON 设置序列化失败: {error}"))?;
                fs::write(&self.path, text)
                    .map_err(|error| format!("JSON 设置写入失败 {}: {error}", self.path.display()))
            }
        }
    }
}

fn settings_format(path: &Path) -> SettingsFormat {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("toml"))
    {
        SettingsFormat::Toml
    } else {
        SettingsFormat::Json
    }
}

fn read_app_settings(path: &Path, format: SettingsFormat) -> Result<AppSettings, String> {
    match format {
        SettingsFormat::Toml => read_toml_app_settings(path),
        SettingsFormat::Json => read_json_app_settings(path),
    }
}

fn read_json_app_settings(path: &Path) -> Result<AppSettings, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("JSON 设置读取失败 {}: {error}", path.display()))?;
    serde_json::from_str::<AppSettings>(&text)
        .map_err(|error| format!("JSON 设置解析失败 {}: {error}", path.display()))
}

fn read_toml_app_settings(path: &Path) -> Result<AppSettings, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("TOML 配置读取失败 {}: {error}", path.display()))?;
    let doc = text
        .parse::<DocumentMut>()
        .map_err(|error| format!("TOML 配置解析失败 {}: {error}", path.display()))?;

    let mut settings = AppSettings::default();
    settings.provider_notes = format!(
        "Editing {}",
        normalize_display_path(&path.to_string_lossy())
    );
    settings.devenv_path = read_desktop_string(&doc, "devenv_path");
    settings.ui_preferences = read_toml_ui_preferences(&doc);
    settings.providers = read_toml_providers(path, &doc);
    Ok(settings)
}

fn write_toml_app_settings(path: &Path, settings: &AppSettings) -> Result<(), String> {
    let existing = fs::read_to_string(path).unwrap_or_default();
    let mut doc = if existing.trim().is_empty() {
        DocumentMut::new()
    } else {
        existing
            .parse::<DocumentMut>()
            .map_err(|error| format!("TOML 配置解析失败 {}: {error}", path.display()))?
    };

    let default_provider = default_config_provider(&settings.providers);
    if let Some(provider) = default_provider {
        set_doc_string(&mut doc, "model_provider", &provider.id);
        set_doc_string(&mut doc, "model", &default_config_model(provider));
    }

    write_toml_desktop_settings(&mut doc, settings)?;
    write_toml_providers(&mut doc, &settings.providers)?;

    fs::write(path, doc.to_string())
        .map_err(|error| format!("TOML 配置写入失败 {}: {error}", path.display()))
}

fn read_toml_providers(config_path: &Path, doc: &DocumentMut) -> Vec<ProviderConfig> {
    let default_provider_id = doc_string(doc, "model_provider").unwrap_or_default();
    let default_model = doc_string(doc, "model").unwrap_or_default();
    let catalog_models = read_model_catalog(config_path, doc_string(doc, "model_catalog_json"));
    let Some(providers_table) = doc.get("model_providers").and_then(Item::as_table) else {
        return Vec::new();
    };

    providers_table
        .iter()
        .filter_map(|(id, item)| {
            let table = item.as_table()?;
            let provider_id = id.trim().to_string();
            if provider_id.is_empty() {
                return None;
            }
            let is_default = provider_id == default_provider_id;
            let token = table_string(table, "experimental_bearer_token").unwrap_or_default();
            let env_key = table_string(table, "env_key").unwrap_or_default();
            let credentials = if token.trim().is_empty() {
                Vec::new()
            } else {
                vec![ProviderCredential {
                    id: "default".to_string(),
                    name: "Bearer Token".to_string(),
                    enabled: true,
                    api_key: token,
                }]
            };
            let default_credential_id = credentials
                .first()
                .map(|credential| credential.id.clone())
                .unwrap_or_default();
            let mut models = catalog_models.clone();
            if !default_model.trim().is_empty()
                && !models.iter().any(|model| model.id == default_model)
            {
                models.insert(0, provider_model(&default_model, &default_model));
            }
            for model in &mut models {
                model.enabled = true;
                model.credential_id = default_credential_id.clone();
            }
            let default_model_for_provider = if is_default {
                default_model.clone()
            } else {
                models
                    .first()
                    .map(|model| model.id.clone())
                    .unwrap_or_default()
            };

            Some(ProviderConfig {
                id: provider_id.clone(),
                provider_type: "openai-compatible".to_string(),
                name: table_string(table, "name").unwrap_or(provider_id),
                enabled: true,
                is_default,
                base_url: table_string(table, "base_url").unwrap_or_default(),
                base_url_locked: false,
                supports_tool_call: None,
                api_key: String::new(),
                default_credential_id,
                default_model: default_model_for_provider,
                temperature: 0.2,
                env_key,
                wire_api: normalize_wire_api(
                    &table_string(table, "wire_api").unwrap_or_else(default_wire_api),
                ),
                requires_openai_auth: table_bool(table, "requires_openai_auth").unwrap_or(false),
                credentials,
                models,
            })
        })
        .collect()
}

fn write_toml_providers(doc: &mut DocumentMut, providers: &[ProviderConfig]) -> Result<(), String> {
    ensure_table(doc, "model_providers")?;
    let providers_item = doc
        .get_mut("model_providers")
        .ok_or_else(|| "TOML model_providers table missing".to_string())?;
    let providers_table = providers_item
        .as_table_mut()
        .ok_or_else(|| "TOML model_providers must be a table".to_string())?;
    let provider_ids = providers
        .iter()
        .map(|provider| provider.id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect::<std::collections::HashSet<_>>();
    let existing_ids = providers_table
        .iter()
        .map(|(id, _)| id.to_string())
        .collect::<Vec<_>>();
    for id in existing_ids {
        if !provider_ids.contains(&id) {
            providers_table.remove(&id);
        }
    }

    for provider in providers {
        let id = provider.id.trim();
        if id.is_empty() {
            continue;
        }
        if !providers_table.get(id).and_then(Item::as_table).is_some() {
            providers_table.insert(id, Item::Table(Table::new()));
        }
        let table = providers_table
            .get_mut(id)
            .and_then(Item::as_table_mut)
            .ok_or_else(|| format!("TOML model_providers.{id} must be a table"))?;
        set_table_string(table, "name", provider.name.trim());
        set_table_string(table, "base_url", provider.base_url.trim());
        set_table_string(table, "env_key", provider.env_key.trim());
        set_table_string(
            table,
            "experimental_bearer_token",
            provider_bearer_token(provider).trim(),
        );
        set_table_string(table, "wire_api", &normalize_wire_api(&provider.wire_api));
        table["requires_openai_auth"] = value(provider.requires_openai_auth);
    }
    Ok(())
}

fn read_toml_ui_preferences(doc: &DocumentMut) -> UiPreferences {
    let mut preferences = default_ui_preferences();
    let Some(desktop) = doc.get("desktop").and_then(Item::as_table) else {
        return preferences;
    };
    let Some(ui) = desktop.get("ui_preferences").and_then(Item::as_table) else {
        return preferences;
    };
    if let Some(value) = table_bool(ui, "show_trace_button") {
        preferences.show_trace_button = value;
    }
    if let Some(value) = table_bool(ui, "auto_open_trace_on_errors") {
        preferences.auto_open_trace_on_errors = value;
    }
    if let Some(value) = table_string(ui, "default_workspace_layout") {
        preferences.default_workspace_layout = value;
    }
    if let Some(value) = table_string(ui, "visual_style") {
        preferences.visual_style = value;
    }
    if let Some(value) = ui
        .get("workspace_history_days")
        .and_then(Item::as_integer)
        .and_then(|value| u32::try_from(value).ok())
    {
        preferences.workspace_history_days = value;
    }
    normalize_ui_preferences(preferences)
}

fn write_toml_desktop_settings(
    doc: &mut DocumentMut,
    settings: &AppSettings,
) -> Result<(), String> {
    ensure_table(doc, "desktop")?;
    let desktop = doc
        .get_mut("desktop")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| "TOML desktop must be a table".to_string())?;
    match settings
        .devenv_path
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        Some(path) => desktop["devenv_path"] = value(path.trim()),
        None => {
            desktop.remove("devenv_path");
        }
    }
    if !desktop
        .get("ui_preferences")
        .and_then(Item::as_table)
        .is_some()
    {
        desktop.insert("ui_preferences", Item::Table(Table::new()));
    }
    let ui = desktop
        .get_mut("ui_preferences")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| "TOML desktop.ui_preferences must be a table".to_string())?;
    ui["show_trace_button"] = value(settings.ui_preferences.show_trace_button);
    ui["auto_open_trace_on_errors"] = value(settings.ui_preferences.auto_open_trace_on_errors);
    ui["default_workspace_layout"] =
        value(settings.ui_preferences.default_workspace_layout.as_str());
    ui["visual_style"] = value(settings.ui_preferences.visual_style.as_str());
    ui["workspace_history_days"] = value(i64::from(settings.ui_preferences.workspace_history_days));
    Ok(())
}

fn read_desktop_string(doc: &DocumentMut, key: &str) -> Option<String> {
    doc.get("desktop")
        .and_then(Item::as_table)
        .and_then(|table| table_string(table, key))
}

fn default_config_provider(providers: &[ProviderConfig]) -> Option<&ProviderConfig> {
    providers
        .iter()
        .find(|provider| provider.is_default)
        .or_else(|| providers.iter().find(|provider| provider.enabled))
        .or_else(|| providers.first())
}

fn default_config_model(provider: &ProviderConfig) -> String {
    if !provider.default_model.trim().is_empty() {
        return provider.default_model.trim().to_string();
    }
    provider
        .models
        .iter()
        .find(|model| model.enabled)
        .or_else(|| provider.models.first())
        .map(|model| model.id.clone())
        .unwrap_or_default()
}

fn provider_bearer_token(provider: &ProviderConfig) -> String {
    provider
        .credentials
        .iter()
        .find(|credential| credential.enabled && !credential.api_key.trim().is_empty())
        .or_else(|| {
            provider
                .credentials
                .iter()
                .find(|credential| !credential.api_key.trim().is_empty())
        })
        .map(|credential| credential.api_key.trim().to_string())
        .unwrap_or_default()
}

fn doc_string(doc: &DocumentMut, key: &str) -> Option<String> {
    doc.get(key)
        .and_then(Item::as_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn table_string(table: &Table, key: &str) -> Option<String> {
    table
        .get(key)
        .and_then(Item::as_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn table_bool(table: &Table, key: &str) -> Option<bool> {
    table.get(key).and_then(Item::as_bool)
}

fn set_doc_string(doc: &mut DocumentMut, key: &str, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        doc.as_table_mut().remove(key);
    } else {
        doc[key] = value(text);
    }
}

fn set_table_string(table: &mut Table, key: &str, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        table.remove(key);
    } else {
        table[key] = value(text);
    }
}

fn ensure_table(doc: &mut DocumentMut, key: &str) -> Result<(), String> {
    if doc.get(key).and_then(Item::as_table).is_none() {
        doc[key] = Item::Table(Table::new());
    }
    if doc.get(key).and_then(Item::as_table).is_some() {
        Ok(())
    } else {
        Err(format!("TOML {key} must be a table"))
    }
}

#[derive(Debug, Deserialize)]
struct ModelCatalogFile {
    #[serde(default)]
    models: Vec<ModelCatalogEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelCatalogEntry {
    #[serde(default)]
    slug: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    supports_vision: Option<bool>,
    #[serde(default, alias = "inputModalities")]
    input_modalities: Vec<String>,
    #[serde(default)]
    default_reasoning_level: Option<String>,
    #[serde(default)]
    supported_reasoning_levels: Vec<serde_json::Value>,
}

fn read_model_catalog(config_path: &Path, catalog_path: Option<String>) -> Vec<ProviderModel> {
    let Some(catalog_path) = catalog_path.filter(|value| !value.trim().is_empty()) else {
        return Vec::new();
    };
    let mut path = PathBuf::from(catalog_path.trim());
    if path.is_relative() {
        if let Some(parent) = config_path.parent() {
            path = parent.join(path);
        }
    }
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(catalog) = serde_json::from_str::<ModelCatalogFile>(&text) else {
        return Vec::new();
    };

    catalog
        .models
        .into_iter()
        .filter_map(|model| {
            let id = model.slug.trim();
            if id.is_empty() {
                return None;
            }
            let name = if model.display_name.trim().is_empty() {
                id.to_string()
            } else {
                model.display_name.trim().to_string()
            };
            let reasoning_mode = if model.supported_reasoning_levels.is_empty() {
                normalize_model_reasoning_mode("", id, &name)
            } else {
                "effort".to_string()
            };
            let default_reasoning = normalize_model_default_reasoning(
                &reasoning_mode,
                model.default_reasoning_level.as_deref().unwrap_or(""),
            );
            Some(ProviderModel {
                id: id.to_string(),
                name: name.clone(),
                enabled: true,
                credential_id: String::new(),
                reasoning_mode,
                default_reasoning,
                supports_vision: Some(catalog_model_supports_vision(&model, id, &name)),
                owned_by: None,
                created: None,
            })
        })
        .collect()
}

fn catalog_model_supports_vision(
    model: &ModelCatalogEntry,
    model_id: &str,
    model_name: &str,
) -> bool {
    model.supports_vision == Some(true)
        || model
            .input_modalities
            .iter()
            .any(|modality| modality.trim().eq_ignore_ascii_case("image"))
        || infer_model_supports_vision(model_id, model_name)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodeBuddyModelsFile {
    #[serde(default)]
    models: Vec<CodeBuddyModelConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodeBuddyModelConfig {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    vendor: String,
    #[serde(default)]
    api_key: String,
    url: String,
    #[serde(default)]
    temperature: Option<f64>,
    #[serde(default)]
    supports_tool_call: Option<bool>,
    #[serde(default)]
    supports_vision: Option<bool>,
}

struct CodeBuddyProviderGroup {
    base_url: String,
    name: String,
    temperature: f64,
    supports_tool_call: Option<bool>,
    credentials: Vec<ProviderCredential>,
    models: Vec<ProviderModel>,
}

fn import_codebuddy_models(path: &Path) -> Option<Vec<ProviderConfig>> {
    let text = fs::read_to_string(path).ok()?;
    let parsed = serde_json::from_str::<CodeBuddyModelsFile>(&text).ok()?;
    let mut groups: Vec<CodeBuddyProviderGroup> = Vec::new();

    for model in parsed.models {
        let base_url = codebuddy_chat_url_to_base_url(&model.url)?;
        let group_index = groups
            .iter()
            .position(|group| group.base_url == base_url)
            .unwrap_or_else(|| {
                groups.push(CodeBuddyProviderGroup {
                    base_url: base_url.clone(),
                    name: codebuddy_group_name(&model),
                    temperature: model.temperature.unwrap_or(1.0).clamp(0.0, 2.0),
                    supports_tool_call: model.supports_tool_call,
                    credentials: Vec::new(),
                    models: Vec::new(),
                });
                groups.len() - 1
            });
        let group = &mut groups[group_index];
        if group.supports_tool_call.is_none() {
            group.supports_tool_call = model.supports_tool_call;
        }
        let credential_id = codebuddy_credential_id(group, &model);
        if !group
            .models
            .iter()
            .any(|item| item.id == model.id && item.credential_id == credential_id)
        {
            group.models.push(ProviderModel {
                id: model.id.trim().to_string(),
                name: model.id.trim().to_string(),
                enabled: true,
                credential_id: credential_id.clone(),
                reasoning_mode: normalize_model_reasoning_mode("", &model.id, &model.id),
                default_reasoning: normalize_model_default_reasoning(
                    &normalize_model_reasoning_mode("", &model.id, &model.id),
                    "",
                ),
                supports_vision: model
                    .supports_vision
                    .or_else(|| Some(infer_model_supports_vision(&model.id, &model.id))),
                owned_by: Some(model.vendor.trim().to_string()).filter(|value| !value.is_empty()),
                created: None,
            });
        }
    }

    let providers = groups
        .into_iter()
        .enumerate()
        .filter(|(_, group)| !group.models.is_empty())
        .map(|(index, group)| ProviderConfig {
            id: format!("codebuddy-import-{}", index + 1),
            provider_type: "openai-compatible".to_string(),
            name: group.name,
            enabled: true,
            is_default: index == 0,
            base_url: group.base_url,
            base_url_locked: false,
            supports_tool_call: group.supports_tool_call,
            api_key: String::new(),
            default_credential_id: group
                .credentials
                .first()
                .map(|credential| credential.id.clone())
                .unwrap_or_default(),
            default_model: group
                .models
                .first()
                .map(|model| model.id.clone())
                .unwrap_or_default(),
            temperature: group.temperature,
            env_key: String::new(),
            wire_api: default_wire_api(),
            requires_openai_auth: false,
            credentials: group.credentials,
            models: group.models,
        })
        .collect::<Vec<_>>();

    (!providers.is_empty()).then_some(providers)
}

fn codebuddy_chat_url_to_base_url(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    trimmed
        .strip_suffix("/chat/completions")
        .map(str::to_string)
        .or_else(|| Some(trimmed.to_string()))
}

fn codebuddy_group_name(model: &CodeBuddyModelConfig) -> String {
    let vendor = model.vendor.trim();
    let alias = model.name.trim();
    match (vendor.is_empty(), alias.is_empty()) {
        (false, false) => format!("CodeBuddy {vendor} {alias}"),
        (false, true) => format!("CodeBuddy {vendor}"),
        (true, false) => format!("CodeBuddy {alias}"),
        (true, true) => "CodeBuddy Imported".to_string(),
    }
}

fn codebuddy_credential_id(
    group: &mut CodeBuddyProviderGroup,
    model: &CodeBuddyModelConfig,
) -> String {
    if let Some(credential) = group
        .credentials
        .iter()
        .find(|credential| credential.api_key == model.api_key)
    {
        return credential.id.clone();
    }

    let base_name = if model.name.trim().is_empty() {
        format!("key-{}", group.credentials.len() + 1)
    } else {
        model.name.trim().to_string()
    };
    let id = unique_provider_part_id(&base_name, group.credentials.len() + 1);
    group.credentials.push(ProviderCredential {
        id: id.clone(),
        name: base_name,
        enabled: true,
        api_key: model.api_key.trim().to_string(),
    });
    id
}

fn unique_provider_part_id(value: &str, fallback_index: usize) -> String {
    let id = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if id.is_empty() {
        format!("key-{fallback_index}")
    } else {
        id
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

fn default_model_reasoning_mode() -> String {
    "none".to_string()
}

fn default_model_default_reasoning() -> String {
    "off".to_string()
}

fn default_wire_api() -> String {
    "responses".to_string()
}

fn normalize_wire_api(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "responses" => "responses".to_string(),
        _ => default_wire_api(),
    }
}

fn normalize_model_reasoning_mode(value: &str, model_id: &str, model_name: &str) -> String {
    let inferred = infer_model_reasoning_mode(model_id, model_name);
    if inferred != "none" && value.trim().is_empty() {
        return inferred.to_string();
    }
    match value.trim().to_ascii_lowercase().as_str() {
        "toggle" => "toggle".to_string(),
        "effort" => "effort".to_string(),
        "none" if inferred == "none" => "none".to_string(),
        "none" => inferred.to_string(),
        _ => inferred.to_string(),
    }
}

fn normalize_model_default_reasoning(reasoning_mode: &str, value: &str) -> String {
    match reasoning_mode {
        "toggle" => {
            if value.eq_ignore_ascii_case("on") {
                "on".to_string()
            } else {
                "off".to_string()
            }
        }
        "effort" => match value.trim().to_ascii_lowercase().as_str() {
            "minimal" => "minimal".to_string(),
            "low" => "low".to_string(),
            "high" => "high".to_string(),
            _ => "medium".to_string(),
        },
        _ => "off".to_string(),
    }
}

fn infer_model_reasoning_mode(model_id: &str, model_name: &str) -> &'static str {
    let text = format!("{model_id} {model_name}").to_ascii_lowercase();
    if text.contains("minimax-m3") {
        "toggle"
    } else {
        "none"
    }
}

pub fn infer_model_supports_vision(model_id: &str, model_name: &str) -> bool {
    let text = format!("{model_id} {model_name}").to_ascii_lowercase();
    let positive_markers = [
        "gpt-4o",
        "gpt-4.1",
        "gpt-5",
        "o3",
        "o4",
        "gemini",
        "claude-3",
        "claude-4",
        "qwen-vl",
        "qwen2-vl",
        "qwen2.5-vl",
        "glm-4v",
        "glm-5v",
        "vision",
        "-vl",
        "_vl",
    ];
    positive_markers.iter().any(|marker| text.contains(marker))
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
            is_default: false,
            base_url: CODEBUDDY_OPENAI_BASE_URL.to_string(),
            base_url_locked: true,
            supports_tool_call: None,
            api_key: String::new(),
            default_credential_id: String::new(),
            default_model: "glm-5.1".to_string(),
            temperature: 1.0,
            env_key: String::new(),
            wire_api: default_wire_api(),
            requires_openai_auth: false,
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
            is_default: false,
            base_url: "http://127.0.0.1:11434".to_string(),
            base_url_locked: false,
            supports_tool_call: None,
            api_key: String::new(),
            default_credential_id: String::new(),
            default_model: "llama3.1".to_string(),
            temperature: 0.2,
            env_key: String::new(),
            wire_api: default_wire_api(),
            requires_openai_auth: false,
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
        is_default: true,
        base_url: String::new(),
        base_url_locked: true,
        supports_tool_call: None,
        api_key: String::new(),
        default_credential_id: String::new(),
        default_model: CODEX_CLI_DEFAULT_MODEL.to_string(),
        temperature: 0.2,
        env_key: String::new(),
        wire_api: default_wire_api(),
        requires_openai_auth: false,
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
        is_default: false,
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
        env_key: String::new(),
        wire_api: default_wire_api(),
        requires_openai_auth: false,
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
        reasoning_mode: normalize_model_reasoning_mode("", id, name),
        default_reasoning: normalize_model_default_reasoning(
            &normalize_model_reasoning_mode("", id, name),
            "",
        ),
        supports_vision: Some(infer_model_supports_vision(id, name)),
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

fn normalize_settings_providers(
    providers: Vec<ProviderConfig>,
    format: SettingsFormat,
) -> Vec<ProviderConfig> {
    match format {
        SettingsFormat::Toml => normalize_config_providers(providers),
        SettingsFormat::Json => normalize_providers(providers),
    }
}

fn normalize_config_providers(providers: Vec<ProviderConfig>) -> Vec<ProviderConfig> {
    let mut normalized = providers
        .into_iter()
        .map(|provider| {
            let id = provider.id.trim().to_string();
            let provider_type = if provider.provider_type.trim().is_empty() {
                "openai-compatible".to_string()
            } else {
                provider.provider_type.trim().to_string()
            };
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
                    reasoning_mode: normalize_model_reasoning_mode(
                        &model.reasoning_mode,
                        &model.id,
                        &model.name,
                    ),
                    default_reasoning: normalize_model_default_reasoning(
                        &normalize_model_reasoning_mode(
                            &model.reasoning_mode,
                            &model.id,
                            &model.name,
                        ),
                        &model.default_reasoning,
                    ),
                    supports_vision: model
                        .supports_vision
                        .or_else(|| Some(infer_model_supports_vision(&model.id, &model.name))),
                    owned_by: model.owned_by,
                    created: model.created,
                })
                .filter(|model| !model.id.is_empty())
                .collect::<Vec<_>>();
            ProviderConfig {
                id: id.clone(),
                provider_type,
                name: if provider.name.trim().is_empty() {
                    id.clone()
                } else {
                    provider.name.trim().to_string()
                },
                enabled: provider.enabled || provider.is_default,
                is_default: provider.is_default,
                base_url: provider.base_url.trim().to_string(),
                base_url_locked: false,
                supports_tool_call: provider.supports_tool_call,
                api_key: String::new(),
                default_credential_id,
                default_model: if provider.default_model.trim().is_empty() {
                    models
                        .first()
                        .map(|model| model.id.clone())
                        .unwrap_or_default()
                } else {
                    provider.default_model.trim().to_string()
                },
                temperature: provider.temperature.clamp(0.0, 2.0),
                env_key: provider.env_key.trim().to_string(),
                wire_api: normalize_wire_api(&provider.wire_api),
                requires_openai_auth: provider.requires_openai_auth,
                credentials,
                models,
            }
        })
        .filter(|provider| !provider.id.is_empty() && !provider.name.is_empty())
        .collect::<Vec<_>>();

    if !normalized.iter().any(|provider| provider.is_default) {
        if let Some(first) = normalized.first_mut() {
            first.is_default = true;
        }
    }
    normalized
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
                    is_default: provider.is_default,
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
                    env_key: String::new(),
                    wire_api: default_wire_api(),
                    requires_openai_auth: false,
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
                            reasoning_mode: normalize_model_reasoning_mode(
                                &model.reasoning_mode,
                                &model.id,
                                &model.name,
                            ),
                            default_reasoning: normalize_model_default_reasoning(
                                &normalize_model_reasoning_mode(
                                    &model.reasoning_mode,
                                    &model.id,
                                    &model.name,
                                ),
                                &model.default_reasoning,
                            ),
                            supports_vision: model.supports_vision.or_else(|| {
                                Some(infer_model_supports_vision(&model.id, &model.name))
                            }),
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
                    reasoning_mode: normalize_model_reasoning_mode(
                        &model.reasoning_mode,
                        &model.id,
                        &model.name,
                    ),
                    default_reasoning: normalize_model_default_reasoning(
                        &normalize_model_reasoning_mode(
                            &model.reasoning_mode,
                            &model.id,
                            &model.name,
                        ),
                        &model.default_reasoning,
                    ),
                    supports_vision: model
                        .supports_vision
                        .or_else(|| Some(infer_model_supports_vision(&model.id, &model.name))),
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
                is_default: provider.is_default,
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
                env_key: provider.env_key.trim().to_string(),
                wire_api: normalize_wire_api(&provider.wire_api),
                requires_openai_auth: provider.requires_openai_auth,
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
    use std::time::{SystemTime, UNIX_EPOCH};

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

    #[test]
    fn toml_settings_reads_configured_model_provider() {
        let root = create_temp_settings_dir();
        let config_path = root.join("config.toml");
        let catalog_path = root.join("models.json");
        fs::write(
            &catalog_path,
            r#"{"models":[{"slug":"model-a","display_name":"Model A","supported_reasoning_levels":[]}]}"#,
        )
        .unwrap();
        fs::write(
            &config_path,
            format!(
                r#"model = "model-a"
model_provider = "gateway"
model_catalog_json = '{}'

[projects.'d:\code\snowagents']
trust_level = "trusted"

[model_providers.gateway]
name = "Gateway"
base_url = "http://127.0.0.1:8080/v1"
experimental_bearer_token = "test-token"
wire_api = "responses"
requires_openai_auth = false
"#,
                catalog_path.to_string_lossy().replace('\\', "\\")
            ),
        )
        .unwrap();

        let settings = read_toml_app_settings(&config_path).unwrap();
        let provider = settings.providers.first().unwrap();

        assert_eq!(provider.id, "gateway");
        assert_eq!(provider.name, "Gateway");
        assert!(provider.is_default);
        assert_eq!(provider.default_model, "model-a");
        assert_eq!(provider.credentials[0].api_key, "test-token");
        assert!(provider.models.iter().any(|model| model.id == "model-a"));
    }

    #[test]
    fn model_catalog_uses_input_modalities_for_vision_support() {
        let root = create_temp_settings_dir();
        let config_path = root.join("config.toml");
        let catalog_path = root.join("models.json");
        fs::write(
            &catalog_path,
            r#"{"models":[{"slug":"MiniMax-M3","display_name":"MiniMax-M3","input_modalities":["text","image"]}]}"#,
        )
        .unwrap();

        let models = read_model_catalog(&config_path, Some(catalog_path.to_string_lossy().into()));
        let model = models
            .iter()
            .find(|model| model.id == "MiniMax-M3")
            .expect("MiniMax-M3 should be loaded from catalog");

        assert_eq!(model.supports_vision, Some(true));
    }

    #[test]
    fn toml_settings_write_preserves_unrelated_sections() {
        let root = create_temp_settings_dir();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            r#"model = "old-model"
model_provider = "gateway"

[projects.'d:\code\snowagents']
trust_level = "trusted"

[model_providers.gateway]
name = "Old Gateway"
base_url = "http://old.example/v1"
experimental_bearer_token = "old-token"
wire_api = "responses"
"#,
        )
        .unwrap();

        let mut settings = AppSettings::default();
        settings.providers = vec![ProviderConfig {
            id: "gateway".to_string(),
            provider_type: "openai-compatible".to_string(),
            name: "New Gateway".to_string(),
            enabled: true,
            is_default: true,
            base_url: "http://new.example/v1".to_string(),
            base_url_locked: false,
            supports_tool_call: None,
            api_key: String::new(),
            default_credential_id: "default".to_string(),
            default_model: "new-model".to_string(),
            temperature: 0.2,
            env_key: String::new(),
            wire_api: "responses".to_string(),
            requires_openai_auth: false,
            credentials: vec![ProviderCredential {
                id: "default".to_string(),
                name: "Bearer Token".to_string(),
                enabled: true,
                api_key: "new-token".to_string(),
            }],
            models: vec![provider_model("new-model", "New Model")],
        }];

        write_toml_app_settings(&config_path, &settings).unwrap();
        let written = fs::read_to_string(config_path).unwrap();

        assert!(written.contains("[projects.'d:\\code\\snowagents']"));
        assert!(written.contains("model = \"new-model\""));
        assert!(written.contains("model_provider = \"gateway\""));
        assert!(written.contains("name = \"New Gateway\""));
        assert!(written.contains("base_url = \"http://new.example/v1\""));
        assert!(written.contains("experimental_bearer_token = \"new-token\""));
    }

    fn create_temp_settings_dir() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("codeforge-settings-test-{unique}"));
        fs::create_dir_all(&root).unwrap();
        root
    }
}
