use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use http_body_util::BodyExt;
use mlue_lib::inference::types::{ChatMessage, InferenceRequest, MessageRole};
use mlue_lib::inference::{InferenceError, ModelInference};
use mlue_lib::providers::ollama::OllamaProvider;
use mlue_lib::registry::{ModelRegistry, ProviderStatus};
use serde_json::Value;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
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
        .expect("failed to bind ephemeral port for test mock");
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
async fn test_case_1_successful_single_turn_chat() {
    let (endpoint, shutdown) = start_mock_ollama_server(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/api/tags" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(
                    r#"{"models":[{"name":"qwen3:8b","modified_at":"2026-03-01T00:00:00Z","size":4500000000}]}"#,
                ))
                .unwrap();
        }
        if path == "/api/show" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(r#"{"capabilities":["chat"]}"#))
                .unwrap();
        }
        if path == "/api/chat" {
            let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
            let val: Value = serde_json::from_slice(&body_bytes).unwrap();
            assert_eq!(val["model"], "qwen3:8b");
            assert_eq!(val["messages"][0]["role"], "user");
            assert_eq!(val["messages"][0]["content"], "Hello world");

            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(
                    r#"{"message":{"role":"assistant","content":"Hello there!"},"done":true,"prompt_eval_count":5,"eval_count":10}"#,
                ))
                .unwrap();
        }
        Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    let snapshot = registry.refresh(&provider).await;
    assert_eq!(snapshot.status, ProviderStatus::Ready);

    let request = InferenceRequest {
        request_id: None,
        model_id: "ollama/qwen3:8b".to_string(),
        messages: vec![ChatMessage {
            role: MessageRole::User,
            content: "Hello world".to_string(),
        }],
    };

    let response = registry
        .execute_inference(&provider, request)
        .await
        .expect("single-turn inference should succeed");

    assert_eq!(response.model_id, "ollama/qwen3:8b");
    assert_eq!(response.content, "Hello there!");
    assert_eq!(response.finish_reason.as_deref(), Some("stop"));
    assert!(response.duration_ms.is_some());

    let _ = shutdown.send(());
}

#[tokio::test]
async fn test_case_2_successful_multi_turn_chat() {
    let (endpoint, shutdown) = start_mock_ollama_server(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/api/tags" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(
                    r#"{"models":[{"name":"qwen3:8b","modified_at":"2026-03-01T00:00:00Z","size":4500000000}]}"#,
                ))
                .unwrap();
        }
        if path == "/api/show" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(r#"{"capabilities":["chat"]}"#))
                .unwrap();
        }
        if path == "/api/chat" {
            let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
            let val: Value = serde_json::from_slice(&body_bytes).unwrap();
            let messages = val["messages"].as_array().unwrap();
            assert_eq!(messages.len(), 3);
            assert_eq!(messages[0]["role"], "user");
            assert_eq!(messages[0]["content"], "Hello");
            assert_eq!(messages[1]["role"], "assistant");
            assert_eq!(messages[1]["content"], "Hi! How can I help?");
            assert_eq!(messages[2]["role"], "user");
            assert_eq!(messages[2]["content"], "What is 2+2?");

            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(
                    r#"{"message":{"role":"assistant","content":"4"},"done":true}"#,
                ))
                .unwrap();
        }
        Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let request = InferenceRequest {
        request_id: None,
        model_id: "ollama/qwen3:8b".to_string(),
        messages: vec![
            ChatMessage {
                role: MessageRole::User,
                content: "Hello".to_string(),
            },
            ChatMessage {
                role: MessageRole::Assistant,
                content: "Hi! How can I help?".to_string(),
            },
            ChatMessage {
                role: MessageRole::User,
                content: "What is 2+2?".to_string(),
            },
        ],
    };

    let response = registry
        .execute_inference(&provider, request)
        .await
        .expect("multi-turn inference should succeed");

    assert_eq!(response.content, "4");

    let _ = shutdown.send(());
}

