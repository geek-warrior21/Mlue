pub mod api;
pub mod inference;
pub mod keys;
pub mod projects;
pub mod providers;
pub mod registry;
pub mod runtime;
pub mod storage;

use inference::types::{ChatMessage, InferenceEvent, InferenceRequest, InferenceResponse};
use inference::{InferenceError, InferenceErrorPayload, InferenceManager, TerminalGuard};
use keys::ApiKeyService;
use keys::types::{AccessPolicy, ApiKeyMetadata, CreatedApiKey};
use projects::ProjectService;
use projects::types::Project;
use providers::ollama::{DEFAULT_OLLAMA_ENDPOINT, OllamaProvider};
use registry::{DiscoveredModelsResponse, ModelRegistry};
use runtime::server::{DEFAULT_HOST, DEFAULT_PORT, default_endpoint, run_server_with_context};
use runtime::state::{AppState, RuntimeStatus};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{Manager, State};
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

// --- Project Management Commands ---

#[tauri::command(rename_all = "snake_case")]
async fn create_project(
    projects: State<'_, Arc<ProjectService>>,
    name: String,
    description: Option<String>,
) -> Result<Project, String> {
    projects
        .create_project(&name, description.as_deref())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
async fn list_projects(projects: State<'_, Arc<ProjectService>>) -> Result<Vec<Project>, String> {
    projects.list_projects().await.map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
async fn update_project(
    projects: State<'_, Arc<ProjectService>>,
    id: String,
    name: Option<String>,
    description: Option<String>,
) -> Result<Project, String> {
    let desc_arg = description.map(|d| if d.is_empty() { None } else { Some(d) });
    projects
        .update_project(
            &id,
            name.as_deref(),
            desc_arg.as_ref().map(|d| d.as_deref()),
        )
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
async fn delete_project(
    projects: State<'_, Arc<ProjectService>>,
    id: String,
) -> Result<bool, String> {
    projects
        .delete_project(&id)
        .await
        .map_err(|e| e.to_string())
}

// --- API Key Management Commands ---

#[tauri::command(rename_all = "snake_case")]
async fn create_api_key(
    keys: State<'_, Arc<ApiKeyService>>,
    project_id: String,
    name: String,
    access_policy: Option<AccessPolicy>,
) -> Result<CreatedApiKey, String> {
    keys.create_key(&project_id, &name, access_policy)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
async fn list_api_keys(
    keys: State<'_, Arc<ApiKeyService>>,
    project_id: String,
) -> Result<Vec<ApiKeyMetadata>, String> {
    keys.list_keys(&project_id).await.map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
async fn revoke_api_key(
    keys: State<'_, Arc<ApiKeyService>>,
    key_id: String,
) -> Result<bool, String> {
    keys.revoke_key(&key_id).await.map_err(|e| e.to_string())
}

#[derive(Debug, Deserialize)]
pub struct VerifyHttpApiRequest {
    pub endpoint: String,
    pub api_key: String,
    pub model: String,
    pub prompt: String,
    pub stream: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VerifyHttpApiResponse {
    pub success: bool,
    pub http_status: u16,
    pub model_used: Option<String>,
    pub content: String,
    pub duration_ms: u64,
    pub time_to_first_token_ms: Option<u64>,
    pub usage: Option<crate::inference::UsageInfo>,
    pub error: Option<VerifyHttpApiError>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VerifyHttpApiError {
    pub code: String,
    pub message: String,
    pub http_status: u16,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VerifyHttpStreamEvent {
    Delta {
        content: String,
        time_to_first_token_ms: Option<u64>,
        model_used: Option<String>,
    },
    Usage {
        usage: crate::inference::UsageInfo,
    },
    Completed {
        content: String,
        duration_ms: u64,
        time_to_first_token_ms: Option<u64>,
        usage: Option<crate::inference::UsageInfo>,
        model_used: Option<String>,
    },
    Cancelled,
    Error {
        code: String,
        message: String,
        http_status: u16,
    },
}

/// Executes non-streaming self-test verification against the local mlue gateway (/v1/chat/completions).
/// Destination is strictly validated against the running mlue port; redirects are disabled.
pub async fn execute_verify_http_api_call(
    request: VerifyHttpApiRequest,
    expected_port: u16,
) -> Result<VerifyHttpApiResponse, String> {
    if request.stream {
        return Err(
            "verify_http_api_call is non-streaming. Set stream: false or call start_verify_http_stream for real-time streaming."
                .to_string(),
        );
    }

    let target_url = crate::api::security::validate_loopback_api_destination(
        &request.endpoint,
        expected_port,
        "/v1/chat/completions",
    )?;

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;

    let payload = serde_json::json!({
        "model": request.model,
        "messages": [{ "role": "user", "content": request.prompt }],
        "stream": false,
    });

    let start_time = std::time::Instant::now();
    let res = client
        .post(target_url.as_str())
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", request.api_key),
        )
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {}", e))?;

    let status = res.status().as_u16();

    if !res.status().is_success() {
        let err_json: serde_json::Value = res.json().await.unwrap_or_default();
        let err_obj = &err_json["error"];
        let code = err_obj["code"].as_str().unwrap_or("HTTP_ERROR").to_string();
        let default_msg = if matches!(status, 301 | 302 | 307 | 308) {
            "Redirects are disabled for self-test verification"
        } else {
            "Request failed"
        };
        let message = err_obj["message"]
            .as_str()
            .unwrap_or(default_msg)
            .to_string();
        let duration_ms = start_time.elapsed().as_millis() as u64;

        return Ok(VerifyHttpApiResponse {
            success: false,
            http_status: status,
            model_used: None,
            content: String::new(),
            duration_ms,
            time_to_first_token_ms: None,
            usage: None,
            error: Some(VerifyHttpApiError {
                code,
                message,
                http_status: status,
            }),
        });
    }

    let body: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    let duration_ms = start_time.elapsed().as_millis() as u64;
    let model_used = body["model"].as_str().map(|s| s.to_string());
    let content = body["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let usage = if body["usage"].is_object() {
        serde_json::from_value::<crate::inference::UsageInfo>(body["usage"].clone()).ok()
    } else {
        None
    };

    Ok(VerifyHttpApiResponse {
        success: true,
        http_status: 200,
        model_used,
        content,
        duration_ms,
        time_to_first_token_ms: Some(duration_ms),
        usage,
        error: None,
    })
}

/// Executes streaming self-test verification using a bounded, cancellation-aware event channel.
pub async fn execute_verify_http_stream(
    window_label: Option<String>,
    manager: Arc<InferenceManager>,
    request: VerifyHttpApiRequest,
    request_id: String,
    expected_port: u16,
    on_event: tauri::ipc::Channel<VerifyHttpStreamEvent>,
) -> Result<(), String> {
    let token = manager
        .register_request(request_id.clone(), request.model.clone(), window_label)
        .map_err(|e| e.to_string())?;

    let target_url = crate::api::security::validate_loopback_api_destination(
        &request.endpoint,
        expected_port,
        "/v1/chat/completions",
    )?;

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;

    let manager_clone = manager.clone();
    let req_id_clone = request_id.clone();
    let token_clone = token.clone();

    tauri::async_runtime::spawn(async move {
        use futures_util::StreamExt;
        let start_time = std::time::Instant::now();

        let payload = serde_json::json!({
            "model": request.model,
            "messages": [{ "role": "user", "content": request.prompt }],
            "stream": true,
            "stream_options": { "include_usage": true },
        });

        let res = match client
            .post(target_url.as_str())
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", request.api_key),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&payload)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let _ = on_event.send(VerifyHttpStreamEvent::Error {
                    code: "HTTP_CONNECT_ERROR".to_string(),
                    message: format!("Failed to connect to mlue gateway: {}", e),
                    http_status: 502,
                });
                manager_clone.unregister_request(&req_id_clone);
                return;
            }
        };

        let status = res.status().as_u16();
        if !res.status().is_success() {
            let err_json: serde_json::Value = res.json().await.unwrap_or_default();
            let err_obj = &err_json["error"];
            let code = err_obj["code"].as_str().unwrap_or("HTTP_ERROR").to_string();
            let default_msg = if matches!(status, 301 | 302 | 307 | 308) {
                "Redirects are disabled for self-test verification"
            } else {
                "Request failed"
            };
            let message = err_obj["message"]
                .as_str()
                .unwrap_or(default_msg)
                .to_string();

            let _ = on_event.send(VerifyHttpStreamEvent::Error {
                code,
                message,
                http_status: status,
            });
            manager_clone.unregister_request(&req_id_clone);
            return;
        }

        let mut stream = res.bytes_stream();
        let mut time_to_first_token_ms: Option<u64> = None;
        let mut accumulated_content = String::new();
        let mut resolved_model: Option<String> = None;
        let mut parsed_usage: Option<crate::inference::UsageInfo> = None;
        let mut buffer = String::new();

        loop {
            tokio::select! {
                _ = token_clone.cancelled() => {
                    let _ = on_event.send(VerifyHttpStreamEvent::Cancelled);
                    break;
                }
                chunk_res = stream.next() => {
                    match chunk_res {
                        Some(Ok(chunk)) => {
                            buffer.push_str(&String::from_utf8_lossy(&chunk));
                            let mut lines: Vec<String> = buffer.split('\n').map(|s| s.to_string()).collect();
                            buffer = lines.pop().unwrap_or_default();

                            for line in lines {
                                let line = line.trim();
                                if line.is_empty() || line == "data: [DONE]" {
                                    continue;
                                }
                                if let Some(data_str) = line.strip_prefix("data:") {
                                    let data_str = data_str.trim();
                                    if data_str == "[DONE]" {
                                        continue;
                                    }
                                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(data_str) {
                                        if resolved_model.is_none() {
                                            resolved_model = json["model"].as_str().map(|s| s.to_string());
                                        }
                                        if let Some(content_piece) = json
                                            .pointer("/choices/0/delta/content")
                                            .and_then(|c| c.as_str())
                                            && !content_piece.is_empty()
                                        {
                                            if time_to_first_token_ms.is_none() {
                                                time_to_first_token_ms = Some(start_time.elapsed().as_millis() as u64);
                                            }
                                            accumulated_content.push_str(content_piece);

                                            if on_event.send(VerifyHttpStreamEvent::Delta {
                                                content: content_piece.to_string(),
                                                time_to_first_token_ms,
                                                model_used: resolved_model.clone(),
                                            }).is_err() {
                                                // Receiver dropped / window closed
                                                break;
                                            }
                                        }
                                        if let Ok(u) = serde_json::from_value::<crate::inference::UsageInfo>(json["usage"].clone()) {
                                            parsed_usage = Some(u.clone());
                                            let _ = on_event.send(VerifyHttpStreamEvent::Usage { usage: u });
                                        }
                                    }
                                }
                            }
                        }
                        Some(Err(err)) => {
                            let _ = on_event.send(VerifyHttpStreamEvent::Error {
                                code: "STREAM_READ_ERROR".to_string(),
                                message: format!("Stream interrupted: {}", err),
                                http_status: 500,
                            });
                            break;
                        }
                        None => {
                            let duration_ms = start_time.elapsed().as_millis() as u64;
                            let _ = on_event.send(VerifyHttpStreamEvent::Completed {
                                content: accumulated_content,
                                duration_ms,
                                time_to_first_token_ms: time_to_first_token_ms.or(Some(duration_ms)),
                                usage: parsed_usage,
                                model_used: resolved_model,
                            });
                            break;
                        }
                    }
                }
            }
        }

        manager_clone.unregister_request(&req_id_clone);
    });

    Ok(())
}

/// Narrowly scoped self-test adapter: executes non-streaming HTTP verification.
#[tauri::command(rename_all = "snake_case")]
async fn verify_http_api_call(
    app_state: State<'_, Arc<AppState>>,
    request: VerifyHttpApiRequest,
) -> Result<VerifyHttpApiResponse, String> {
    let expected_port = {
        let ep = app_state.get_status().endpoint;
        reqwest::Url::parse(&ep)
            .ok()
            .and_then(|u| u.port())
            .unwrap_or(crate::runtime::server::DEFAULT_PORT)
    };
    execute_verify_http_api_call(request, expected_port).await
}

/// Real-time streaming self-test adapter using a bounded, cancellation-aware event channel.
#[tauri::command(rename_all = "snake_case")]
async fn start_verify_http_stream(
    window: tauri::Window,
    manager: State<'_, Arc<InferenceManager>>,
    app_state: State<'_, Arc<AppState>>,
    request: VerifyHttpApiRequest,
    request_id: String,
    on_event: tauri::ipc::Channel<VerifyHttpStreamEvent>,
) -> Result<(), String> {
    let expected_port = {
        let ep = app_state.get_status().endpoint;
        reqwest::Url::parse(&ep)
            .ok()
            .and_then(|u| u.port())
            .unwrap_or(crate::runtime::server::DEFAULT_PORT)
    };

    execute_verify_http_stream(
        Some(window.label().to_string()),
        manager.inner().clone(),
        request,
        request_id,
        expected_port,
        on_event,
    )
    .await
}

/// Cancels an in-flight verification request.
#[tauri::command(rename_all = "snake_case")]
fn cancel_verify_http_call(
    window: tauri::Window,
    manager: State<'_, Arc<InferenceManager>>,
    request_id: String,
) -> CancelResult {
    let success = manager.cancel_request(&request_id, Some(window.label()));
    let message = if success {
        "Self-test verification cancellation signal dispatched".to_string()
    } else {
        "Self-test request not found, already completed, or window mismatch".to_string()
    };
    CancelResult {
        success,
        request_id,
        message,
    }
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
    let registry_for_server = model_registry.clone();
    let provider_for_tauri = ollama_provider.clone();
    let provider_for_init = ollama_provider.clone();
    let provider_for_server = ollama_provider.clone();
    let manager_for_tauri = inference_manager.clone();
    let manager_for_server = inference_manager.clone();
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
            cancel_test_completion,
            create_project,
            list_projects,
            update_project,
            delete_project,
            create_api_key,
            list_api_keys,
            revoke_api_key,
            verify_http_api_call,
            start_verify_http_stream,
            cancel_verify_http_call
        ])
        .setup(move |app| {
            let app_handle = app.handle().clone();

            // 0. Initialize SQLite database in OS app data directory
            let app_data_dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from(".mlue_data"));
            let db_path = app_data_dir.join("mlue.db");
            let pool = tauri::async_runtime::block_on(async {
                storage::init_pool_at_path(&db_path)
                    .await
                    .expect("Failed to initialize SQLite database")
            });
            let project_service = Arc::new(ProjectService::new(pool.clone()));
            let api_key_service = Arc::new(ApiKeyService::new(pool));

            app.manage(project_service);
            app.manage(api_key_service.clone());

            let server_context = runtime::server::ServerContext {
                app_state: state_for_server.clone(),
                model_registry: registry_for_server,
                ollama_provider: provider_for_server,
                inference_manager: manager_for_server,
                api_key_service,
            };

            // 1. Spawn embedded Axum HTTP server with full API context
            tauri::async_runtime::spawn(async move {
                let _ = run_server_with_context(
                    DEFAULT_HOST,
                    DEFAULT_PORT,
                    state_for_server,
                    Some(server_context),
                    shutdown_rx,
                )
                .await;
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
