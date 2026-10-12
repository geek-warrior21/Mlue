use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use mlue_lib::providers::ollama::OllamaProvider;
use mlue_lib::registry::{ModelRegistry, ProviderStatus};
use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

async fn start_mock_ollama_server<F, Fut>(handler: F) -> (String, oneshot::Sender<()>)
where
    F: Fn(Request<Body>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Response<Body>> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind ephemeral port");
    let port = listener.local_addr().unwrap().port();
    let endpoint = format!("http://127.0.0.1:{}", port);
    let (tx, rx) = oneshot::channel::<()>();

    let app = axum::Router::new().fallback(move |req: Request<Body>| {
        let handler = handler.clone();
        async move { handler(req).await }
    });

    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await;
    });

    (endpoint, tx)
}

#[tokio::test]
async fn test_case_1_and_7_valid_tags_with_multiple_models_and_preserved_names() {
    let mock_json = r#"{
        "models": [
            {
                "name": "qwen3:8b",
                "model": "qwen3:8b",
                "modified_at": "2024-10-12T10:00:00Z",
                "size": 4920739840,
                "details": {
                    "parameter_size": "8B",
                    "quantization_level": "Q4_K_M"
                }
            },
            {
                "name": "library/mistral:7b-instruct-q4",
                "model": "library/mistral:7b-instruct-q4",
                "modified_at": "2024-10-11T09:30:00Z",
                "size": 4100000000,
                "details": {
                    "parameter_size": "7B",
                    "quantization_level": "Q4_0"
                }
            }
        ]
    }"#;

    let (endpoint, shutdown) = start_mock_ollama_server(move |req| {
        let body_str = mock_json.to_string();
        async move {
            assert_eq!(req.uri().path(), "/api/tags");
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/json")
                .body(Body::from(body_str))
                .unwrap()
        }
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();

    let snapshot = registry.refresh(&provider).await;

    assert_eq!(snapshot.status, ProviderStatus::Ready);
    assert_eq!(snapshot.models.len(), 2);
    assert!(snapshot.error.is_none());
    assert!(snapshot.last_successful_at.is_some());

    // Verify model 0: standard name and tag
    let m0 = &snapshot.models[0];
    assert_eq!(m0.id, "ollama/qwen3:8b");
    assert_eq!(m0.provider_id, "ollama");
    assert_eq!(m0.upstream_id, "qwen3:8b");
    assert_eq!(m0.display_name, "qwen3:8b");
    assert_eq!(m0.parameter_size.as_deref(), Some("8B"));
    assert_eq!(m0.quantization_level.as_deref(), Some("Q4_K_M"));
    assert_eq!(m0.size_bytes, Some(4920739840));
    assert_eq!(m0.capabilities.chat, None);
    assert_eq!(m0.capabilities.tools, None);

    // Verify model 1: namespace and compound tag preserved
    let m1 = &snapshot.models[1];
    assert_eq!(m1.id, "ollama/library/mistral:7b-instruct-q4");
    assert_eq!(m1.upstream_id, "library/mistral:7b-instruct-q4");
    assert_eq!(m1.display_name, "library/mistral:7b-instruct-q4");
    assert_eq!(m1.parameter_size.as_deref(), Some("7B"));
    assert_eq!(m1.quantization_level.as_deref(), Some("Q4_0"));

    let _ = shutdown.send(());
}

#[tokio::test]
async fn test_case_2_empty_models_list() {
    let (endpoint, shutdown) = start_mock_ollama_server(|_| async {
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Body::from(r#"{"models": []}"#))
            .unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();

    let snapshot = registry.refresh(&provider).await;

    assert_eq!(snapshot.status, ProviderStatus::NoModels);
    assert!(snapshot.models.is_empty());
    assert!(snapshot.error.is_none());
    assert!(snapshot.last_successful_at.is_some());

    let _ = shutdown.send(());
}

#[tokio::test]
async fn test_case_3_connection_refused() {
    // Probe an unused port and immediately close it
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    drop(listener);

    let provider = OllamaProvider::new(format!("http://{}", addr));
    let registry = ModelRegistry::new();

    let snapshot = registry.refresh(&provider).await;

    assert_eq!(snapshot.status, ProviderStatus::Unreachable);
    assert!(snapshot.models.is_empty());
    assert!(snapshot.error.is_some());
}

#[tokio::test]
async fn test_case_4_request_timeout() {
    let (endpoint, shutdown) = start_mock_ollama_server(|_| async {
        // Sleep longer than the client timeout
        tokio::time::sleep(Duration::from_millis(300)).await;
        Response::builder()
            .status(StatusCode::OK)
            .body(Body::from(r#"{"models":[]}"#))
            .unwrap()
    })
    .await;

    // Set tight client timeout of 50ms
    let provider = OllamaProvider::with_timeouts(
        &endpoint,
        Duration::from_millis(50),
        Duration::from_millis(50),
    );
    let registry = ModelRegistry::new();

    let snapshot = registry.refresh(&provider).await;

    assert_eq!(snapshot.status, ProviderStatus::Unreachable);
    assert!(snapshot.models.is_empty());
    assert!(snapshot.error.is_some());

    let _ = shutdown.send(());
}

#[tokio::test]
async fn test_case_5_malformed_json() {
    let (endpoint, shutdown) = start_mock_ollama_server(|_| async {
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Body::from(r#"{"models": [{invalid_json"#))
            .unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();

    let snapshot = registry.refresh(&provider).await;

    assert_eq!(snapshot.status, ProviderStatus::Error);
    assert!(snapshot.models.is_empty());
    assert!(
        snapshot
            .error
            .as_deref()
            .unwrap()
            .contains("Invalid response format")
    );

    let _ = shutdown.send(());
}

#[tokio::test]
async fn test_case_6_http_500_response() {
    let (endpoint, shutdown) = start_mock_ollama_server(|_| async {
        Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Body::from("Internal engine failure"))
            .unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();

    let snapshot = registry.refresh(&provider).await;

    assert_eq!(snapshot.status, ProviderStatus::Error);
    assert!(snapshot.models.is_empty());
    assert!(
        snapshot
            .error
            .as_deref()
            .unwrap()
            .contains("HTTP error 500")
    );

    let _ = shutdown.send(());
}

#[tokio::test]
async fn test_case_8_refresh_does_not_create_duplicates() {
    let mock_json = r#"{
        "models": [
            {
                "name": "llama3:8b",
                "modified_at": "2024-10-12T10:00:00Z",
                "size": 4900000000
            }
        ]
    }"#;

    let (endpoint, shutdown) = start_mock_ollama_server(move |_| {
        let body_str = mock_json.to_string();
        async move {
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/json")
                .body(Body::from(body_str))
                .unwrap()
        }
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();

    // First refresh
    let s1 = registry.refresh(&provider).await;
    assert_eq!(s1.models.len(), 1);

    // Second refresh
    let s2 = registry.refresh(&provider).await;
    assert_eq!(s2.models.len(), 1);
    assert_eq!(s2.models[0].id, "ollama/llama3:8b");

    let _ = shutdown.send(());
}

#[tokio::test]
async fn test_case_9_registry_reflects_provider_going_offline() {
    let (endpoint, shutdown) = start_mock_ollama_server(|_| async {
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Body::from(r#"{"models":[{"name":"phi3:mini"}]}"#))
            .unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();

    // Step 1: Initial successful discovery
    let s1 = registry.refresh(&provider).await;
    assert_eq!(s1.status, ProviderStatus::Ready);
    assert_eq!(s1.models.len(), 1);

    // Step 2: Server shuts down (provider goes offline)
    let _ = shutdown.send(());
    // Give short moment for shutdown to complete
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Step 3: Refresh again
    let s2 = registry.refresh(&provider).await;
    assert_eq!(s2.status, ProviderStatus::Unreachable);
    assert!(
        s2.models.is_empty(),
        "Models must be cleared when provider goes offline"
    );
    assert!(s2.error.is_some());
}

#[tokio::test]
async fn test_case_10_existing_health_endpoint_remains_functional() {
    use mlue_lib::api::health::health_handler;
    let res = health_handler().await;
    assert_eq!(res.status, "ok");
    assert_eq!(res.service, "mlue");
    assert_eq!(res.version, "0.1.0");
}

#[tokio::test]
async fn test_case_11_startup_discovery_transitions_from_checking_to_terminal_state() {
    let registry = std::sync::Arc::new(ModelRegistry::new());
    // Initial state before background task completes is Checking
    assert_eq!(registry.get_snapshot().status, ProviderStatus::Checking);

    let (endpoint, shutdown) = start_mock_ollama_server(|_| async {
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Body::from(r#"{"models":[{"name":"llama3:8b"}]}"#))
            .unwrap()
    })
    .await;

    let provider = std::sync::Arc::new(OllamaProvider::new(&endpoint));

    // Simulate startup asynchronous task
    let reg_clone = registry.clone();
    let prov_clone = provider.clone();
    let handle = tokio::spawn(async move { reg_clone.refresh(&*prov_clone).await });

    let snapshot = handle.await.expect("task panicked");
    assert_eq!(snapshot.status, ProviderStatus::Ready);
    assert_ne!(registry.get_snapshot().status, ProviderStatus::Checking);
    assert_eq!(registry.get_snapshot().models.len(), 1);

    let _ = shutdown.send(());
}