#[tokio::test]
async fn test_case_3_system_message_forwarded_correctly() {
    let (endpoint, shutdown) = start_mock_ollama_server(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/api/tags" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(
                    r#"{"models":[{"name":"qwen3:8b","modified_at":"2026-03-01T00:00:00Z","size":4500000000}]}"#,
                ))
                .unwrap();
        }
        if path == "/api/show" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(r#"{"capabilities":["chat"]}"#))
                .unwrap();
        }
        if path == "/api/chat" {
            let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
            let val: Value = serde_json::from_slice(&body_bytes).unwrap();
            let messages = val["messages"].as_array().unwrap();
            assert_eq!(messages[0]["role"], "system");
            assert_eq!(messages[0]["content"], "You are a concise mathematician");
            assert_eq!(messages[1]["role"], "user");
            assert_eq!(messages[1]["content"], "3*3?");

            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(
                    r#"{"message":{"role":"assistant","content":"9"},"done":true}"#,
                ))
                .unwrap();
        }
        Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let request = InferenceRequest {
        request_id: None,
        model_id: "ollama/qwen3:8b".to_string(),
        messages: vec![
            ChatMessage {
                role: MessageRole::System,
                content: "You are a concise mathematician".to_string(),
            },
            ChatMessage {
                role: MessageRole::User,
                content: "3*3?".to_string(),
            },
        ],
    };

    let response = registry
        .execute_inference(&provider, request)
        .await
        .expect("system message inference should succeed");

    assert_eq!(response.content, "9");

    let _ = shutdown.send(());
}

#[tokio::test]
async fn test_case_4_exact_upstream_model_identifier_used() {
    let (endpoint, shutdown) = start_mock_ollama_server(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/api/tags" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(
                    r#"{"models":[{"name":"llama3.2:1b-instruct-q4_K_M","modified_at":"2026-03-01T00:00:00Z","size":1200000000}]}"#,
                ))
                .unwrap();
        }
        if path == "/api/show" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(r#"{"capabilities":["chat"]}"#))
                .unwrap();
        }
        if path == "/api/chat" {
            let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
            let val: Value = serde_json::from_slice(&body_bytes).unwrap();
            // Upstream model must be the exact Ollama name, not prefixed with "ollama/"
            assert_eq!(val["model"], "llama3.2:1b-instruct-q4_K_M");

            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(
                    r#"{"message":{"role":"assistant","content":"Done"},"done":true}"#,
                ))
                .unwrap();
        }
        Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let request = InferenceRequest {
        request_id: None,
        model_id: "ollama/llama3.2:1b-instruct-q4_K_M".to_string(),
        messages: vec![ChatMessage {
            role: MessageRole::User,
            content: "Ping".to_string(),
        }],
    };

    let response = registry
        .execute_inference(&provider, request)
        .await
        .expect("exact model inference should succeed");

    // Output model_id must be canonical
    assert_eq!(response.model_id, "ollama/llama3.2:1b-instruct-q4_K_M");

    let _ = shutdown.send(());
}

#[tokio::test]
async fn test_case_5_correct_normalization_of_response_content() {
    let (endpoint, shutdown) = start_mock_ollama_server(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/api/tags" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(
                    r#"{"models":[{"name":"test:1b","modified_at":"2026-03-01T00:00:00Z","size":1000}]}"#,
                ))
                .unwrap();
        }
        if path == "/api/show" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(r#"{"capabilities":["completion"]}"#))
                .unwrap();
        }
        if path == "/api/chat" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(
                    r#"{"message":{"role":"assistant","content":"  Proper response.  "},"done":true,"done_reason":"stop"}"#,
                ))
                .unwrap();
        }
        Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let request = InferenceRequest {
        request_id: None,
        model_id: "ollama/test:1b".to_string(),
        messages: vec![ChatMessage {
            role: MessageRole::User,
            content: "Test".to_string(),
        }],
    };

    let response = registry
        .execute_inference(&provider, request)
        .await
        .unwrap();

    assert_eq!(response.content, "  Proper response.  ");
    assert_eq!(response.finish_reason.as_deref(), Some("stop"));

    let _ = shutdown.send(());
}

