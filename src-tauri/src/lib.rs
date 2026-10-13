pub mod api;
pub mod inference;
pub mod providers;
pub mod registry;
pub mod runtime;

use inference::types::{ChatMessage, InferenceEvent, InferenceRequest, InferenceResponse};
use inference::{InferenceError, InferenceErrorPayload, InferenceManager, TerminalGuard};
use providers::ollama::{DEFAULT_OLLAMA_ENDPOINT, OllamaProvider};
use registry::{DiscoveredModelsResponse, ModelRegistry};
use runtime::server::{DEFAULT_HOST, DEFAULT_PORT, default_endpoint, run_server};
use runtime::state::{AppState, RuntimeStatus};
use serde::{Deserialize, Serialize};
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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CancelResult {
    pub success: bool,
    pub request_id: String,
    pub message: String,
}

#[tauri::command(rename_all = "snake_case")]
fn cancel_test_completion(
    window: tauri::Window,
    manager: State<'_, Arc<InferenceManager>>,
    request_id: String,
) -> CancelResult {
    let success = manager.cancel_request(&request_id, Some(window.label()));
    let message = if success {
        "Cancellation signal dispatched".to_string()
    } else {
        "Request not found, already completed, or window mismatch".to_string()
    };
    CancelResult {
        success,
        request_id,
        message,
    }
}

#[allow(clippy::too_many_arguments)]
#[tauri::command(rename_all = "snake_case")]
async fn start_test_stream(
    window: tauri::Window,
    registry: State<'_, Arc<ModelRegistry>>,
    provider: State<'_, Arc<OllamaProvider>>,
    manager: State<'_, Arc<InferenceManager>>,
    request_id: String,
    model_id: String,
    messages: Vec<ChatMessage>,
    on_event: tauri::ipc::Channel<InferenceEvent>,
) -> Result<(), InferenceErrorPayload> {
    let token = manager
        .register_request(
            request_id.clone(),
            model_id.clone(),
            Some(window.label().to_string()),
        )
        .map_err(InferenceErrorPayload::from)?;

    let request = InferenceRequest {
        request_id: Some(request_id.clone()),
        model_id,
        messages,
    };

    let registry_clone = registry.inner().clone();
    let provider_clone = provider.inner().clone();
    let manager_clone = manager.inner().clone();
    let req_id_clone = request_id.clone();
    let token_clone = token.clone();

    tauri::async_runtime::spawn(async move {
        use futures_util::StreamExt;
        let terminal_guard = TerminalGuard::new();

        let permit = match manager_clone.acquire_permit_cancellable(&token_clone).await {
            Ok(p) => p,
            Err(InferenceError::Cancelled) => {
                if terminal_guard.try_claim() {
                    let _ = on_event.send(InferenceEvent::Cancelled {
                        request_id: req_id_clone.clone(),
                    });
                }
                manager_clone.unregister_request(&req_id_clone);
                return;
            }
            Err(err) => {
                if terminal_guard.try_claim() {
                    let _ = on_event.send(InferenceEvent::Error {
                        request_id: req_id_clone.clone(),
                        code: "CONCURRENCY_LIMIT".to_string(),
                        message: err.to_string(),
                    });
                }
                manager_clone.unregister_request(&req_id_clone);
                return;
            }
        };

        if token_clone.is_cancelled() {
            if terminal_guard.try_claim() {
                let _ = on_event.send(InferenceEvent::Cancelled {
                    request_id: req_id_clone.clone(),
                });
            }
            drop(permit);
            manager_clone.unregister_request(&req_id_clone);
            return;
        }

        let stream_res = registry_clone
            .execute_inference_stream(&provider_clone, request, token_clone.clone())
            .await;

        let mut stream = match stream_res {
            Ok(s) => s,
            Err(err) => {
                if terminal_guard.try_claim() {
                    if matches!(err, InferenceError::Cancelled) {
                        let _ = on_event.send(InferenceEvent::Cancelled {
                            request_id: req_id_clone.clone(),
                        });
                    } else {
                        let payload = InferenceErrorPayload::from(err);
                        let _ = on_event.send(InferenceEvent::Error {
                            request_id: req_id_clone.clone(),
                            code: payload.code,
                            message: payload.message,
                        });
                    }
                }
                drop(permit);
                manager_clone.unregister_request(&req_id_clone);
                return;
            }
        };

        while let Some(event_result) = stream.next().await {
            match event_result {
                Ok(event) => {
                    if event.is_terminal() {
                        if terminal_guard.try_claim() {
                            let _ = on_event.send(event);
                        }
                        break;
                    } else if !terminal_guard.has_emitted() && on_event.send(event).is_err() {
                        token_clone.cancel();
                        break;
                    }
                }
                Err(err) => {
                    if terminal_guard.try_claim() {
                        if matches!(err, InferenceError::Cancelled) {
                            let _ = on_event.send(InferenceEvent::Cancelled {
                                request_id: req_id_clone.clone(),
                            });
                        } else {
                            let payload = InferenceErrorPayload::from(err);
                            let _ = on_event.send(InferenceEvent::Error {
                                request_id: req_id_clone.clone(),
                                code: payload.code,
                                message: payload.message,
                            });
                        }
                    }
                    break;
                }
            }
        }

        drop(permit);
        manager_clone.unregister_request(&req_id_clone);
    });

    Ok(())
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
    let inference_manager = Arc::new(InferenceManager::default());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let state_for_server = app_state.clone();
    let state_for_tauri = app_state.clone();
    let registry_for_tauri = model_registry.clone();
    let registry_for_init = model_registry.clone();
    let provider_for_tauri = ollama_provider.clone();
    let provider_for_init = ollama_provider.clone();
    let manager_for_tauri = inference_manager.clone();
    let shutdown_tx_clone = shutdown_tx.clone();

    let builder = tauri::Builder::default()
        .manage(state_for_tauri)
        .manage(registry_for_tauri)
        .manage(provider_for_tauri)
        .manage(manager_for_tauri)
        .manage(ServerShutdownHandle { tx: shutdown_tx })
        .invoke_handler(tauri::generate_handler![
            get_runtime_status,
            get_discovered_models,
            refresh_ollama,
            generate_test_completion,
            start_test_stream,
            cancel_test_completion
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
