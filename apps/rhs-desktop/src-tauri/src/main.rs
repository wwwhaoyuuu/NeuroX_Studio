#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod analysis_export;
mod backend;
mod recording;
mod spikes;

use backend::{
    arm_stimulation, arm_triggered_recording, cancel_impedance_scan, cancel_stimulation,
    cancel_triggered_recording, close_playback, configure_io_outputs, configure_spikes,
    disarm_stimulation, emergency_stop, fire_software_trigger, get_pipeline_registry, get_status,
    open_playback, pause_playback, play_playback, poll_events, poll_spikes, poll_waveform,
    poll_waveforms, prepare_impedance_scan, read_recording_parameters, scan_device, seek_playback,
    set_playback_speed, start_acquisition, start_recording, start_remote_api, stop_acquisition,
    stop_recording, stop_remote_api, trigger_stimulation, update_pipeline, DesktopState,
};
use tauri::Manager;

fn main() {
    let app = tauri::Builder::default()
        .manage(DesktopState::default())
        .invoke_handler(tauri::generate_handler![
            get_status,
            get_pipeline_registry,
            poll_waveform,
            poll_waveforms,
            poll_events,
            configure_spikes,
            poll_spikes,
            scan_device,
            start_acquisition,
            stop_acquisition,
            start_recording,
            stop_recording,
            arm_triggered_recording,
            fire_software_trigger,
            cancel_triggered_recording,
            arm_stimulation,
            trigger_stimulation,
            cancel_stimulation,
            disarm_stimulation,
            configure_io_outputs,
            prepare_impedance_scan,
            cancel_impedance_scan,
            start_remote_api,
            stop_remote_api,
            open_playback,
            read_recording_parameters,
            analysis_export::export_recording_analysis,
            play_playback,
            pause_playback,
            seek_playback,
            set_playback_speed,
            close_playback,
            update_pipeline,
            emergency_stop,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build NeuroX Studio");

    app.run(|app_handle, event| {
        if matches!(event, tauri::RunEvent::ExitRequested { .. }) {
            app_handle.state::<DesktopState>().shutdown();
        }
    });
}