#[tokio::test]
async fn test_case_6_usage_metadata_parsed_when_present() {
    let (endpoint, shutdown) = start_mock_ollama_server(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/api/tags" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(
                    r#"{"models":[{"name":"test:1b","modified_at":"2026-03-01T00:00:00Z","size":1000}]}"#,
                ))
                .unwrap();
        }
        if path == "/api/show" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(r#"{"capabilities":["chat"]}"#))
                .unwrap();
        }
        if path == "/api/chat" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(
                    r#"{"message":{"role":"assistant","content":"Result"},"done":true,"prompt_eval_count":15,"eval_count":30}"#,
                ))
                .unwrap();
        }
        Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let request = InferenceRequest {
        request_id: None,
        model_id: "ollama/test:1b".to_string(),
        messages: vec![ChatMessage {
            role: MessageRole::User,
            content: "Test".to_string(),
        }],
    };

    let response = registry
        .execute_inference(&provider, request)
        .await
        .unwrap();

    let usage = response.usage.expect("usage should be parsed");
    assert_eq!(usage.prompt_tokens, Some(15));
    assert_eq!(usage.completion_tokens, Some(30));
    assert_eq!(usage.total_tokens, Some(45));

    let _ = shutdown.send(());
}

#[tokio::test]
async fn test_case_7_unavailable_model_rejected() {
    let (endpoint, shutdown) = start_mock_ollama_server(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/api/tags" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(
                    r#"{"models":[{"name":"qwen3:8b","modified_at":"2026-03-01T00:00:00Z","size":4500000000}]}"#,
                ))
                .unwrap();
        }
        Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let request = InferenceRequest {
        request_id: None,
        model_id: "ollama/ghost-model:latest".to_string(),
        messages: vec![ChatMessage {
            role: MessageRole::User,
            content: "Hello".to_string(),
        }],
    };

    let err = registry
        .execute_inference(&provider, request)
        .await
        .unwrap_err();

    match err {
        InferenceError::ModelNotFound(msg) => {
            assert!(msg.contains("ghost-model"));
        }
        other => panic!("Expected ModelNotFound, got {:?}", other),
    }

    let _ = shutdown.send(());
}

