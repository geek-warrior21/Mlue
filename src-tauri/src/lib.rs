pub mod api;
pub mod inference;
pub mod providers;
pub mod registry;
pub mod runtime;

use inference::InferenceErrorPayload;
use inference::types::{InferenceRequest, InferenceResponse};
use providers::ollama::{DEFAULT_OLLAMA_ENDPOINT, OllamaProvider};
use registry::{DiscoveredModelsResponse, ModelRegistry};
use runtime::server::{DEFAULT_HOST, DEFAULT_PORT, default_endpoint, run_server};
use runtime::state::{AppState, RuntimeStatus};
use std::sync::Arc;
use tauri::State;
use tokio::sync::watch;

#[tauri::command]
fn get_runtime_status(state: State<'_, Arc<AppState>>) -> RuntimeStatus {
    state.get_status()
}

#[tauri::command]
fn get_discovered_models(registry: State<'_, Arc<ModelRegistry>>) -> DiscoveredModelsResponse {
    registry.get_snapshot()
}

#[tauri::command]
async fn refresh_ollama(
    registry: State<'_, Arc<ModelRegistry>>,
    provider: State<'_, Arc<OllamaProvider>>,
) -> Result<DiscoveredModelsResponse, String> {
    provider.clear_capabilities_cache();
    Ok(registry.refresh(provider.inner()).await)
}

#[tauri::command]
async fn generate_test_completion(
    registry: State<'_, Arc<ModelRegistry>>,
    provider: State<'_, Arc<OllamaProvider>>,
    request: InferenceRequest,
) -> Result<InferenceResponse, InferenceErrorPayload> {
    registry
        .execute_inference(provider.inner().as_ref(), request)
        .await
        .map_err(InferenceErrorPayload::from)
}

pub struct ServerShutdownHandle {
    pub tx: watch::Sender<bool>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let initial_status = RuntimeStatus::starting(default_endpoint());
    let app_state = Arc::new(AppState::new(initial_status));
    let model_registry = Arc::new(ModelRegistry::new());
    let ollama_provider = Arc::new(OllamaProvider::new(DEFAULT_OLLAMA_ENDPOINT));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let state_for_server = app_state.clone();
    let state_for_tauri = app_state.clone();
    let registry_for_tauri = model_registry.clone();
    let registry_for_init = model_registry.clone();
    let provider_for_tauri = ollama_provider.clone();
    let provider_for_init = ollama_provider.clone();
    let shutdown_tx_clone = shutdown_tx.clone();

    let builder = tauri::Builder::default()
        .manage(state_for_tauri)
        .manage(registry_for_tauri)
        .manage(provider_for_tauri)
        .manage(ServerShutdownHandle { tx: shutdown_tx })
        .invoke_handler(tauri::generate_handler![
            get_runtime_status,
            get_discovered_models,
            refresh_ollama,
            generate_test_completion
        ])
        .setup(move |app| {
            let app_handle = app.handle().clone();

            // 1. Spawn embedded Axum HTTP server
            tauri::async_runtime::spawn(async move {
                let _ = run_server(DEFAULT_HOST, DEFAULT_PORT, state_for_server, shutdown_rx).await;
            });

            // 2. Spawn initial asynchronous Ollama model discovery and emit event
            tauri::async_runtime::spawn(async move {
                use tauri::Emitter;
                provider_for_init.clear_capabilities_cache();
                let snapshot = registry_for_init.refresh(&*provider_for_init).await;
                let _ = app_handle.emit("models-updated", &snapshot);
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
