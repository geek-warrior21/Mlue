pub mod api;
pub mod runtime;

use runtime::server::{DEFAULT_HOST, DEFAULT_PORT, default_endpoint, run_server};
use runtime::state::{AppState, RuntimeStatus};
use std::sync::Arc;
use tauri::State;
use tokio::sync::watch;

#[tauri::command]
fn get_runtime_status(state: State<'_, Arc<AppState>>) -> RuntimeStatus {
    state.get_status()
}

pub struct ServerShutdownHandle {
    pub tx: watch::Sender<bool>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let initial_status = RuntimeStatus::starting(default_endpoint());
    let app_state = Arc::new(AppState::new(initial_status));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let state_for_server = app_state.clone();
    let state_for_tauri = app_state.clone();
    let shutdown_tx_clone = shutdown_tx.clone();

    let builder = tauri::Builder::default()
        .manage(state_for_tauri)
        .manage(ServerShutdownHandle { tx: shutdown_tx })
        .invoke_handler(tauri::generate_handler![get_runtime_status])
        .setup(move |_app| {
            tauri::async_runtime::spawn(async move {
                let _ = run_server(DEFAULT_HOST, DEFAULT_PORT, state_for_server, shutdown_rx).await;
            });
            Ok(())
        });

    let app = builder
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(move |_app_handle, event| {
        if let tauri::RunEvent::ExitRequested { .. } = event {
            let _ = shutdown_tx_clone.send(true);
        }
    });
}