#[tokio::test]
async fn test_case_8_embedding_only_model_rejected_after_capability_inspection() {
    let chat_called = Arc::new(AtomicUsize::new(0));
    let chat_called_clone = chat_called.clone();

    let (endpoint, shutdown) = start_mock_ollama_server(move |req| {
        let chat_called = chat_called_clone.clone();
        async move {
            let path = req.uri().path().to_string();
            if path == "/api/tags" {
                return Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::from(
                        r#"{"models":[{"name":"nomic-embed-text","modified_at":"2026-03-01T00:00:00Z","size":274000000}]}"#,
                    ))
                    .unwrap();
            }
            if path == "/api/show" {
                return Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::from(r#"{"capabilities":["embedding"]}"#))
                    .unwrap();
            }
            if path == "/api/chat" {
                chat_called.fetch_add(1, Ordering::SeqCst);
                return Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::from(r#"{"message":{"role":"assistant","content":"Should not happen"}}"#))
                    .unwrap();
            }
            Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).unwrap()
        }
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let request = InferenceRequest {
        request_id: None,
        model_id: "ollama/nomic-embed-text".to_string(),
        messages: vec![ChatMessage {
            role: MessageRole::User,
            content: "Generate embedding".to_string(),
        }],
    };

    let err = registry
        .execute_inference(&provider, request)
        .await
        .unwrap_err();

    match err {
        InferenceError::UnsupportedCapability(msg) => {
            assert!(msg.contains("does not support text"));
        }
        other => panic!("Expected UnsupportedCapability, got {:?}", other),
    }

    // Crucial check: /api/chat was NEVER called
    assert_eq!(chat_called.load(Ordering::SeqCst), 0);

    let _ = shutdown.send(());
}

#[tokio::test]
async fn test_case_9_provider_http_error_handled() {
    let (endpoint, shutdown) = start_mock_ollama_server(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/api/tags" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(
                    r#"{"models":[{"name":"qwen3:8b","modified_at":"2026-03-01T00:00:00Z","size":4500000000}]}"#,
                ))
                .unwrap();
        }
        if path == "/api/show" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(r#"{"capabilities":["chat"]}"#))
                .unwrap();
        }
        if path == "/api/chat" {
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from("CUDA out of memory error"))
                .unwrap();
        }
        Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let request = InferenceRequest {
        request_id: None,
        model_id: "ollama/qwen3:8b".to_string(),
        messages: vec![ChatMessage {
            role: MessageRole::User,
            content: "Hello".to_string(),
        }],
    };

    let err = registry
        .execute_inference(&provider, request)
        .await
        .unwrap_err();

    match err {
        InferenceError::HttpStatus { code, message } => {
            assert_eq!(code, 500);
            assert!(message.contains("CUDA out of memory"));
        }
        InferenceError::ModelLoadFailure(msg) => {
            assert!(msg.contains("CUDA out of memory"));
        }
        other => panic!("Expected HttpStatus or ModelLoadFailure, got {:?}", other),
    }

    let _ = shutdown.send(());
}

#[tokio::test]
async fn test_case_10_malformed_json_handled() {
    let (endpoint, shutdown) = start_mock_ollama_server(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/api/tags" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(
                    r#"{"models":[{"name":"qwen3:8b","modified_at":"2026-03-01T00:00:00Z","size":4500000000}]}"#,
                ))
                .unwrap();
        }
        if path == "/api/show" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(r#"{"capabilities":["chat"]}"#))
                .unwrap();
        }
        if path == "/api/chat" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from("{broken json payload!"))
                .unwrap();
        }
        Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let request = InferenceRequest {
        request_id: None,
        model_id: "ollama/qwen3:8b".to_string(),
        messages: vec![ChatMessage {
            role: MessageRole::User,
            content: "Hello".to_string(),
        }],
    };

    let err = registry
        .execute_inference(&provider, request)
        .await
        .unwrap_err();

    match err {
        InferenceError::MalformedResponse(msg) => {
            assert!(!msg.is_empty());
        }
        other => panic!("Expected MalformedResponse, got {:?}", other),
    }

    let _ = shutdown.send(());
}

#[tokio::test]
async fn test_case_11_provider_timeout_handled() {
    let (endpoint, shutdown) = start_mock_ollama_server(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/api/tags" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(
                    r#"{"models":[{"name":"qwen3:8b","modified_at":"2026-03-01T00:00:00Z","size":4500000000}]}"#,
                ))
                .unwrap();
        }
        if path == "/api/show" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(r#"{"capabilities":["chat"]}"#))
                .unwrap();
        }
        if path == "/api/chat" {
            // Sleep longer than inference client timeout
            tokio::time::sleep(Duration::from_millis(250)).await;
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(r#"{"message":{"role":"assistant","content":"Late"}}"#))
                .unwrap();
        }
        Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).unwrap()
    })
    .await;

    // Fast 50ms inference timeout
    let provider = OllamaProvider::with_all_timeouts(
        &endpoint,
        Duration::from_millis(50),
        Duration::from_millis(500),
        Duration::from_millis(50),
    );
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let request = InferenceRequest {
        request_id: None,
        model_id: "ollama/qwen3:8b".to_string(),
        messages: vec![ChatMessage {
            role: MessageRole::User,
            content: "Hello".to_string(),
        }],
    };

    let err = registry
        .execute_inference(&provider, request)
        .await
        .unwrap_err();

    match err {
        InferenceError::Timeout(dur) => {
            assert_eq!(dur, Duration::from_millis(50));
        }
        other => panic!("Expected Timeout, got {:?}", other),
    }

    let _ = shutdown.send(());
}

