use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use codex_config::CloudConfigBundleLoader;
use codex_config::LoaderOverrides;
use codex_exec_server::EnvironmentManager;
use codex_feedback::CodexFeedback;
use codex_protocol::config_types::AltScreenMode;
use codex_rollout::StateDbHandle;

use crate::AppExitInfo;
use crate::AppServerTarget;
use crate::app::App;
use crate::app_server_session::AppServerSession;
use crate::app_server_session::ThreadParamsMode;
use crate::cli::Cli;
use crate::legacy_core::config::ConfigBuilder;
use crate::legacy_core::config::ConfigOverrides;
use crate::resume_picker::SessionSelection;
use crate::tui;
use crate::tui::Tui;

pub async fn run_codeforge_main() -> std::io::Result<AppExitInfo> {
    let cli = Cli::parse();
    let codeforge_home = find_codeforge_home()?;
    std::fs::create_dir_all(&codeforge_home)?;
    let configured_model = read_configured_model(&codeforge_home);
    let cli_kv_overrides = Vec::new();
    let loader_overrides = LoaderOverrides::default();
    let initial_cloud_config_bundle = CloudConfigBundleLoader::default();
    let feedback = CodexFeedback::new();
    let environment_manager = Arc::new(EnvironmentManager::default_for_tests());
    let initial_model = cli
        .model
        .clone()
        .or(configured_model)
        .or_else(|| Some("MiniMax-M3".to_string()));

    let overrides = ConfigOverrides {
        model: initial_model.clone(),
        cwd: cli.cwd.clone(),
        ..Default::default()
    };

    let mut config = ConfigBuilder::default()
        .codex_home(codeforge_home.clone())
        .cli_overrides(cli_kv_overrides.clone())
        .harness_overrides(overrides.clone())
        .loader_overrides(loader_overrides.clone())
        .cloud_config_bundle(initial_cloud_config_bundle)
        .build()
        .await?;
    config.model.clone_from(&initial_model);
    config.tui_alternate_screen = AltScreenMode::Never;
    config.check_for_update_on_startup = false;
    config.show_tooltips = false;

    let cloud_config_bundle = CloudConfigBundleLoader::default();

    let initialized_terminal = tui::init()?;
    let mut tui = Tui::new(
        initialized_terminal.terminal,
        initialized_terminal.enhanced_keys_supported,
        initialized_terminal.stderr_guard,
    );
    tui.set_alt_screen_enabled(false);

    let Cli {
        prompt,
        shared,
        no_alt_screen: _,
        ..
    } = cli;
    let images = shared.into_inner().images;

    let app_server = AppServerSession::stub(ThreadParamsMode::Embedded, config.clone());
    let startup_bootstrap = Some(app_server.stub_bootstrap(&config));
    let app_result = App::run(
        &mut tui,
        app_server,
        config,
        cli_kv_overrides,
        overrides,
        loader_overrides,
        cloud_config_bundle,
        prompt,
        images,
        SessionSelection::StartFresh,
        feedback,
        /*is_first_run*/ false,
        /*should_prompt_windows_sandbox_nux_at_startup*/ false,
        AppServerTarget::Embedded,
        Option::<StateDbHandle>::None,
        environment_manager,
        Duration::ZERO,
        startup_bootstrap,
        /*startup_hooks_browser*/ None,
    )
    .await
    .map_err(std::io::Error::other);

    let _ = tui::restore_after_exit();
    app_result
}

fn find_codeforge_home() -> std::io::Result<std::path::PathBuf> {
    if let Some(value) = std::env::var_os("CODEFORGE_HOME") {
        return Ok(std::path::PathBuf::from(value));
    }
    dirs::home_dir()
        .map(|home| home.join(".codeforge"))
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "home directory not found")
        })
}

fn read_configured_model(codeforge_home: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(codeforge_home.join("config.toml")).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    value
        .get("model")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}
