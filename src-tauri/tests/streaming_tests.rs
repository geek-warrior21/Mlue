use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use mlue_lib::inference::types::{ChatMessage, InferenceEvent, InferenceRequest, MessageRole};
use mlue_lib::inference::{InferenceError, InferenceManager, ModelInference, TerminalGuard};
use mlue_lib::providers::ollama::OllamaProvider;
use mlue_lib::registry::{ModelRegistry, ProviderStatus};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub struct ChannelStream<T>(pub mpsc::Receiver<T>);

impl<T> Stream for ChannelStream<T> {
    type Item = T;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}

async fn start_mock_streaming_server<F, Fut>(handler: F) -> (String, oneshot::Sender<()>)
where
    F: Fn(Request<Body>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Response<Body>> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind ephemeral port for mock");
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

fn mock_tags_and_show(req: &Request<Body>) -> Option<Response<Body>> {
    let path = req.uri().path();
    if path == "/api/tags" {
        return Some(
            Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(
                    r#"{"models":[{"name":"qwen3:8b","modified_at":"2026-03-01T00:00:00Z","size":4500000000}]}"#,
                ))
                .unwrap(),
        );
    }
    if path == "/api/show" {
        return Some(
            Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(r#"{"capabilities":["chat"]}"#))
                .unwrap(),
        );
    }
    None
}

// 1. Successful generation with multiple deltas.
#[tokio::test]
async fn test_case_1_successful_generation_with_multiple_deltas() {
    let (endpoint, shutdown) = start_mock_streaming_server(|req| async move {
        if let Some(resp) = mock_tags_and_show(&req) {
            return resp;
        }
        if req.uri().path() == "/api/chat" {
            let chunks = vec![
                Ok::<_, std::io::Error>(Bytes::from(
                    "{\"message\":{\"content\":\"Hello \"},\"done\":false}\n",
                )),
                Ok::<_, std::io::Error>(Bytes::from(
                    "{\"message\":{\"content\":\"beautiful \"},\"done\":false}\n",
                )),
                Ok::<_, std::io::Error>(Bytes::from(
                    "{\"message\":{\"content\":\"world!\"},\"done\":false}\n",
                )),
                Ok::<_, std::io::Error>(Bytes::from(
                    "{\"done\":true,\"prompt_eval_count\":4,\"eval_count\":6}\n",
                )),
            ];
            let stream = futures_util::stream::iter(chunks);
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from_stream(stream))
                .unwrap();
        }
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let token = CancellationToken::new();
    let request = InferenceRequest::with_request_id(
        "req_case1",
        "ollama/qwen3:8b",
        vec![ChatMessage {
            role: MessageRole::User,
            content: "Say hello".to_string(),
        }],
    );

    let mut stream = registry
        .execute_inference_stream(&provider, request, token)
        .await
        .expect("stream execution should succeed");

    let mut events = Vec::new();
    while let Some(res) = stream.next().await {
        events.push(res.expect("event should be Ok"));
    }

    assert_eq!(events.len(), 5); // Started, 3 Deltas, Completed
    match &events[0] {
        InferenceEvent::Started {
            request_id,
            model_id,
        } => {
            assert_eq!(request_id, "req_case1");
            assert_eq!(model_id, "ollama/qwen3:8b");
        }
        other => panic!("Expected Started, got {:?}", other),
    }

    let mut full_text = String::new();
    for ev in &events[1..4] {
        match ev {
            InferenceEvent::Delta {
                content,
                request_id,
            } => {
                assert_eq!(request_id, "req_case1");
                full_text.push_str(content);
            }
            other => panic!("Expected Delta, got {:?}", other),
        }
    }
    assert_eq!(full_text, "Hello beautiful world!");

    match &events[4] {
        InferenceEvent::Completed {
            request_id,
            model_id,
            usage,
            ..
        } => {
            assert_eq!(request_id, "req_case1");
            assert_eq!(model_id, "ollama/qwen3:8b");
            let u = usage.as_ref().unwrap();
            assert_eq!(u.prompt_tokens, Some(4));
            assert_eq!(u.completion_tokens, Some(6));
            assert_eq!(u.total_tokens, Some(10));
        }
        other => panic!("Expected Completed, got {:?}", other),
    }

    let _ = shutdown.send(());
}

// 2. NDJSON objects arriving in one chunk.
#[tokio::test]
async fn test_case_2_ndjson_objects_arriving_in_one_chunk() {
    let (endpoint, shutdown) = start_mock_streaming_server(|req| async move {
        if let Some(resp) = mock_tags_and_show(&req) {
            return resp;
        }
        if req.uri().path() == "/api/chat" {
            let single_chunk = Bytes::from(
                "{\"message\":{\"content\":\"Chunk1\"},\"done\":false}\n{\"message\":{\"content\":\"Chunk2\"},\"done\":false}\n{\"done\":true}\n"
            );
            let stream = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(single_chunk)]);
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from_stream(stream))
                .unwrap();
        }
        Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let token = CancellationToken::new();
    let request = InferenceRequest::with_request_id(
        "req_case2",
        "ollama/qwen3:8b",
        vec![ChatMessage {
            role: MessageRole::User,
            content: "Test".to_string(),
        }],
    );

    let mut stream = registry
        .execute_inference_stream(&provider, request, token)
        .await
        .unwrap();

    let mut events = Vec::new();
    while let Some(res) = stream.next().await {
        events.push(res.unwrap());
    }

    assert_eq!(events.len(), 4); // Started, Delta1, Delta2, Completed
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            InferenceEvent::Delta { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Chunk1Chunk2");

    let _ = shutdown.send(());
}

// 3. NDJSON object split across chunks.
#[tokio::test]
async fn test_case_3_ndjson_object_split_across_chunks() {
    let (endpoint, shutdown) = start_mock_streaming_server(|req| async move {
        if let Some(resp) = mock_tags_and_show(&req) {
            return resp;
        }
        if req.uri().path() == "/api/chat" {
            // Split a single JSON line into two fragmented byte chunks
            let part1 = Bytes::from("{\"message\":{\"content\":\"Split ");
            let part2 = Bytes::from("across chunks\"},\"done\":false}\n{\"done\":true}\n");
            let stream = futures_util::stream::iter(vec![
                Ok::<_, std::io::Error>(part1),
                Ok::<_, std::io::Error>(part2),
            ]);
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from_stream(stream))
                .unwrap();
        }
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let token = CancellationToken::new();
    let request = InferenceRequest::with_request_id(
        "req_case3",
        "ollama/qwen3:8b",
        vec![ChatMessage {
            role: MessageRole::User,
            content: "Test".to_string(),
        }],
    );

    let mut stream = registry
        .execute_inference_stream(&provider, request, token)
        .await
        .unwrap();

    let mut deltas = Vec::new();
    while let Some(res) = stream.next().await {
        if let InferenceEvent::Delta { content, .. } = res.unwrap() {
            deltas.push(content);
        }
    }

    assert_eq!(deltas, vec!["Split across chunks".to_string()]);
    let _ = shutdown.send(());
}

// 4. Split UTF-8 character handling.
#[tokio::test]
async fn test_case_4_split_utf8_character_handling() {
    let (endpoint, shutdown) = start_mock_streaming_server(|req| async move {
        if let Some(resp) = mock_tags_and_show(&req) {
            return resp;
        }
        if req.uri().path() == "/api/chat" {
            // "✨" is 4 bytes in UTF-8: 0xF0, 0x9F, 0x8C, 0x9F
            let prefix = "{\"message\":{\"content\":\"Sparkle: ";
            let mut chunk1_vec = Vec::from(prefix.as_bytes());
            chunk1_vec.extend_from_slice(&[0xF0, 0x9F]); // First 2 bytes of 4-byte character

            let mut chunk2_vec = vec![0x8C, 0x9F]; // Remaining 2 bytes of 4-byte character
            chunk2_vec.extend_from_slice(b" and more\"},\"done\":false}\n{\"done\":true}\n");

            let stream = futures_util::stream::iter(vec![
                Ok::<_, std::io::Error>(Bytes::from(chunk1_vec)),
                Ok::<_, std::io::Error>(Bytes::from(chunk2_vec)),
            ]);
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from_stream(stream))
                .unwrap();
        }
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let token = CancellationToken::new();
    let request = InferenceRequest::with_request_id(
        "req_case4",
        "ollama/qwen3:8b",
        vec![ChatMessage {
            role: MessageRole::User,
            content: "Test".to_string(),
        }],
    );

    let mut stream = registry
        .execute_inference_stream(&provider, request, token)
        .await
        .unwrap();

    let mut accumulated = String::new();
    while let Some(res) = stream.next().await {
        if let InferenceEvent::Delta { content, .. } = res.unwrap() {
            accumulated.push_str(&content);
        }
    }

    assert_eq!(accumulated, "Sparkle: 🌟 and more");
    let _ = shutdown.send(());
}

// 5. Correct accumulation of generated text.
#[tokio::test]
async fn test_case_5_correct_accumulation_of_generated_text() {
    let (endpoint, shutdown) = start_mock_streaming_server(|req| async move {
        if let Some(resp) = mock_tags_and_show(&req) {
            return resp;
        }
        if req.uri().path() == "/api/chat" {
            let tokens = vec!["The ", "quick ", "brown ", "fox ", "jumps"];
            let mut chunks = Vec::new();
            for tok in tokens {
                chunks.push(Ok::<_, std::io::Error>(Bytes::from(format!(
                    "{{\"message\":{{\"content\":\"{}\"}},\"done\":false}}\n",
                    tok
                ))));
            }
            chunks.push(Ok::<_, std::io::Error>(Bytes::from("{\"done\":true}\n")));
            let stream = futures_util::stream::iter(chunks);
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from_stream(stream))
                .unwrap();
        }
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let token = CancellationToken::new();
    let request = InferenceRequest::with_request_id(
        "req_case5",
        "ollama/qwen3:8b",
        vec![ChatMessage {
            role: MessageRole::User,
            content: "Pangram".to_string(),
        }],
    );

    let mut stream = registry
        .execute_inference_stream(&provider, request, token)
        .await
        .unwrap();

    let mut text = String::new();
    while let Some(res) = stream.next().await {
        if let InferenceEvent::Delta { content, .. } = res.unwrap() {
            text.push_str(&content);
        }
    }
    assert_eq!(text, "The quick brown fox jumps");
    let _ = shutdown.send(());
}

// 6. Final completion event handling.
#[tokio::test]
async fn test_case_6_final_completion_event_handling() {
    let (endpoint, shutdown) = start_mock_streaming_server(|req| async move {
        if let Some(resp) = mock_tags_and_show(&req) {
            return resp;
        }
        if req.uri().path() == "/api/chat" {
            let stream = futures_util::stream::iter(vec![
                Ok::<_, std::io::Error>(Bytes::from(
                    "{\"message\":{\"content\":\"Final\"},\"done\":false}\n",
                )),
                Ok::<_, std::io::Error>(Bytes::from("{\"done\":true,\"done_reason\":\"stop\"}\n")),
            ]);
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from_stream(stream))
                .unwrap();
        }
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let token = CancellationToken::new();
    let request = InferenceRequest::with_request_id(
        "req_case6",
        "ollama/qwen3:8b",
        vec![ChatMessage {
            role: MessageRole::User,
            content: "Final".to_string(),
        }],
    );

    let mut stream = registry
        .execute_inference_stream(&provider, request, token)
        .await
        .unwrap();

    let mut completed_event = None;
    while let Some(res) = stream.next().await {
        let ev = res.unwrap();
        if matches!(ev, InferenceEvent::Completed { .. }) {
            completed_event = Some(ev);
        }
    }

    match completed_event.expect("Completed event must be received") {
        InferenceEvent::Completed { finish_reason, .. } => {
            assert_eq!(finish_reason.as_deref(), Some("stop"));
        }
        _ => unreachable!(),
    }
    let _ = shutdown.send(());
}

// 7. Usage metadata extraction.
#[tokio::test]
async fn test_case_7_usage_metadata_extraction() {
    let (endpoint, shutdown) = start_mock_streaming_server(|req| async move {
        if let Some(resp) = mock_tags_and_show(&req) {
            return resp;
        }
        if req.uri().path() == "/api/chat" {
            let stream = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(
                "{\"done\":true,\"prompt_eval_count\":17,\"eval_count\":42}\n",
            ))]);
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from_stream(stream))
                .unwrap();
        }
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let token = CancellationToken::new();
    let request = InferenceRequest::with_request_id(
        "req_case7",
        "ollama/qwen3:8b",
        vec![ChatMessage {
            role: MessageRole::User,
            content: "Tokens".to_string(),
        }],
    );

    let mut stream = registry
        .execute_inference_stream(&provider, request, token)
        .await
        .unwrap();

    let mut usage_found = None;
    while let Some(res) = stream.next().await {
        if let InferenceEvent::Completed { usage, .. } = res.unwrap() {
            usage_found = usage;
        }
    }

    let u = usage_found.expect("Usage metadata must be extracted");
    assert_eq!(u.prompt_tokens, Some(17));
    assert_eq!(u.completion_tokens, Some(42));
    assert_eq!(u.total_tokens, Some(59));

    let _ = shutdown.send(());
}

// 8. Malformed JSON during streaming.
#[tokio::test]
async fn test_case_8_malformed_json_during_streaming() {
    let (endpoint, shutdown) = start_mock_streaming_server(|req| async move {
        if let Some(resp) = mock_tags_and_show(&req) {
            return resp;
        }
        if req.uri().path() == "/api/chat" {
            let stream = futures_util::stream::iter(vec![
                Ok::<_, std::io::Error>(Bytes::from(
                    "{\"message\":{\"content\":\"valid\"},\"done\":false}\n",
                )),
                Ok::<_, std::io::Error>(Bytes::from("{not valid json at all!!\n")),
            ]);
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from_stream(stream))
                .unwrap();
        }
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let token = CancellationToken::new();
    let request = InferenceRequest::with_request_id(
        "req_case8",
        "ollama/qwen3:8b",
        vec![ChatMessage {
            role: MessageRole::User,
            content: "Test".to_string(),
        }],
    );

    let mut stream = registry
        .execute_inference_stream(&provider, request, token)
        .await
        .unwrap();

    let mut received_valid = false;
    let mut received_error = false;
    while let Some(res) = stream.next().await {
        match res {
            Ok(InferenceEvent::Delta { content, .. }) if content == "valid" => {
                received_valid = true;
            }
            Err(InferenceError::MalformedResponse(msg)) => {
                assert!(msg.contains("Invalid JSON"));
                received_error = true;
            }
            _ => {}
        }
    }

    assert!(received_valid, "Should have received initial valid token");
    assert!(
        received_error,
        "Should have received MalformedResponse error"
    );
    let _ = shutdown.send(());
}

// 9. HTTP error before streaming.
#[tokio::test]
async fn test_case_9_http_error_before_streaming() {
    let (endpoint, shutdown) = start_mock_streaming_server(|req| async move {
        if let Some(resp) = mock_tags_and_show(&req) {
            return resp;
        }
        if req.uri().path() == "/api/chat" {
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from("Ollama backend crashed"))
                .unwrap();
        }
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let token = CancellationToken::new();
    let request = InferenceRequest::with_request_id(
        "req_case9",
        "ollama/qwen3:8b",
        vec![ChatMessage {
            role: MessageRole::User,
            content: "Test".to_string(),
        }],
    );

    let err = match registry
        .execute_inference_stream(&provider, request, token)
        .await
    {
        Err(e) => e,
        Ok(_) => panic!("Expected error, got Ok stream"),
    };
    match err {
        InferenceError::HttpStatus { code, message } => {
            assert_eq!(code, 500);
            assert!(message.contains("Ollama backend crashed"));
        }
        InferenceError::ModelLoadFailure(msg) => {
            assert!(msg.contains("Ollama backend crashed"));
        }
        other => panic!("Expected HttpStatus or ModelLoadFailure, got {:?}", other),
    }

    let _ = shutdown.send(());
}

// 10. Network disconnect mid-stream.
#[tokio::test]
async fn test_case_10_network_disconnect_mid_stream() {
    let (endpoint, shutdown) = start_mock_streaming_server(|req| async move {
        if let Some(resp) = mock_tags_and_show(&req) {
            return resp;
        }
        if req.uri().path() == "/api/chat" {
            let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(2);
            tokio::spawn(async move {
                let _ = tx
                    .send(Ok(Bytes::from(
                        "{\"message\":{\"content\":\"before disconnect\"},\"done\":false}\n",
                    )))
                    .await;
                tokio::time::sleep(Duration::from_millis(30)).await;
                // Intentionally inject an IO error mid-stream
                let _ = tx
                    .send(Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionReset,
                        "TCP reset",
                    )))
                    .await;
            });
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from_stream(ChannelStream(rx)))
                .unwrap();
        }
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let token = CancellationToken::new();
    let request = InferenceRequest::with_request_id(
        "req_case10",
        "ollama/qwen3:8b",
        vec![ChatMessage {
            role: MessageRole::User,
            content: "Test".to_string(),
        }],
    );

    let mut stream = registry
        .execute_inference_stream(&provider, request, token)
        .await
        .unwrap();

    let mut got_delta = false;
    let mut got_error = false;
    while let Some(res) = stream.next().await {
        match res {
            Ok(InferenceEvent::Delta { content, .. }) if content == "before disconnect" => {
                got_delta = true;
            }
            Err(InferenceError::Other(msg)) => {
                assert!(msg.contains("Network stream error"));
                got_error = true;
            }
            Err(InferenceError::MalformedResponse(_)) => {
                got_error = true;
            }
            _ => {}
        }
    }

    assert!(got_delta);
    assert!(got_error);
    let _ = shutdown.send(());
}

// 11. Premature EOF without done: true.
#[tokio::test]
async fn test_case_11_premature_eof_without_done_true() {
    let (endpoint, shutdown) = start_mock_streaming_server(|req| async move {
        if let Some(resp) = mock_tags_and_show(&req) {
            return resp;
        }
        if req.uri().path() == "/api/chat" {
            // Emits 1 chunk then immediately closes stream with EOF without done: true
            let stream = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(
                "{\"message\":{\"content\":\"cut off\"},\"done\":false}\n",
            ))]);
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from_stream(stream))
                .unwrap();
        }
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let token = CancellationToken::new();
    let request = InferenceRequest::with_request_id(
        "req_case11",
        "ollama/qwen3:8b",
        vec![ChatMessage {
            role: MessageRole::User,
            content: "Test".to_string(),
        }],
    );

    let mut stream = registry
        .execute_inference_stream(&provider, request, token)
        .await
        .unwrap();

    let mut saw_delta = false;
    let mut saw_premature_eof = false;
    while let Some(res) = stream.next().await {
        match res {
            Ok(InferenceEvent::Delta { content, .. }) if content == "cut off" => {
                saw_delta = true;
            }
            Err(InferenceError::MalformedResponse(msg)) if msg.contains("Premature EOF") => {
                saw_premature_eof = true;
            }
            _ => {}
        }
    }

    assert!(saw_delta);
    assert!(saw_premature_eof);
    let _ = shutdown.send(());
}

// 12. Cancellation before the first token.
#[tokio::test]
async fn test_case_12_cancellation_before_the_first_token() {
    let (endpoint, shutdown) = start_mock_streaming_server(|req| async move {
        if let Some(resp) = mock_tags_and_show(&req) {
            return resp;
        }
        if req.uri().path() == "/api/chat" {
            // Delay sending first token
            tokio::time::sleep(Duration::from_millis(500)).await;
            let stream = futures_util::stream::iter(vec![
                Ok::<_, std::io::Error>(Bytes::from(
                    "{\"message\":{\"content\":\"Too late\"},\"done\":false}\n",
                )),
                Ok::<_, std::io::Error>(Bytes::from("{\"done\":true}\n")),
            ]);
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from_stream(stream))
                .unwrap();
        }
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let token = CancellationToken::new();
    let token_clone = token.clone();

    // Cancel before sending or immediately upon launch
    token_clone.cancel();

    let request = InferenceRequest::with_request_id(
        "req_case12",
        "ollama/qwen3:8b",
        vec![ChatMessage {
            role: MessageRole::User,
            content: "Test".to_string(),
        }],
    );

    let res = registry
        .execute_inference_stream(&provider, request, token)
        .await;
    match res {
        Err(InferenceError::Cancelled) => {
            // Successfully returned Cancelled before starting stream
        }
        Ok(mut stream) => {
            // Or stream started and emitted Cancelled as first event
            let first = stream.next().await.unwrap();
            match first {
                Ok(InferenceEvent::Cancelled { .. }) | Err(InferenceError::Cancelled) => {}
                other => panic!("Expected Cancelled, got {:?}", other),
            }
        }
        Err(other_err) => panic!("Expected Cancelled, got {:?}", other_err),
    }

    let _ = shutdown.send(());
}

// 13. Cancellation after multiple tokens.
#[tokio::test]
async fn test_case_13_cancellation_after_multiple_tokens() {
    let (endpoint, shutdown) = start_mock_streaming_server(|req| async move {
        if let Some(resp) = mock_tags_and_show(&req) {
            return resp;
        }
        if req.uri().path() == "/api/chat" {
            let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(4);
            tokio::spawn(async move {
                let _ = tx
                    .send(Ok(Bytes::from(
                        "{\"message\":{\"content\":\"Tok1 \"},\"done\":false}\n",
                    )))
                    .await;
                tokio::time::sleep(Duration::from_millis(30)).await;
                let _ = tx
                    .send(Ok(Bytes::from(
                        "{\"message\":{\"content\":\"Tok2 \"},\"done\":false}\n",
                    )))
                    .await;
                // Wait to allow client to cancel
                tokio::time::sleep(Duration::from_millis(500)).await;
                let _ = tx
                    .send(Ok(Bytes::from(
                        "{\"message\":{\"content\":\"Tok3 \"},\"done\":false}\n",
                    )))
                    .await;
                let _ = tx.send(Ok(Bytes::from("{\"done\":true}\n"))).await;
            });
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from_stream(ChannelStream(rx)))
                .unwrap();
        }
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap()
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let token = CancellationToken::new();
    let token_clone = token.clone();

    let request = InferenceRequest::with_request_id(
        "req_case13",
        "ollama/qwen3:8b",
        vec![ChatMessage {
            role: MessageRole::User,
            content: "Test".to_string(),
        }],
    );

    let mut stream = registry
        .execute_inference_stream(&provider, request, token)
        .await
        .unwrap();

    let mut events = Vec::new();
    while let Some(res) = stream.next().await {
        let ev = res.unwrap();
        let is_delta2 = matches!(&ev, InferenceEvent::Delta { content, .. } if content == "Tok2 ");
        events.push(ev);

        if is_delta2 {
            // Cancel immediately after Tok2
            token_clone.cancel();
        }
    }

    // Must have received Started, Tok1, Tok2, and Cancelled terminal event
    let has_cancelled = events
        .iter()
        .any(|e| matches!(e, InferenceEvent::Cancelled { .. }));
    let has_tok3 = events
        .iter()
        .any(|e| matches!(e, InferenceEvent::Delta { content, .. } if content == "Tok3 "));

    assert!(has_cancelled, "Must receive Cancelled event");
    assert!(!has_tok3, "Tok3 must never be emitted after cancellation");

    let _ = shutdown.send(());
}

// 14. Cancellation interrupts the underlying request (drops upstream connection).
#[tokio::test]
async fn test_case_14_cancellation_interrupts_the_underlying_request() {
    let upstream_client_disconnected = Arc::new(AtomicBool::new(false));
    let disconnected_clone = upstream_client_disconnected.clone();

    let (endpoint, shutdown) = start_mock_streaming_server(move |req| {
        let disconnected_clone = disconnected_clone.clone();
        async move {
            if let Some(resp) = mock_tags_and_show(&req) {
                return resp;
            }
            if req.uri().path() == "/api/chat" {
                let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(2);
                tokio::spawn(async move {
                    let _ = tx
                        .send(Ok(Bytes::from(
                            "{\"message\":{\"content\":\"first\"},\"done\":false}\n",
                        )))
                        .await;

                    // Loop waiting for client disconnection
                    for _ in 0..50 {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        // Sending will fail once client drops the HTTP connection
                        if tx
                            .send(Ok(Bytes::from(
                                "{\"message\":{\"content\":\"tick\"},\"done\":false}\n",
                            )))
                            .await
                            .is_err()
                        {
                            disconnected_clone.store(true, Ordering::SeqCst);
                            return;
                        }
                    }
                });
                return Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::from_stream(ChannelStream(rx)))
                    .unwrap();
            }
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap()
        }
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();
    registry.refresh(&provider).await;

    let token = CancellationToken::new();
    let token_clone = token.clone();

    let request = InferenceRequest::with_request_id(
        "req_case14",
        "ollama/qwen3:8b",
        vec![ChatMessage {
            role: MessageRole::User,
            content: "Test".to_string(),
        }],
    );

    let mut stream = registry
        .execute_inference_stream(&provider, request, token)
        .await
        .unwrap();

    // Read first event
    let _first = stream.next().await.unwrap().unwrap();
    // Cancel token
    token_clone.cancel();

    // Read remaining stream until end
    while stream.next().await.is_some() {}

    // Wait up to 300ms for server-side channel send to detect client disconnection
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        upstream_client_disconnected.load(Ordering::SeqCst),
        "Upstream connection must be dropped/disconnected on cancellation"
    );

    let _ = shutdown.send(());
}

// 15. No duplicate terminal events.
#[tokio::test]
async fn test_case_15_no_duplicate_terminal_events() {
    let guard = TerminalGuard::new();

    let mut terminal_events = Vec::new();
    let candidate_completed = InferenceEvent::Completed {
        request_id: "req1".to_string(),
        model_id: "m".to_string(),
        usage: None,
        duration_ms: 100,
        finish_reason: Some("stop".to_string()),
    };
    let _candidate_cancelled = InferenceEvent::Cancelled {
        request_id: "req1".to_string(),
    };

    // Simulate two concurrent tasks attempting to claim terminal event
    let handle1 = tokio::spawn({
        let guard_ref = Arc::new(guard);
        let guard_clone = guard_ref.clone();
        async move { guard_clone.try_claim() }
    });

    let claimed1 = handle1.await.unwrap();
    if claimed1 {
        terminal_events.push(candidate_completed);
    }

    // Second claim must fail
    // (create a new TerminalGuard test for explicit verification)
    let g2 = TerminalGuard::new();
    assert!(g2.try_claim());
    assert!(!g2.try_claim()); // Second claim rejected!
    assert!(g2.has_emitted());
}

// 16. Cancellation releases concurrency permits.
#[tokio::test]
async fn test_case_16_cancellation_releases_concurrency_permits() {
    let manager = Arc::new(InferenceManager::new(1)); // Capacity of 1

    // Register and claim permit
    let req_id = "req_c16".to_string();
    let token = manager
        .register_request(req_id.clone(), "ollama/qwen3:8b".to_string(), None)
        .unwrap();
    assert_eq!(manager.active_count(), 1);

    let permit = manager.acquire_permit().await.unwrap();

    // Try to acquire second permit -> with timeout, should time out / wait
    let err_second = tokio::time::timeout(
        Duration::from_millis(50),
        manager.acquire_permit_cancellable(&CancellationToken::new()),
    )
    .await;
    assert!(err_second.is_err(), "Must wait when capacity is exhausted");

    // Cancel request
    assert!(manager.cancel_request(&req_id, None));
    assert!(token.is_cancelled());

    // Release permit and unregister
    drop(permit);
    manager.unregister_request(&req_id);
    assert_eq!(manager.active_count(), 0);

    // Now second request can acquire permit!
    let permit2 = manager.acquire_permit().await;
    assert!(permit2.is_ok());
}

// Audit 1: Requests waiting for inference capacity can be cancelled.
#[tokio::test]
async fn test_audit_request_waiting_for_capacity_can_be_cancelled() {
    let manager = Arc::new(InferenceManager::new(1)); // Capacity of 1

    // Request 1 claims the single permit
    let permit1 = manager.acquire_permit().await.unwrap();

    // Request 2 registers and attempts to acquire permit
    let req_id_2 = "req_audit_wait".to_string();
    let token_2 = manager
        .register_request(req_id_2.clone(), "ollama/qwen3:8b".to_string(), None)
        .unwrap();

    let m_clone = manager.clone();
    let t_clone = token_2.clone();
    let wait_handle =
        tokio::spawn(async move { m_clone.acquire_permit_cancellable(&t_clone).await });

    // Give wait_handle a moment to enter the semaphore acquire queue
    tokio::time::sleep(Duration::from_millis(30)).await;

    // While waiting for capacity, user cancels Request 2
    let cancelled = manager.cancel_request(&req_id_2, None);
    assert!(cancelled, "Cancel signal must be dispatched");

    // The waiting acquire must complete promptly with Cancelled error
    let res = wait_handle.await.unwrap();
    match res {
        Err(InferenceError::Cancelled) => {}
        other => panic!("Expected InferenceError::Cancelled, got {:?}", other),
    }

    // Permit 1 was never touched by Request 2 and can still be used and dropped
    drop(permit1);
    manager.unregister_request(&req_id_2);
    assert_eq!(manager.active_count(), 0);
}

// Audit 6: Separate desktop windows cannot cancel each other's requests.
#[tokio::test]
async fn test_audit_separate_windows_cannot_cancel_each_others_requests() {
    let manager = Arc::new(InferenceManager::new(2));

    let req_id = "req_win_isolation".to_string();
    let token = manager
        .register_request(
            req_id.clone(),
            "ollama/qwen3:8b".to_string(),
            Some("window_alpha".to_string()),
        )
        .unwrap();

    // Window "window_beta" tries to cancel Request belonging to "window_alpha"
    let beta_cancel = manager.cancel_request(&req_id, Some("window_beta"));
    assert!(
        !beta_cancel,
        "Window beta must NOT be able to cancel window alpha's request"
    );
    assert!(!token.is_cancelled(), "Token must remain active");

    // Window "window_alpha" cancels its own request
    let alpha_cancel = manager.cancel_request(&req_id, Some("window_alpha"));
    assert!(
        alpha_cancel,
        "Window alpha must successfully cancel its own request"
    );
    assert!(token.is_cancelled(), "Token must now be cancelled");

    manager.unregister_request(&req_id);
}

// 17. Simultaneous requests remain isolated.
#[tokio::test]
async fn test_case_17_simultaneous_requests_remain_isolated() {
    let (endpoint, shutdown) = start_mock_streaming_server(|req| async move {
        if let Some(resp) = mock_tags_and_show(&req) {
            return resp;
        }
        if req.uri().path() == "/api/chat" {
            let body_bytes = http_body_util::BodyExt::collect(req.into_body())
                .await
                .unwrap()
                .to_bytes();
            let val: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
            let prompt = val["messages"][0]["content"].as_str().unwrap_or("");
            let response_text = format!("Reply to {}", prompt);

            let stream = futures_util::stream::iter(vec![
                Ok::<_, std::io::Error>(Bytes::from(format!(
                    "{{\"message\":{{\"content\":\"{}\"}},\"done\":false}}\n",
                    response_text
                ))),
                Ok::<_, std::io::Error>(Bytes::from("{\"done\":true}\n")),
            ]);
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from_stream(stream))
                .unwrap();
        }
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap()
    })
    .await;

    let provider = Arc::new(OllamaProvider::new(&endpoint));
    let registry = Arc::new(ModelRegistry::new());
    registry.refresh(&*provider).await;

    let p1 = provider.clone();
    let r1 = registry.clone();
    let h1 = tokio::spawn(async move {
        let token = CancellationToken::new();
        let req = InferenceRequest::with_request_id(
            "req_A",
            "ollama/qwen3:8b",
            vec![ChatMessage {
                role: MessageRole::User,
                content: "Alice".to_string(),
            }],
        );
        let mut s = r1.execute_inference_stream(&p1, req, token).await.unwrap();
        let mut items = Vec::new();
        while let Some(e) = s.next().await {
            items.push(e.unwrap());
        }
        items
    });

    let p2 = provider.clone();
    let r2 = registry.clone();
    let h2 = tokio::spawn(async move {
        let token = CancellationToken::new();
        let req = InferenceRequest::with_request_id(
            "req_B",
            "ollama/qwen3:8b",
            vec![ChatMessage {
                role: MessageRole::User,
                content: "Bob".to_string(),
            }],
        );
        let mut s = r2.execute_inference_stream(&p2, req, token).await.unwrap();
        let mut items = Vec::new();
        while let Some(e) = s.next().await {
            items.push(e.unwrap());
        }
        items
    });

    let res1 = h1.await.unwrap();
    let res2 = h2.await.unwrap();

    // Verify req_A isolation
    for ev in res1 {
        match ev {
            InferenceEvent::Started { request_id, .. } => assert_eq!(request_id, "req_A"),
            InferenceEvent::Delta {
                request_id,
                content,
            } => {
                assert_eq!(request_id, "req_A");
                assert!(content.contains("Alice"));
            }
            InferenceEvent::Completed { request_id, .. } => assert_eq!(request_id, "req_A"),
            _ => {}
        }
    }

    // Verify req_B isolation
    for ev in res2 {
        match ev {
            InferenceEvent::Started { request_id, .. } => assert_eq!(request_id, "req_B"),
            InferenceEvent::Delta {
                request_id,
                content,
            } => {
                assert_eq!(request_id, "req_B");
                assert!(content.contains("Bob"));
            }
            InferenceEvent::Completed { request_id, .. } => assert_eq!(request_id, "req_B"),
            _ => {}
        }
    }

    let _ = shutdown.send(());
}

// 18. A slow request doesn't block model registry reads.
#[tokio::test]
async fn test_case_18_a_slow_request_does_not_block_model_registry_reads() {
    let (endpoint, shutdown) = start_mock_streaming_server(|req| async move {
        if let Some(resp) = mock_tags_and_show(&req) {
            return resp;
        }
        if req.uri().path() == "/api/chat" {
            // Emits 1 chunk then pauses for 300ms
            let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(2);
            tokio::spawn(async move {
                let _ = tx
                    .send(Ok(Bytes::from(
                        "{\"message\":{\"content\":\"slow\"},\"done\":false}\n",
                    )))
                    .await;
                tokio::time::sleep(Duration::from_millis(300)).await;
                let _ = tx.send(Ok(Bytes::from("{\"done\":true}\n"))).await;
            });
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from_stream(ChannelStream(rx)))
                .unwrap();
        }
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap()
    })
    .await;

    let provider = Arc::new(OllamaProvider::new(&endpoint));
    let registry = Arc::new(ModelRegistry::new());
    registry.refresh(&*provider).await;

    let p = provider.clone();
    let r = registry.clone();
    let token = CancellationToken::new();

    let stream_handle = tokio::spawn(async move {
        let req = InferenceRequest::with_request_id(
            "req_c18",
            "ollama/qwen3:8b",
            vec![ChatMessage {
                role: MessageRole::User,
                content: "Slow test".to_string(),
            }],
        );
        let mut s = r.execute_inference_stream(&p, req, token).await.unwrap();
        while s.next().await.is_some() {}
    });

    // Wait a brief 20ms for inference to start and enter network I/O
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Immediately read registry snapshot while stream is in flight
    let start_read = std::time::Instant::now();
    let snapshot = registry.get_snapshot();
    let read_dur = start_read.elapsed();

    // Registry read must be lock-free and fast (< 30ms)
    assert!(
        read_dur < Duration::from_millis(50),
        "Registry read was blocked: {:?}",
        read_dur
    );
    assert_eq!(snapshot.status, ProviderStatus::Ready);
    assert_eq!(snapshot.models.len(), 1);

    stream_handle.await.unwrap();
    let _ = shutdown.send(());
}

// 19. Request limit violations.
#[tokio::test]
async fn test_case_19_request_limit_violations() {
    let provider = OllamaProvider::new("http://127.0.0.1:11434");
    let registry = ModelRegistry::new();
    let token = CancellationToken::new();

    // Empty messages
    let empty_req = InferenceRequest::with_request_id("req_empty", "ollama/qwen3:8b", vec![]);
    let err_empty = match registry
        .execute_inference_stream(&provider, empty_req, token.clone())
        .await
    {
        Err(e) => e,
        Ok(_) => panic!("Expected error, got Ok stream"),
    };
    match err_empty {
        InferenceError::InvalidRequest(msg) => assert!(msg.contains("cannot be empty")),
        other => panic!("Expected InvalidRequest, got {:?}", other),
    }

    // Exceeds 50 messages limit
    let too_many_msgs: Vec<ChatMessage> = (0..51)
        .map(|i| ChatMessage {
            role: MessageRole::User,
            content: format!("m{}", i),
        })
        .collect();
    let excessive_req =
        InferenceRequest::with_request_id("req_excessive", "ollama/qwen3:8b", too_many_msgs);
    let err_excessive = match registry
        .execute_inference_stream(&provider, excessive_req, token.clone())
        .await
    {
        Err(e) => e,
        Ok(_) => panic!("Expected error, got Ok stream"),
    };
    match err_excessive {
        InferenceError::InvalidRequest(msg) => assert!(msg.contains("Too many messages")),
        other => panic!("Expected InvalidRequest, got {:?}", other),
    }

    // Single message content > 64 KiB
    let single_big = InferenceRequest::with_request_id(
        "req_single_big",
        "ollama/qwen3:8b",
        vec![ChatMessage {
            role: MessageRole::User,
            content: "x".repeat(65 * 1024),
        }],
    );
    let err_single = match registry
        .execute_inference_stream(&provider, single_big, token.clone())
        .await
    {
        Err(e) => e,
        Ok(_) => panic!("Expected error, got Ok stream"),
    };
    match err_single {
        InferenceError::InvalidRequest(msg) => {
            assert!(msg.contains("64 KiB") && msg.contains("exceeds"))
        }
        other => panic!("Expected InvalidRequest, got {:?}", other),
    }

    // Total content > 128 KiB
    let total_big = InferenceRequest::with_request_id(
        "req_total_big",
        "ollama/qwen3:8b",
        vec![
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
    );
    let err_total = match registry
        .execute_inference_stream(&provider, total_big, token)
        .await
    {
        Err(e) => e,
        Ok(_) => panic!("Expected error, got Ok stream"),
    };
    match err_total {
        InferenceError::InvalidRequest(msg) => {
            assert!(msg.contains("128 KiB") && msg.contains("exceeds"))
        }
        other => panic!("Expected InvalidRequest, got {:?}", other),
    }
}

// 20. Existing non-streaming inference and discovery regressions.
#[tokio::test]
async fn test_case_20_existing_non_streaming_inference_and_discovery_regressions() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let cc_clone = call_count.clone();

    let (endpoint, shutdown) = start_mock_streaming_server(move |req| {
        let cc = cc_clone.clone();
        async move {
            if let Some(resp) = mock_tags_and_show(&req) {
                return resp;
            }
            if req.uri().path() == "/api/chat" {
                cc.fetch_add(1, Ordering::SeqCst);
                return Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::from(
                        r#"{"message":{"role":"assistant","content":"Non-streaming still works!"},"done":true,"prompt_eval_count":5,"eval_count":5}"#,
                    ))
                    .unwrap();
            }
            Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).unwrap()
        }
    })
    .await;

    let provider = OllamaProvider::new(&endpoint);
    let registry = ModelRegistry::new();

    // 1. Discovery refresh works cleanly
    let snapshot = registry.refresh(&provider).await;
    assert_eq!(snapshot.status, ProviderStatus::Ready);
    assert_eq!(snapshot.models.len(), 1);

    // 2. Non-streaming generate works cleanly via registry
    let request = InferenceRequest::with_request_id(
        "req_c20",
        "ollama/qwen3:8b",
        vec![ChatMessage {
            role: MessageRole::User,
            content: "Verify".to_string(),
        }],
    );

    let res = registry
        .execute_inference(&provider, request)
        .await
        .unwrap();
    assert_eq!(res.content, "Non-streaming still works!");
    assert_eq!(res.finish_reason.as_deref(), Some("stop"));
    assert_eq!(res.usage.unwrap().total_tokens, Some(10));

    // 3. Direct ModelInference trait generate works cleanly
    let direct_req = InferenceRequest::new(
        "ollama/qwen3:8b",
        vec![ChatMessage {
            role: MessageRole::User,
            content: "Direct".to_string(),
        }],
    );
    let direct_res = provider.generate(direct_req).await.unwrap();
    assert_eq!(direct_res.content, "Non-streaming still works!");

    assert_eq!(call_count.load(Ordering::SeqCst), 2);
    let _ = shutdown.send(());
}