#[tokio::test]
async fn test_case_12_model_capability_cache_works_and_invalidates_correctly() {
    let show_call_count = Arc::new(AtomicUsize::new(0));
    let show_call_count_clone = show_call_count.clone();

    let (endpoint, shutdown) = start_mock_ollama_server(move |req| {
        let show_call_count = show_call_count_clone.clone();
        async move {
            let path = req.uri().path().to_string();
            if path == "/api/tags" {
                return Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::from(
                        r#"{"models":[{"name":"qwen3:8b","modified_at":"2026-03-01T00:00:00Z","size":4500000000}]}"#,
                    ))
                    .unwrap();
            }
            if path == "/api/show" {
                show_call_count.fetch_add(1, Ordering::SeqCst);
                return Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::from(r#"{"capabilities":["chat"]}"#))
                    .unwrap();
            }
            if path == "/api/chat" {
                return Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::from(
                        r#"{"message":{"role":"assistant","content":"Response"},"done":true}"#,
                    ))
                    .unwrap();
            }
            Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).unwrap()
        }
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let make_req = || InferenceRequest {
        request_id: None,
        model_id: "ollama/qwen3:8b".to_string(),
        messages: vec![ChatMessage {
            role: MessageRole::User,
            content: "Hi".to_string(),
        }],
    };

    // First inference: /api/show must be called (count = 1)
    let res1 = registry.execute_inference(&provider, make_req()).await;
    assert!(res1.is_ok());
    assert_eq!(show_call_count.load(Ordering::SeqCst), 1);

    // Second inference for same model: cached, /api/show NOT called (count remains 1)
    let res2 = registry.execute_inference(&provider, make_req()).await;
    assert!(res2.is_ok());
    assert_eq!(show_call_count.load(Ordering::SeqCst), 1);

    // Explicit cache invalidation
    provider.clear_capabilities_cache();

    // Third inference: /api/show called again after invalidation (count = 2)
    let res3 = registry.execute_inference(&provider, make_req()).await;
    assert!(res3.is_ok());
    assert_eq!(show_call_count.load(Ordering::SeqCst), 2);

    let _ = shutdown.send(());
}

#[tokio::test]
async fn test_case_13_concurrent_inference_does_not_block_registry_reads() {
    let (endpoint, shutdown) = start_mock_ollama_server(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/api/tags" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(
                    r#"{"models":[{"name":"qwen3:8b","modified_at":"2026-03-01T00:00:00Z","size":4500000000}]}"#,
                ))
                .unwrap();
        }
        if path == "/api/show" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(r#"{"capabilities":["chat"]}"#))
                .unwrap();
        }
        if path == "/api/chat" {
            // Emulate slow model inference taking 300ms
            tokio::time::sleep(Duration::from_millis(300)).await;
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(
                    r#"{"message":{"role":"assistant","content":"Slow answer"},"done":true}"#,
                ))
                .unwrap();
        }
        Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).unwrap()
    })
    .await;

    let provider = Arc::new(OllamaProvider::new(&endpoint));
    let registry = Arc::new(ModelRegistry::new());
    registry.refresh(&*provider).await;

    let provider_clone = provider.clone();
    let registry_clone = registry.clone();

    // Spawn long-running inference in background
    let inference_handle = tokio::spawn(async move {
        let request = InferenceRequest {
            request_id: None,
            model_id: "ollama/qwen3:8b".to_string(),
            messages: vec![ChatMessage {
                role: MessageRole::User,
                content: "Think deeply".to_string(),
            }],
        };
        registry_clone
            .execute_inference(&provider_clone, request)
            .await
    });

    // Wait a brief 20ms for inference to start and enter network I/O
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Immediately read registry snapshot while inference is in flight
    let start_read = std::time::Instant::now();
    let snapshot = registry.get_snapshot();
    let read_duration = start_read.elapsed();

    // Registry read must not be blocked by the in-flight inference (should take < 20ms, far less than 300ms)
    assert!(
        read_duration < Duration::from_millis(50),
        "Registry read was blocked for {:?} during inference",
        read_duration
    );
    assert_eq!(snapshot.status, ProviderStatus::Ready);
    assert_eq!(snapshot.models.len(), 1);

    // Ensure inference eventually finishes cleanly
    let inference_res = inference_handle.await.unwrap();
    assert!(inference_res.is_ok());

    let _ = shutdown.send(());
}

