#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use codeforge_desktop::app_state::AppState;
use tauri::Manager;

fn main() {
    let state = AppState::load().unwrap_or_else(|error| {
        panic!("Failed to initialize SnowAgent Desktop state: {error}");
    });

    let http_state = state.clone();

    tauri::Builder::default()
        .manage(state)
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            if let (Some(window), Some(icon)) = (
                app.get_webview_window("main"),
                app.default_window_icon().cloned(),
            ) {
                window.set_icon(icon)?;
            }

            codeforge_desktop::desktop_http_server::start(http_state.clone()).map_err(|error| {
                Box::<dyn std::error::Error>::from(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    error,
                ))
            })?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            codeforge_desktop::commands::list_projects,
            codeforge_desktop::commands::add_project,
            codeforge_desktop::commands::update_project,
            codeforge_desktop::commands::delete_project,
            codeforge_desktop::commands::get_project,
            codeforge_desktop::commands::open_visual_studio,
            codeforge_desktop::commands::register_vs_instance,
            codeforge_desktop::commands::unregister_vs_instance,
            codeforge_desktop::commands::heartbeat_vs_instance,
            codeforge_desktop::commands::list_vs_instances,
            codeforge_desktop::commands::list_tools,
            codeforge_desktop::commands::load_workspace_history,
            codeforge_desktop::commands::load_workspace_session,
            codeforge_desktop::commands::save_workspace_history,
            codeforge_desktop::commands::save_workspace_session,
            codeforge_desktop::commands::save_workspace_selection,
            codeforge_desktop::commands::delete_workspace_sessions,
            codeforge_desktop::commands::import_workspace_history,
            codeforge_desktop::commands::run_agent,
            codeforge_desktop::commands::run_tool_call_test,
            codeforge_desktop::commands::run_mock_agent,
            codeforge_desktop::commands::list_traces,
            codeforge_desktop::commands::open_code_link,
            codeforge_desktop::commands::get_settings,
            codeforge_desktop::commands::update_settings,
            codeforge_desktop::commands::fetch_minimax_models,
            codeforge_desktop::commands::fetch_openai_compatible_models
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|error| {
            panic!("SnowAgent Desktop exited with an error: {error}");
        });
}
