mod commands;
mod state;

use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app_state = AppState::new().expect("failed to initialize app state");

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::check_setup,
            commands::run_setup,
            commands::get_device_guides,
            commands::get_connection_guide,
            commands::set_custom_adb_path,
            commands::get_adb_status,
            commands::list_devices,
            commands::sync_pull,
            commands::sync_push,
            commands::scan_packages,
            commands::bulk_uninstall,
            commands::set_package_marks,
            commands::set_airplane_mode,
            commands::get_sync_status,
            commands::get_sync_settings,
            commands::set_sync_provider,
            commands::set_sync_folder,
            commands::sync_now,
            commands::export_report,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