#[tokio::test]
async fn test_case_14_direct_model_inference_trait() {
    let (endpoint, shutdown) = start_mock_ollama_server(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/api/chat" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(
                    r#"{"message":{"role":"assistant","content":"Direct trait works"},"done":true}"#,
                ))
                .unwrap();
        }
        Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let request = InferenceRequest {
        request_id: None,
        model_id: "ollama/qwen3:8b".to_string(),
        messages: vec![ChatMessage {
            role: MessageRole::User,
            content: "Direct test".to_string(),
        }],
    };

    let response = provider
        .generate(request)
        .await
        .expect("ModelInference trait generate should succeed");

    assert_eq!(response.content, "Direct trait works");

    let _ = shutdown.send(());
}

#[tokio::test]
async fn test_case_15_ipc_json_roundtrip_and_serialization() {
    let (endpoint, shutdown) = start_mock_ollama_server(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/api/tags" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(
                    r#"{"models":[{"name":"qwen3:8b","modified_at":"2026-03-01T00:00:00Z","size":4500000000}]}"#,
                ))
                .unwrap();
        }
        if path == "/api/show" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(r#"{"capabilities":["chat"]}"#))
                .unwrap();
        }
        if path == "/api/chat" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(
                    r#"{"message":{"role":"assistant","content":"JSON roundtrip verified"},"done":true,"prompt_eval_count":8,"eval_count":16}"#,
                ))
                .unwrap();
        }
        Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    // Simulate exact JSON input sent by React UI invoke('generate_test_completion', { request })
    let frontend_json = serde_json::json!({
        "model_id": "ollama/qwen3:8b",
        "messages": [
            {
                "role": "user",
                "content": "Explain AI"
            }
        ]
    });

    let deserialized_request: InferenceRequest =
        serde_json::from_value(frontend_json).expect("Request must deserialize from frontend JSON");

    let response = registry
        .execute_inference(&provider, deserialized_request)
        .await
        .expect("Inference should succeed");

    // Serialize to JSON as returned to React UI
    let response_json = serde_json::to_value(&response).expect("Response must serialize to JSON");

    assert_eq!(response_json["model_id"], "ollama/qwen3:8b");
    assert_eq!(response_json["content"], "JSON roundtrip verified");
    assert!(response_json["duration_ms"].is_number());
    assert_eq!(response_json["usage"]["prompt_tokens"], 8);
    assert_eq!(response_json["usage"]["completion_tokens"], 16);
    assert_eq!(response_json["usage"]["total_tokens"], 24);

    let _ = shutdown.send(());
}

