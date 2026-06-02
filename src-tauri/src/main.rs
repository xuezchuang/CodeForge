#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent_runner;
mod app_state;
mod code_link;
mod commands;
mod desktop_http_server;
mod path_utils;
mod process_manager;
mod project_registry;
mod tool_registry;
mod tool_trace;
mod vs_bridge_service;
mod vs_registry;

use app_state::AppState;

fn main() {
    let state = AppState::load().unwrap_or_else(|error| {
        panic!("Failed to initialize SnowAgent Desktop state: {error}");
    });

    let http_state = state.clone();

    tauri::Builder::default()
        .manage(state)
        .plugin(tauri_plugin_dialog::init())
        .setup(move |_| {
            desktop_http_server::start(http_state.clone()).map_err(|error| {
                Box::<dyn std::error::Error>::from(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    error,
                ))
            })?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_projects,
            commands::add_project,
            commands::update_project,
            commands::delete_project,
            commands::get_project,
            commands::open_visual_studio,
            commands::register_vs_instance,
            commands::unregister_vs_instance,
            commands::heartbeat_vs_instance,
            commands::list_vs_instances,
            commands::run_agent,
            commands::run_tool_call_test,
            commands::run_mock_agent,
            commands::list_traces,
            commands::open_code_link,
            commands::get_settings,
            commands::update_settings,
            commands::fetch_minimax_models
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|error| {
            panic!("SnowAgent Desktop exited with an error: {error}");
        });
}
