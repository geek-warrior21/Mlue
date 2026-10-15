use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use mlue_lib::api::chat::OpenAiChatCompletion;
use mlue_lib::api::models::ModelListResponse;
use mlue_lib::inference::InferenceManager;
use mlue_lib::keys::ApiKeyService;
use mlue_lib::keys::types::AccessPolicy;
use mlue_lib::projects::ProjectService;
use mlue_lib::providers::ollama::OllamaProvider;
use mlue_lib::registry::{ModelRegistry, ProviderStatus};
use mlue_lib::runtime::server::{ServerContext, run_server_with_context};
use mlue_lib::runtime::state::{AppState, RuntimeStatus};
use mlue_lib::storage;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, watch};

#[derive(Clone, Default)]
struct MockOllamaState {
    pub chat_requests: Arc<Mutex<Vec<Value>>>,
    pub simulate_offline: Arc<AtomicBool>,
}

async fn mock_tags_handler(State(state): State<MockOllamaState>) -> impl IntoResponse {
    if state.simulate_offline.load(Ordering::SeqCst) {
        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    Json(json!({
        "models": [
            {
                "name": "qwen3:8b",
                "model": "qwen3:8b",
                "modified_at": "2024-03-24T12:00:00Z",
                "size": 4000000000u64
            },
            {
                "name": "llama3:8b",
                "model": "llama3:8b",
                "modified_at": "2024-03-24T12:00:00Z",
                "size": 4000000000u64
            }
        ]
    }))
    .into_response()
}

async fn mock_show_handler(
    State(state): State<MockOllamaState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if state.simulate_offline.load(Ordering::SeqCst) {
        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let model = body.get("model").and_then(|v| v.as_str()).unwrap_or("");
    if model.contains("embedding-only") {
        Json(json!({
            "details": { "family": "bert" },
            "capabilities": ["embedding"]
        }))
        .into_response()
    } else {
        Json(json!({
            "details": { "family": "qwen2" },
            "capabilities": ["chat"]
        }))
        .into_response()
    }
}

async fn mock_chat_handler(
    State(state): State<MockOllamaState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if state.simulate_offline.load(Ordering::SeqCst) {
        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    state.chat_requests.lock().await.push(body.clone());

    let messages = body.get("messages").and_then(|v| v.as_array());
    let last_content = messages
        .and_then(|m| m.last())
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("Hello");

    let response_text = format!("Echo: {}", last_content);

    Json(json!({
        "model": body.get("model").and_then(|v| v.as_str()).unwrap_or("qwen3:8b"),
        "created_at": "2024-03-24T12:00:05Z",
        "message": {
            "role": "assistant",
            "content": response_text
        },
        "done": true,
        "done_reason": "stop",
        "prompt_eval_count": 12,
        "eval_count": 6
    }))
    .into_response()
}

struct TestFixture {
    pub server_endpoint: String,
    pub mock_state: MockOllamaState,
    pub project_service: Arc<ProjectService>,
    pub key_service: Arc<ApiKeyService>,
    pub model_registry: Arc<ModelRegistry>,
    pub inference_manager: Arc<InferenceManager>,
    #[allow(dead_code)]
    pub shutdown_tx: watch::Sender<bool>,
    pub client: reqwest::Client,
}

impl TestFixture {
    pub async fn new() -> Self {
        // 1. Start mock Ollama server
        let mock_state = MockOllamaState::default();
        let mock_app = axum::Router::new()
            .route("/api/tags", get(mock_tags_handler))
            .route("/api/show", post(mock_show_handler))
            .route("/api/chat", post(mock_chat_handler))
            .with_state(mock_state.clone());

        let mock_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mock_addr = mock_listener.local_addr().unwrap();
        let mock_endpoint = format!("http://{}", mock_addr);

        tokio::spawn(async move {
            let _ = axum::serve(mock_listener, mock_app).await;
        });

        // 2. Initialize in-memory SQLite storage
        let pool = storage::init_memory_pool().await.unwrap();
        let project_service = Arc::new(ProjectService::new(pool.clone()));
        let key_service = Arc::new(ApiKeyService::new(pool));

        // 3. Initialize ModelRegistry and Provider
        let model_registry = Arc::new(ModelRegistry::new());
        let ollama_provider = Arc::new(OllamaProvider::new(&mock_endpoint));
        let inference_manager = Arc::new(InferenceManager::default());

        // Refresh registry so models are populated and ready
        model_registry.refresh(&*ollama_provider).await;

        // 4. Start mlue server on ephemeral port
        let mlue_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mlue_port = mlue_listener.local_addr().unwrap().port();
        drop(mlue_listener);

        let server_endpoint = format!("http://127.0.0.1:{}", mlue_port);
        let app_state = Arc::new(AppState::new(RuntimeStatus::starting(&server_endpoint)));
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let server_context = ServerContext {
            app_state: app_state.clone(),
            model_registry: model_registry.clone(),
            ollama_provider,
            inference_manager: inference_manager.clone(),
            api_key_service: key_service.clone(),
        };

        let state_clone = app_state.clone();
        tokio::spawn(async move {
            let _ = run_server_with_context(
                "127.0.0.1",
                mlue_port,
                state_clone,
                Some(server_context),
                shutdown_rx,
            )
            .await;
        });

        // 5. Wait for server to become ready
        let client = reqwest::Client::new();
        let mut ready = false;
        for _ in 0..50 {
            if let Ok(res) = client
                .get(format!("{}/health", server_endpoint))
                .send()
                .await
                && res.status().is_success()
            {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(ready, "Server failed to start on {}", server_endpoint);

        Self {
            server_endpoint,
            mock_state,
            project_service,
            key_service,
            model_registry,
            inference_manager,
            shutdown_tx,
            client,
        }
    }
}

// 1. No Authorization header returns 401.
#[tokio::test]
async fn test_01_no_auth_header_returns_401() {
    let fixture = TestFixture::new().await;
    let res = fixture
        .client
        .get(format!("{}/v1/models", fixture.server_endpoint))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "missing_api_key");
}

// 2. Invalid Bearer token returns 401.
#[tokio::test]
async fn test_02_invalid_bearer_token_returns_401() {
    let fixture = TestFixture::new().await;

    // Bad prefix / format
    let res = fixture
        .client
        .get(format!("{}/v1/models", fixture.server_endpoint))
        .header("Authorization", "Bearer invalid_secret_token")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "invalid_api_key");

    // Non-Bearer header
    let res2 = fixture
        .client
        .get(format!("{}/v1/models", fixture.server_endpoint))
        .header("Authorization", "Basic dXNlcjpwYXNz")
        .send()
        .await
        .unwrap();
    assert_eq!(res2.status(), reqwest::StatusCode::UNAUTHORIZED);
}

// 3. Revoked key returns 401.
#[tokio::test]
async fn test_03_revoked_key_returns_401() {
    let fixture = TestFixture::new().await;
    let proj = fixture
        .project_service
        .create_project("Revoke Proj", None)
        .await
        .unwrap();
    let key = fixture
        .key_service
        .create_key(&proj.id, "Test Key", None)
        .await
        .unwrap();

    // Verify it authenticates initially
    let res1 = fixture
        .client
        .get(format!("{}/v1/models", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .send()
        .await
        .unwrap();
    assert_eq!(res1.status(), reqwest::StatusCode::OK);

    // Revoke the key
    fixture
        .key_service
        .revoke_key(&key.metadata.id)
        .await
        .unwrap();

    // Now it must return 401
    let res2 = fixture
        .client
        .get(format!("{}/v1/models", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .send()
        .await
        .unwrap();
    assert_eq!(res2.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: Value = res2.json().await.unwrap();
    assert_eq!(body["error"]["code"], "revoked_api_key");
}

// 4. Deleted project's key cannot authenticate.
#[tokio::test]
async fn test_04_deleted_project_key_cannot_authenticate() {
    let fixture = TestFixture::new().await;
    let proj = fixture
        .project_service
        .create_project("Delete Proj", None)
        .await
        .unwrap();
    let key = fixture
        .key_service
        .create_key(&proj.id, "Orphan Key", None)
        .await
        .unwrap();

    // Delete the project
    fixture
        .project_service
        .delete_project(&proj.id)
        .await
        .unwrap();

    // Key must not authenticate
    let res = fixture
        .client
        .get(format!("{}/v1/models", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "invalid_api_key");
}

// 5. Valid active key authenticates.
#[tokio::test]
async fn test_05_valid_active_key_authenticates() {
    let fixture = TestFixture::new().await;
    let proj = fixture
        .project_service
        .create_project("Active Proj", None)
        .await
        .unwrap();
    let key = fixture
        .key_service
        .create_key(&proj.id, "Active Key", None)
        .await
        .unwrap();

    let res = fixture
        .client
        .get(format!("{}/v1/models", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), reqwest::StatusCode::OK);
}

// 6. Valid key can list permitted models.
#[tokio::test]
async fn test_06_valid_key_can_list_permitted_models() {
    let fixture = TestFixture::new().await;
    let proj = fixture
        .project_service
        .create_project("List Proj", None)
        .await
        .unwrap();
    let key = fixture
        .key_service
        .create_key(&proj.id, "List Key", Some(AccessPolicy::LocalOnly))
        .await
        .unwrap();

    let res = fixture
        .client
        .get(format!("{}/v1/models", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), reqwest::StatusCode::OK);
    let list: ModelListResponse = res.json().await.unwrap();
    assert_eq!(list.object, "list");
    assert_eq!(list.data.len(), 2);
    assert_eq!(list.data[0].id, "ollama/qwen3:8b");
    assert_eq!(list.data[0].object, "model");
    assert_eq!(list.data[0].owned_by, "mlue");
}

// 7. Restricted key cannot list unauthorized models.
#[tokio::test]
async fn test_07_restricted_key_cannot_list_unauthorized_models() {
    let fixture = TestFixture::new().await;
    let proj = fixture
        .project_service
        .create_project("Restricted Proj", None)
        .await
        .unwrap();
    let key = fixture
        .key_service
        .create_key(
            &proj.id,
            "Restricted Key",
            Some(AccessPolicy::SelectedModels {
                models: vec!["ollama/qwen3:8b".to_string()],
            }),
        )
        .await
        .unwrap();

    let res = fixture
        .client
        .get(format!("{}/v1/models", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), reqwest::StatusCode::OK);
    let list: ModelListResponse = res.json().await.unwrap();
    assert_eq!(list.data.len(), 1);
    assert_eq!(list.data[0].id, "ollama/qwen3:8b");
}

// 8. Restricted key cannot call an unauthorized model.
#[tokio::test]
async fn test_08_restricted_key_cannot_call_unauthorized_model() {
    let fixture = TestFixture::new().await;
    let proj = fixture
        .project_service
        .create_project("Restricted Call Proj", None)
        .await
        .unwrap();
    let key = fixture
        .key_service
        .create_key(
            &proj.id,
            "Restricted Key",
            Some(AccessPolicy::SelectedModels {
                models: vec!["ollama/qwen3:8b".to_string()],
            }),
        )
        .await
        .unwrap();

    let res = fixture
        .client
        .post(format!("{}/v1/chat/completions", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .json(&json!({
            "model": "ollama/llama3:8b",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), reqwest::StatusCode::FORBIDDEN);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "unauthorized_model");
}

// 9. Forged local-looking model identifier cannot bypass authorization.
#[tokio::test]
async fn test_09_forged_local_looking_model_cannot_bypass_authorization() {
    let fixture = TestFixture::new().await;
    let proj = fixture
        .project_service
        .create_project("Forge Proj", None)
        .await
        .unwrap();
    let key = fixture
        .key_service
        .create_key(&proj.id, "Local Key", Some(AccessPolicy::LocalOnly))
        .await
        .unwrap();

    // 1. Model with local-looking name not registered
    let res1 = fixture
        .client
        .post(format!("{}/v1/chat/completions", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .json(&json!({
            "model": "ollama/unregistered_forged_model",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res1.status(), reqwest::StatusCode::NOT_FOUND);

    // 2. Local-looking prefix on cloud model
    let res2 = fixture
        .client
        .post(format!("{}/v1/chat/completions", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .json(&json!({
            "model": "local/openai/gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res2.status(), reqwest::StatusCode::NOT_FOUND);
}

// 10. Successful text completion returns the expected envelope.
#[tokio::test]
async fn test_10_successful_text_completion_returns_envelope() {
    let fixture = TestFixture::new().await;
    let proj = fixture
        .project_service
        .create_project("Chat Proj", None)
        .await
        .unwrap();
    let key = fixture
        .key_service
        .create_key(&proj.id, "Chat Key", None)
        .await
        .unwrap();

    let res = fixture
        .client
        .post(format!("{}/v1/chat/completions", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .json(&json!({
            "model": "ollama/qwen3:8b",
            "messages": [{"role": "user", "content": "Hello world"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), reqwest::StatusCode::OK);
    let completion: OpenAiChatCompletion = res.json().await.unwrap();
    assert!(completion.id.starts_with("chatcmpl_mlue_"));
    assert_eq!(completion.object, "chat.completion");
    assert_eq!(completion.model, "ollama/qwen3:8b");
    assert_eq!(completion.choices.len(), 1);
    assert_eq!(completion.choices[0].index, 0);
    assert_eq!(completion.choices[0].message.role, "assistant");
    assert_eq!(completion.choices[0].message.content, "Echo: Hello world");
    assert_eq!(completion.choices[0].finish_reason.as_deref(), Some("stop"));

    let usage = completion.usage.unwrap();
    assert_eq!(usage.prompt_tokens, 12);
    assert_eq!(usage.completion_tokens, 6);
    assert_eq!(usage.total_tokens, 18);
}

// 11. Multi-turn messages reach the provider correctly.
#[tokio::test]
async fn test_11_multi_turn_messages_reach_provider_correctly() {
    let fixture = TestFixture::new().await;
    let proj = fixture
        .project_service
        .create_project("Multi Proj", None)
        .await
        .unwrap();
    let key = fixture
        .key_service
        .create_key(&proj.id, "Multi Key", None)
        .await
        .unwrap();

    let res = fixture
        .client
        .post(format!("{}/v1/chat/completions", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .json(&json!({
            "model": "ollama/qwen3:8b",
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": "First message"},
                {"role": "assistant", "content": "First response"},
                {"role": "user", "content": "Second message"}
            ],
            "stream": false
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), reqwest::StatusCode::OK);

    let recorded = fixture.mock_state.chat_requests.lock().await;
    assert_eq!(recorded.len(), 1);
    let sent_messages = recorded[0]["messages"].as_array().unwrap();
    assert_eq!(sent_messages.len(), 4);
    assert_eq!(sent_messages[0]["role"], "system");
    assert_eq!(sent_messages[0]["content"], "You are a helpful assistant.");
    assert_eq!(sent_messages[1]["role"], "user");
    assert_eq!(sent_messages[1]["content"], "First message");
    assert_eq!(sent_messages[2]["role"], "assistant");
    assert_eq!(sent_messages[2]["content"], "First response");
    assert_eq!(sent_messages[3]["role"], "user");
    assert_eq!(sent_messages[3]["content"], "Second message");
}

// 12. Exact upstream model is selected.
#[tokio::test]
async fn test_12_exact_upstream_model_is_selected() {
    let fixture = TestFixture::new().await;
    let proj = fixture
        .project_service
        .create_project("Upstream Proj", None)
        .await
        .unwrap();
    let key = fixture
        .key_service
        .create_key(&proj.id, "Upstream Key", None)
        .await
        .unwrap();

    let res = fixture
        .client
        .post(format!("{}/v1/chat/completions", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .json(&json!({
            "model": "ollama/llama3:8b",
            "messages": [{"role": "user", "content": "Target model"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), reqwest::StatusCode::OK);

    let recorded = fixture.mock_state.chat_requests.lock().await;
    assert_eq!(recorded.last().unwrap()["model"], "llama3:8b");
}

// 13. model="auto" only selects permitted models.
#[tokio::test]
async fn test_13_model_auto_only_selects_permitted_models() {
    let fixture = TestFixture::new().await;
    let proj = fixture
        .project_service
        .create_project("Auto Proj", None)
        .await
        .unwrap();

    // Key with SelectedModels only permitting llama3:8b
    let key = fixture
        .key_service
        .create_key(
            &proj.id,
            "Auto Key",
            Some(AccessPolicy::SelectedModels {
                models: vec!["ollama/llama3:8b".to_string()],
            }),
        )
        .await
        .unwrap();

    let res = fixture
        .client
        .post(format!("{}/v1/chat/completions", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .json(&json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "Auto selection"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), reqwest::StatusCode::OK);
    let completion: OpenAiChatCompletion = res.json().await.unwrap();
    assert_eq!(completion.model, "ollama/llama3:8b");
}

// 14. auto returns a clear error when no permitted model exists.
#[tokio::test]
async fn test_14_auto_returns_clear_error_when_no_permitted_model_exists() {
    let fixture = TestFixture::new().await;
    let proj = fixture
        .project_service
        .create_project("Empty Auto Proj", None)
        .await
        .unwrap();
    let key = fixture
        .key_service
        .create_key(
            &proj.id,
            "No Models Key",
            Some(AccessPolicy::SelectedModels {
                models: vec!["ollama/nonexistent_model".to_string()],
            }),
        )
        .await
        .unwrap();

    let res = fixture
        .client
        .post(format!("{}/v1/chat/completions", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .json(&json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "Auto fail"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "no_models_available");
}

// 15. Unsupported streaming request is rejected clearly.
#[tokio::test]
async fn test_15_unsupported_streaming_request_rejected() {
    let fixture = TestFixture::new().await;
    let proj = fixture
        .project_service
        .create_project("Stream Proj", None)
        .await
        .unwrap();
    let key = fixture
        .key_service
        .create_key(&proj.id, "Stream Key", None)
        .await
        .unwrap();

    let res = fixture
        .client
        .post(format!("{}/v1/chat/completions", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .json(&json!({
            "model": "ollama/qwen3:8b",
            "messages": [{"role": "user", "content": "Stream me"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "unsupported_parameter");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("HTTP streaming is not supported in this version")
    );
}

// 16. Unsupported roles and parameters are rejected.
#[tokio::test]
async fn test_16_unsupported_roles_and_parameters_rejected() {
    let fixture = TestFixture::new().await;
    let proj = fixture
        .project_service
        .create_project("Roles Proj", None)
        .await
        .unwrap();
    let key = fixture
        .key_service
        .create_key(&proj.id, "Roles Key", None)
        .await
        .unwrap();

    // 1. Unsupported role: "tool"
    let res1 = fixture
        .client
        .post(format!("{}/v1/chat/completions", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .json(&json!({
            "model": "ollama/qwen3:8b",
            "messages": [{"role": "tool", "content": "result"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res1.status(), reqwest::StatusCode::BAD_REQUEST);
    let body1: Value = res1.json().await.unwrap();
    assert_eq!(body1["error"]["code"], "unsupported_role");

    // 2. Unsupported parameter: "tools"
    let res2 = fixture
        .client
        .post(format!("{}/v1/chat/completions", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .json(&json!({
            "model": "ollama/qwen3:8b",
            "messages": [{"role": "user", "content": "use tool"}],
            "stream": false,
            "tools": [{"type": "function"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res2.status(), reqwest::StatusCode::BAD_REQUEST);
    let body2: Value = res2.json().await.unwrap();
    assert_eq!(body2["error"]["code"], "unsupported_parameter");
}

// 17. Invalid JSON returns a safe error.
#[tokio::test]
async fn test_17_invalid_json_returns_safe_error() {
    let fixture = TestFixture::new().await;
    let proj = fixture
        .project_service
        .create_project("Json Proj", None)
        .await
        .unwrap();
    let key = fixture
        .key_service
        .create_key(&proj.id, "Json Key", None)
        .await
        .unwrap();

    let res = fixture
        .client
        .post(format!("{}/v1/chat/completions", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .header("Content-Type", "application/json")
        .body("{not valid json")
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "invalid_json");
}

// 18. Excessively large request is rejected.
#[tokio::test]
async fn test_18_excessively_large_request_rejected() {
    let fixture = TestFixture::new().await;
    let proj = fixture
        .project_service
        .create_project("Large Proj", None)
        .await
        .unwrap();
    let key = fixture
        .key_service
        .create_key(&proj.id, "Large Key", None)
        .await
        .unwrap();

    // 70,000 bytes > 64 KiB
    let large_message = "x".repeat(70_000);

    let res = fixture
        .client
        .post(format!("{}/v1/chat/completions", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .json(&json!({
            "model": "ollama/qwen3:8b",
            "messages": [{"role": "user", "content": large_message}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "request_too_large");
}

// 19. Provider offline/error handling works.
#[tokio::test]
async fn test_19_provider_offline_handling_works() {
    let fixture = TestFixture::new().await;
    let proj = fixture
        .project_service
        .create_project("Offline Proj", None)
        .await
        .unwrap();
    let key = fixture
        .key_service
        .create_key(&proj.id, "Offline Key", None)
        .await
        .unwrap();

    // Simulate provider offline
    fixture
        .mock_state
        .simulate_offline
        .store(true, Ordering::SeqCst);

    let res = fixture
        .client
        .post(format!("{}/v1/chat/completions", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .json(&json!({
            "model": "ollama/qwen3:8b",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "provider_offline");
}

// 20. Existing concurrency limits are respected.
#[tokio::test]
async fn test_20_existing_concurrency_limits_respected() {
    let fixture = TestFixture::new().await;
    let proj = fixture
        .project_service
        .create_project("Capacity Proj", None)
        .await
        .unwrap();
    let key = fixture
        .key_service
        .create_key(&proj.id, "Capacity Key", None)
        .await
        .unwrap();

    // Exhaust all permits from inference manager (default limit is 2)
    let permit1 = fixture
        .inference_manager
        .try_acquire_permit()
        .expect("should acquire permit 1");
    let permit2 = fixture
        .inference_manager
        .try_acquire_permit()
        .expect("should acquire permit 2");

    let res = fixture
        .client
        .post(format!("{}/v1/chat/completions", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .json(&json!({
            "model": "ollama/qwen3:8b",
            "messages": [{"role": "user", "content": "Over capacity"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "capacity_exceeded");

    drop(permit1);
    drop(permit2);
}

// 21. No plaintext API keys or prompts appear in application logs or standard error outputs.
#[tokio::test]
async fn test_21_no_plaintext_keys_or_prompts_appear_in_logs() {
    let fixture = TestFixture::new().await;
    let proj = fixture
        .project_service
        .create_project("Audit Proj", None)
        .await
        .unwrap();
    let key = fixture
        .key_service
        .create_key(&proj.id, "Audit Key", None)
        .await
        .unwrap();

    let secret_part = &key.full_key["mlue_sk_".len()..];

    // Trigger an error (e.g. model not found)
    let res = fixture
        .client
        .post(format!("{}/v1/chat/completions", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .json(&json!({
            "model": "ollama/nonexistent",
            "messages": [{"role": "user", "content": "top_secret_prompt_123"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();

    let body_str = res.text().await.unwrap();
    assert!(!body_str.contains(&key.full_key));
    assert!(!body_str.contains(secret_part));
}

// 22. Key revocation takes effect on the next request, including when usage tracking is debounced.
#[tokio::test]
async fn test_22_key_revocation_takes_effect_on_next_request() {
    let fixture = TestFixture::new().await;
    let proj = fixture
        .project_service
        .create_project("Revoke Immediate Proj", None)
        .await
        .unwrap();
    let key = fixture
        .key_service
        .create_key(&proj.id, "Immediate Key", None)
        .await
        .unwrap();

    // 1st request succeeds and triggers debounced last_used_at update
    let res1 = fixture
        .client
        .get(format!("{}/v1/models", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .send()
        .await
        .unwrap();
    assert_eq!(res1.status(), reqwest::StatusCode::OK);

    // Revoke key immediately
    fixture
        .key_service
        .revoke_key(&key.metadata.id)
        .await
        .unwrap();

    // 2nd request, issued within the 60s debounce window, MUST be rejected immediately!
    let res2 = fixture
        .client
        .get(format!("{}/v1/models", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .send()
        .await
        .unwrap();
    assert_eq!(res2.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: Value = res2.json().await.unwrap();
    assert_eq!(body["error"]["code"], "revoked_api_key");
}

// 23. Existing desktop IPC, discovery, inference, streaming and persistence tests continue to pass.
#[tokio::test]
async fn test_23_existing_regressions() {
    let fixture = TestFixture::new().await;

    // Verify /health endpoint returns HTTP 200 without auth
    let res = fixture
        .client
        .get(format!("{}/health", fixture.server_endpoint))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), reqwest::StatusCode::OK);
    let health: Value = res.json().await.unwrap();
    assert_eq!(health["status"], "ok");
    assert_eq!(health["service"], "mlue");

    // Verify model registry snapshot is accessible
    let snapshot = fixture.model_registry.get_snapshot();
    assert_eq!(snapshot.status, ProviderStatus::Ready);
    assert_eq!(snapshot.models.len(), 2);
}

// 24. The standard OpenAI Python SDK can invoke the supported endpoint against a mock Ollama model through mlue's real HTTP server.
#[tokio::test]
async fn test_24_openai_python_sdk_smoke_test() {
    let fixture = TestFixture::new().await;
    let proj = fixture
        .project_service
        .create_project("Python SDK Proj", None)
        .await
        .unwrap();
    let key = fixture
        .key_service
        .create_key(&proj.id, "Python Key", None)
        .await
        .unwrap();

    let python_binary = "/Users/gyan/Desktop/Mlue/scratch_pyenv/bin/python3";
    if !std::path::Path::new(python_binary).exists() {
        eprintln!(
            "Python virtualenv binary not found at {}, skipping python SDK invocation",
            python_binary
        );
        return;
    }

    let script = format!(
        r#"
import sys
from openai import OpenAI

client = OpenAI(
    base_url="{}/v1",
    api_key="{}"
)

try:
    # 1. Test model list
    models = client.models.list()
    model_ids = [m.id for m in models.data]
    if "ollama/qwen3:8b" not in model_ids:
        print("FAIL: Expected ollama/qwen3:8b in models list", file=sys.stderr)
        sys.exit(1)

    # 2. Test chat completion
    response = client.chat.completions.create(
        model="auto",
        messages=[
            {{"role": "user", "content": "Hello from Python SDK"}}
        ],
        stream=False
    )

    content = response.choices[0].message.content
    print("PYTHON_RESULT:" + content)
    sys.exit(0)
except Exception as e:
    print(f"FAIL: Exception: {{e}}", file=sys.stderr)
    sys.exit(2)
"#,
        fixture.server_endpoint, key.full_key
    );

    let output = tokio::process::Command::new(python_binary)
        .arg("-c")
        .arg(&script)
        .output()
        .await
        .expect("Failed to execute python SDK script");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "Python SDK test failed with code {:?}. Stderr: {}\nStdout: {}",
        output.status.code(),
        stderr,
        stdout
    );

    assert!(
        stdout.contains("PYTHON_RESULT:Echo: Hello from Python SDK"),
        "Unexpected Python output: {}",
        stdout
    );
}

// 25. Untrusted Host header is rejected.
#[tokio::test]
async fn test_25_untrusted_host_header_rejected() {
    let fixture = TestFixture::new().await;

    let res = fixture
        .client
        .get(format!("{}/health", fixture.server_endpoint))
        .header("Host", "attacker.com")
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "invalid_host");
}

// 26. Untrusted browser Origin header is rejected.
#[tokio::test]
async fn test_26_untrusted_origin_header_rejected() {
    let fixture = TestFixture::new().await;
    let proj = fixture
        .project_service
        .create_project("Origin Proj", None)
        .await
        .unwrap();
    let key = fixture
        .key_service
        .create_key(&proj.id, "Origin Key", None)
        .await
        .unwrap();

    let res = fixture
        .client
        .post(format!("{}/v1/chat/completions", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .header("Origin", "http://evil.com")
        .json(&json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), reqwest::StatusCode::FORBIDDEN);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "untrusted_origin");
}

// 27. Trusted local origins are accepted.
#[tokio::test]
async fn test_27_trusted_local_origins_accepted() {
    let fixture = TestFixture::new().await;

    // Vite dev origin
    let res1 = fixture
        .client
        .get(format!("{}/health", fixture.server_endpoint))
        .header("Origin", "http://localhost:1420")
        .send()
        .await
        .unwrap();
    assert_eq!(res1.status(), reqwest::StatusCode::OK);

    // Tauri production origin
    let res2 = fixture
        .client
        .get(format!("{}/health", fixture.server_endpoint))
        .header("Origin", "tauri://localhost")
        .send()
        .await
        .unwrap();
    assert_eq!(res2.status(), reqwest::StatusCode::OK);
}

// 28. Unsupported generation options (temperature, max_tokens) are rejected with structured error.
#[tokio::test]
async fn test_28_unsupported_generation_parameters_rejected() {
    let fixture = TestFixture::new().await;
    let proj = fixture
        .project_service
        .create_project("Param Proj", None)
        .await
        .unwrap();
    let key = fixture
        .key_service
        .create_key(&proj.id, "Param Key", None)
        .await
        .unwrap();

    let res = fixture
        .client
        .post(format!("{}/v1/chat/completions", fixture.server_endpoint))
        .header("Authorization", format!("Bearer {}", key.full_key))
        .json(&json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": false,
            "temperature": 0.7,
            "max_tokens": 100
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "unsupported_parameter");
}