#[tokio::test]
async fn test_case_16_structured_error_payload_conversion() {
    use mlue_lib::inference::InferenceErrorPayload;

    // Test conversion of various error variants
    let err1 = InferenceError::ProviderUnreachable("connection refused".to_string());
    let payload1 = InferenceErrorPayload::from(err1);
    assert_eq!(payload1.code, "PROVIDER_UNREACHABLE");
    assert!(payload1.message.contains("Ollama couldn't be reached"));
    assert_eq!(
        payload1.technical_details.as_deref(),
        Some("connection refused")
    );

    let err2 = InferenceError::UnsupportedCapability("embedding only".to_string());
    let payload2 = InferenceErrorPayload::from(err2);
    assert_eq!(payload2.code, "UNSUPPORTED_CAPABILITY");
    assert!(payload2.message.contains("doesn't support text chat"));

    let err3 = InferenceError::Timeout(Duration::from_secs(60));
    let payload3 = InferenceErrorPayload::from(err3);
    assert_eq!(payload3.code, "TIMEOUT");
    assert!(payload3.message.contains("took too long to respond"));

    let err4 = InferenceError::ModelNotFound("qwen3:8b".to_string());
    let payload4 = InferenceErrorPayload::from(err4);
    assert_eq!(payload4.code, "MODEL_NOT_FOUND");

    // Verify JSON serialization matches frontend InferenceErrorState interface
    let payload_json = serde_json::to_value(&payload1).unwrap();
    assert_eq!(payload_json["code"], "PROVIDER_UNREACHABLE");
    assert!(payload_json["message"].is_string());
    assert!(payload_json["technical_details"].is_string());
}

#[tokio::test]
async fn test_case_17_request_limits_validation() {
    let provider = OllamaProvider::new("http://127.0.0.1:11434");
    let registry = ModelRegistry::new();

    // 1. Empty messages rejected
    let empty_req = InferenceRequest {
        request_id: None,
        model_id: "ollama/qwen3:8b".to_string(),
        messages: vec![],
    };
    let err_empty = registry
        .execute_inference(&provider, empty_req)
        .await
        .unwrap_err();
    match err_empty {
        InferenceError::InvalidRequest(msg) => assert!(msg.contains("cannot be empty")),
        other => panic!("Expected InvalidRequest, got {:?}", other),
    }

    // 2. Too many messages rejected (> 50)
    let too_many_messages: Vec<ChatMessage> = (0..51)
        .map(|i| ChatMessage {
            role: MessageRole::User,
            content: format!("msg {}", i),
        })
        .collect();
    let excessive_req = InferenceRequest {
        request_id: None,
        model_id: "ollama/qwen3:8b".to_string(),
        messages: too_many_messages,
    };
    let err_excessive = registry
        .execute_inference(&provider, excessive_req)
        .await
        .unwrap_err();
    match err_excessive {
        InferenceError::InvalidRequest(msg) => assert!(msg.contains("Too many messages")),
        other => panic!("Expected InvalidRequest, got {:?}", other),
    }

    // 3. Single message content > 64 KiB rejected
    let big_single_req = InferenceRequest {
        request_id: None,
        model_id: "ollama/qwen3:8b".to_string(),
        messages: vec![ChatMessage {
            role: MessageRole::User,
            content: "x".repeat(65 * 1024),
        }],
    };
    let err_single = registry
        .execute_inference(&provider, big_single_req)
        .await
        .unwrap_err();
    match err_single {
        InferenceError::InvalidRequest(msg) => {
            assert!(msg.contains("64 KiB") && msg.contains("exceeds"));
        }
        other => panic!("Expected InvalidRequest, got {:?}", other),
    }

    // 4. Combined message content > 128 KiB rejected
    let big_total_req = InferenceRequest {
        request_id: None,
        model_id: "ollama/qwen3:8b".to_string(),
        messages: vec![
            ChatMessage {
                role: MessageRole::User,
                content: "x".repeat(60 * 1024),
            },
            ChatMessage {
                role: MessageRole::User,
                content: "y".repeat(60 * 1024),
            },
            ChatMessage {
                role: MessageRole::User,
                content: "z".repeat(10 * 1024),
            },
        ],
    };
    let err_total = registry
        .execute_inference(&provider, big_total_req)
        .await
        .unwrap_err();
    match err_total {
        InferenceError::InvalidRequest(msg) => {
            assert!(msg.contains("128 KiB") && msg.contains("exceeds"));
        }
        other => panic!("Expected InvalidRequest, got {:?}", other),
    }
}
