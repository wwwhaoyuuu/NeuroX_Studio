#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backend;

use backend::{
    emergency_stop, get_pipeline_registry, get_status, poll_events, poll_waveform, scan_device,
    start_acquisition, start_recording, stop_acquisition, stop_recording, update_pipeline,
    DesktopState,
};
use tauri::Manager;

fn main() {
    let app = tauri::Builder::default()
        .manage(DesktopState::default())
        .invoke_handler(tauri::generate_handler![
            get_status,
            get_pipeline_registry,
            poll_waveform,
            poll_events,
            scan_device,
            start_acquisition,
            stop_acquisition,
            start_recording,
            stop_recording,
            update_pipeline,
            emergency_stop,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build RHS Studio");

    app.run(|app_handle, event| {
        if matches!(event, tauri::RunEvent::ExitRequested { .. }) {
            app_handle.state::<DesktopState>().shutdown();
        }
    });
}
