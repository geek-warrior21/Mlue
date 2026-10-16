use super::auth::AuthenticatedKey;
use super::error::{ApiError, AppJson};
use crate::inference::types::{ChatMessage, InferenceEvent, InferenceRequest, MessageRole};
use crate::inference::{InferenceError, InferenceManager};
use crate::keys::ApiKeyService;
use crate::keys::types::ApiKeyError;
use crate::providers::ollama::OllamaProvider;
use crate::registry::ModelRegistry;
use axum::Json;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::sync::OwnedSemaphorePermit;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub const MAX_MESSAGES_COUNT: usize = 50;
pub const MAX_SINGLE_MESSAGE_BYTES: usize = 64 * 1024; // 64 KiB
pub const MAX_TOTAL_CONTENT_BYTES: usize = 128 * 1024; // 128 KiB

#[derive(Debug, Clone, Deserialize)]
pub struct OpenAiChatMessage {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamOptions {
    #[serde(default)]
    pub include_usage: Option<bool>,
    #[serde(flatten)]
    pub unsupported_fields: std::collections::BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpenAiChatCompletionRequest {
    pub model: String,
    pub messages: Vec<OpenAiChatMessage>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub stream_options: Option<StreamOptions>,
    #[serde(flatten)]
    pub unsupported_fields: std::collections::BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiResponseMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiChoice {
    pub index: u32,
    pub message: OpenAiResponseMessage,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiChatCompletion {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<OpenAiChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<OpenAiUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OpenAiChunkDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiChunkChoice {
    pub index: u32,
    pub delta: OpenAiChunkDelta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiChatCompletionChunk {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<OpenAiChunkChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<OpenAiUsage>,
}

pub struct StreamGuard {
    pub request_id: String,
    pub inference_manager: Arc<InferenceManager>,
    pub cancellation_token: CancellationToken,
    pub permit: Option<OwnedSemaphorePermit>,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.cancellation_token.cancel();
        self.inference_manager.unregister_request(&self.request_id);
        drop(self.permit.take());
    }
}

pub struct SseStream {
    pub event_stream: Pin<Box<dyn Stream<Item = Result<InferenceEvent, InferenceError>> + Send>>,
    pub completion_id: String,
    pub created: i64,
    pub model_id: String,
    pub include_usage: bool,
    pub guard: StreamGuard,
    pub pending_frames: VecDeque<Bytes>,
    pub emitted_initial: bool,
    pub emitted_finish: bool,
    pub emitted_done: bool,
}

impl Stream for SseStream {
    type Item = Result<Bytes, std::convert::Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            // 1. Drain pending frames first
            if let Some(frame) = self.pending_frames.pop_front() {
                return Poll::Ready(Some(Ok(frame)));
            }

            // 2. If [DONE] was emitted, stream is finished
            if self.emitted_done {
                return Poll::Ready(None);
            }

            // 3. Poll upstream inference event stream
            match self.event_stream.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(event))) => match event {
                    InferenceEvent::Started { .. } => {
                        if !self.emitted_initial {
                            self.emitted_initial = true;
                            let chunk = OpenAiChatCompletionChunk {
                                id: self.completion_id.clone(),
                                object: "chat.completion.chunk".to_string(),
                                created: self.created,
                                model: self.model_id.clone(),
                                choices: vec![OpenAiChunkChoice {
                                    index: 0,
                                    delta: OpenAiChunkDelta {
                                        role: Some("assistant".to_string()),
                                        content: Some(String::new()),
                                    },
                                    finish_reason: None,
                                }],
                                usage: None,
                            };
                            if let Ok(json_str) = serde_json::to_string(&chunk) {
                                self.pending_frames
                                    .push_back(Bytes::from(format!("data: {}\n\n", json_str)));
                            }
                        }
                    }
                    InferenceEvent::Delta { content, .. } => {
                        if !self.emitted_initial {
                            self.emitted_initial = true;
                            let chunk = OpenAiChatCompletionChunk {
                                id: self.completion_id.clone(),
                                object: "chat.completion.chunk".to_string(),
                                created: self.created,
                                model: self.model_id.clone(),
                                choices: vec![OpenAiChunkChoice {
                                    index: 0,
                                    delta: OpenAiChunkDelta {
                                        role: Some("assistant".to_string()),
                                        content: Some(String::new()),
                                    },
                                    finish_reason: None,
                                }],
                                usage: None,
                            };
                            if let Ok(json_str) = serde_json::to_string(&chunk) {
                                self.pending_frames
                                    .push_back(Bytes::from(format!("data: {}\n\n", json_str)));
                            }
                        }

                        if !content.is_empty() {
                            let chunk = OpenAiChatCompletionChunk {
                                id: self.completion_id.clone(),
                                object: "chat.completion.chunk".to_string(),
                                created: self.created,
                                model: self.model_id.clone(),
                                choices: vec![OpenAiChunkChoice {
                                    index: 0,
                                    delta: OpenAiChunkDelta {
                                        role: None,
                                        content: Some(content),
                                    },
                                    finish_reason: None,
                                }],
                                usage: None,
                            };
                            if let Ok(json_str) = serde_json::to_string(&chunk) {
                                self.pending_frames
                                    .push_back(Bytes::from(format!("data: {}\n\n", json_str)));
                            }
                        }
                    }
                    InferenceEvent::Completed {
                        usage,
                        finish_reason,
                        ..
                    } => {
                        if !self.emitted_initial {
                            self.emitted_initial = true;
                            let chunk = OpenAiChatCompletionChunk {
                                id: self.completion_id.clone(),
                                object: "chat.completion.chunk".to_string(),
                                created: self.created,
                                model: self.model_id.clone(),
                                choices: vec![OpenAiChunkChoice {
                                    index: 0,
                                    delta: OpenAiChunkDelta {
                                        role: Some("assistant".to_string()),
                                        content: Some(String::new()),
                                    },
                                    finish_reason: None,
                                }],
                                usage: None,
                            };
                            if let Ok(json_str) = serde_json::to_string(&chunk) {
                                self.pending_frames
                                    .push_back(Bytes::from(format!("data: {}\n\n", json_str)));
                            }
                        }

                        if !self.emitted_finish {
                            self.emitted_finish = true;
                            let reason = finish_reason.unwrap_or_else(|| "stop".to_string());
                            let finish_chunk = OpenAiChatCompletionChunk {
                                id: self.completion_id.clone(),
                                object: "chat.completion.chunk".to_string(),
                                created: self.created,
                                model: self.model_id.clone(),
                                choices: vec![OpenAiChunkChoice {
                                    index: 0,
                                    delta: OpenAiChunkDelta::default(),
                                    finish_reason: Some(reason),
                                }],
                                usage: None,
                            };
                            if let Ok(json_str) = serde_json::to_string(&finish_chunk) {
                                self.pending_frames
                                    .push_back(Bytes::from(format!("data: {}\n\n", json_str)));
                            }
                        }

                        // Emit usage chunk if requested and available
                        let maybe_usage = if self.include_usage {
                            usage.filter(|u| {
                                u.prompt_tokens.is_some() || u.completion_tokens.is_some()
                            })
                        } else {
                            None
                        };
                        if let Some(u) = maybe_usage {
                            let usage_chunk = OpenAiChatCompletionChunk {
                                id: self.completion_id.clone(),
                                object: "chat.completion.chunk".to_string(),
                                created: self.created,
                                model: self.model_id.clone(),
                                choices: vec![],
                                usage: Some(OpenAiUsage {
                                    prompt_tokens: u.prompt_tokens.unwrap_or(0),
                                    completion_tokens: u.completion_tokens.unwrap_or(0),
                                    total_tokens: u.total_tokens.unwrap_or(0),
                                }),
                            };
                            if let Ok(json_str) = serde_json::to_string(&usage_chunk) {
                                self.pending_frames
                                    .push_back(Bytes::from(format!("data: {}\n\n", json_str)));
                            }
                        }

                        if !self.emitted_done {
                            self.emitted_done = true;
                            self.pending_frames
                                .push_back(Bytes::from("data: [DONE]\n\n"));
                        }
                    }
                    InferenceEvent::Cancelled { .. } | InferenceEvent::Error { .. } => {
                        self.emitted_done = true;
                    }
                },
                Poll::Ready(Some(Err(_err))) => {
                    self.emitted_done = true;
                }
                Poll::Ready(None) => {
                    if !self.emitted_finish {
                        self.emitted_finish = true;
                        let finish_chunk = OpenAiChatCompletionChunk {
                            id: self.completion_id.clone(),
                            object: "chat.completion.chunk".to_string(),
                            created: self.created,
                            model: self.model_id.clone(),
                            choices: vec![OpenAiChunkChoice {
                                index: 0,
                                delta: OpenAiChunkDelta::default(),
                                finish_reason: Some("stop".to_string()),
                            }],
                            usage: None,
                        };
                        if let Ok(json_str) = serde_json::to_string(&finish_chunk) {
                            self.pending_frames
                                .push_back(Bytes::from(format!("data: {}\n\n", json_str)));
                        }
                    }
                    if !self.emitted_done {
                        self.emitted_done = true;
                        self.pending_frames
                            .push_back(Bytes::from("data: [DONE]\n\n"));
                    }
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

pub struct ChatCompletionState {
    pub registry: Arc<ModelRegistry>,
    pub provider: Arc<OllamaProvider>,
    pub inference_manager: Arc<InferenceManager>,
    pub key_service: Arc<ApiKeyService>,
}

pub fn map_inference_error(err: InferenceError) -> ApiError {
    match err {
        InferenceError::ProviderUnreachable(msg) => {
            ApiError::service_unavailable(msg, "provider_offline")
        }
        InferenceError::Timeout(d) => {
            ApiError::gateway_timeout(format!("Inference request timed out after {:?}", d))
        }
        InferenceError::UnsupportedCapability(msg) => {
            ApiError::bad_request(msg, "unsupported_capability", Some("model".to_string()))
        }
        InferenceError::ModelNotFound(msg) => {
            ApiError::not_found(msg, "model_not_found", Some("model".to_string()))
        }
        InferenceError::InvalidRequest(msg) => {
            ApiError::bad_request(msg, "invalid_request_error", None)
        }
        InferenceError::HttpStatus { code, message } => {
            let error_code = match code {
                503 => "provider_offline",
                504 => "timeout",
                _ => "provider_error",
            };
            ApiError {
                status: StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY),
                response: super::error::OpenAiErrorResponse::new(
                    message,
                    "api_error",
                    None,
                    error_code,
                ),
            }
        }
        InferenceError::Cancelled => ApiError::bad_request("Request cancelled", "cancelled", None),
        err => ApiError::internal(format!("Provider inference failure: {}", err)),
    }
}

pub async fn chat_completions_handler(
    Extension(AuthenticatedKey(key)): Extension<AuthenticatedKey>,
    State(state): State<Arc<ChatCompletionState>>,
    AppJson(req): AppJson<OpenAiChatCompletionRequest>,
) -> Result<Response, ApiError> {
    // 1. Unsupported parameters check
    if let Some((field, _)) = req.unsupported_fields.iter().next() {
        return Err(ApiError::bad_request(
            format!(
                "Unsupported parameter '{}'. Only basic text chat completion ('model', 'messages', 'stream', 'stream_options') is currently supported.",
                field
            ),
            "unsupported_parameter",
            Some(field.clone()),
        ));
    }
    if let Some(field) = req
        .stream_options
        .as_ref()
        .and_then(|opts| opts.unsupported_fields.keys().next())
    {
        return Err(ApiError::bad_request(
            format!(
                "Unsupported stream_options parameter '{}'. Only 'include_usage' is currently supported.",
                field
            ),
            "unsupported_parameter",
            Some(format!("stream_options.{}", field)),
        ));
    }

    // 2. Message role and boundary validations
    if req.messages.is_empty() {
        return Err(ApiError::bad_request(
            "Messages list cannot be empty",
            "invalid_messages",
            Some("messages".to_string()),
        ));
    }

    if req.messages.len() > MAX_MESSAGES_COUNT {
        return Err(ApiError::bad_request(
            format!(
                "Too many messages: maximum is {} (received {})",
                MAX_MESSAGES_COUNT,
                req.messages.len()
            ),
            "invalid_messages",
            Some("messages".to_string()),
        ));
    }

    let mut total_bytes = 0;
    let mut canonical_messages = Vec::with_capacity(req.messages.len());

    for msg in req.messages {
        let role = match msg.role.to_lowercase().as_str() {
            "system" => MessageRole::System,
            "user" => MessageRole::User,
            "assistant" => MessageRole::Assistant,
            other => {
                return Err(ApiError::bad_request(
                    format!(
                        "Unsupported role '{}'. Supported roles are 'system', 'user', 'assistant'",
                        other
                    ),
                    "unsupported_role",
                    Some("role".to_string()),
                ));
            }
        };

        let msg_len = msg.content.len();
        if msg_len > MAX_SINGLE_MESSAGE_BYTES {
            return Err(ApiError::payload_too_large(format!(
                "A single message exceeds the maximum allowed size of 64 KiB ({} bytes)",
                msg_len
            )));
        }

        total_bytes += msg_len;
        if total_bytes > MAX_TOTAL_CONTENT_BYTES {
            return Err(ApiError::payload_too_large(format!(
                "Total prompt content exceeds the maximum allowed size of 128 KiB ({} bytes)",
                total_bytes
            )));
        }

        canonical_messages.push(ChatMessage {
            role,
            content: msg.content,
        });
    }

    // 3. Model resolution and authorization
    let resolved_model_id = if req.model == "auto" {
        match state.key_service.resolve_auto_model(&key, &state.registry) {
            Ok(model_info) => model_info.id,
            Err(ApiKeyError::NoEligibleModels) => {
                return Err(ApiError::bad_request(
                    "No compatible models are available under the current access policy",
                    "no_models_available",
                    Some("model".to_string()),
                ));
            }
            Err(err) => {
                return Err(ApiError::internal(format!(
                    "Failed to resolve auto model: {}",
                    err
                )));
            }
        }
    } else {
        match state
            .key_service
            .authorize_model(&key, &req.model, &state.registry)
        {
            Ok(model_info) => model_info.id,
            Err(ApiKeyError::ModelNotFound(m)) => {
                return Err(ApiError::not_found(
                    format!("The model '{}' does not exist or is not registered", m),
                    "model_not_found",
                    Some("model".to_string()),
                ));
            }
            Err(ApiKeyError::UnauthorizedModel(m)) => {
                return Err(ApiError::forbidden(
                    format!(
                        "Model '{}' is not permitted under the current API key access policy",
                        m
                    ),
                    "unauthorized_model",
                    Some("model".to_string()),
                ));
            }
            Err(err) => {
                return Err(ApiError::internal(format!(
                    "Failed to authorize model: {}",
                    err
                )));
            }
        }
    };

    // 4. Concurrency limit acquisition
    let permit = state.inference_manager.try_acquire_permit().map_err(|_| {
        ApiError::service_unavailable(
            "Inference capacity exceeded. Please retry shortly.",
            "capacity_exceeded",
        )
    })?;

    let is_streaming = req.stream.unwrap_or(false);

    if is_streaming {
        let completion_id = format!("chatcmpl_mlue_{}", Uuid::new_v4().simple());
        let created = chrono::Utc::now().timestamp();
        let cancel_token = match state.inference_manager.register_request(
            completion_id.clone(),
            resolved_model_id.clone(),
            None,
        ) {
            Ok(token) => token,
            Err(e) => {
                drop(permit);
                return Err(map_inference_error(e));
            }
        };

        let inference_req = InferenceRequest::with_request_id(
            &completion_id,
            &resolved_model_id,
            canonical_messages,
        );
        let stream_res = state
            .registry
            .execute_inference_stream(&state.provider, inference_req, cancel_token.clone())
            .await;

        let event_stream = match stream_res {
            Ok(stream) => stream,
            Err(err) => {
                state.inference_manager.unregister_request(&completion_id);
                drop(permit);
                return Err(map_inference_error(err));
            }
        };

        let include_usage = req
            .stream_options
            .as_ref()
            .and_then(|opts| opts.include_usage)
            .unwrap_or(false);

        let guard = StreamGuard {
            request_id: completion_id.clone(),
            inference_manager: state.inference_manager.clone(),
            cancellation_token: cancel_token,
            permit: Some(permit),
        };

        let sse_stream = SseStream {
            event_stream,
            completion_id,
            created,
            model_id: resolved_model_id,
            include_usage,
            guard,
            pending_frames: VecDeque::new(),
            emitted_initial: false,
            emitted_finish: false,
            emitted_done: false,
        };

        let body = axum::body::Body::from_stream(sse_stream);
        let response = Response::builder()
            .status(StatusCode::OK)
            .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
            .header(axum::http::header::CACHE_CONTROL, "no-cache")
            .header(axum::http::header::CONNECTION, "keep-alive")
            .header("X-Accel-Buffering", "no")
            .body(body)
            .map_err(|e| ApiError::internal(e.to_string()))?;

        Ok(response)
    } else {
        // Execute non-streaming inference
        let inference_req = InferenceRequest::new(&resolved_model_id, canonical_messages);
        let inference_res = state
            .registry
            .execute_inference(&state.provider, inference_req)
            .await;

        drop(permit);

        let inference_res = inference_res.map_err(map_inference_error)?;

        let completion_id = format!("chatcmpl_mlue_{}", Uuid::new_v4().simple());
        let created = chrono::Utc::now().timestamp();
        let finish_reason = inference_res
            .finish_reason
            .unwrap_or_else(|| "stop".to_string());
        let usage = inference_res.usage.map(|u| OpenAiUsage {
            prompt_tokens: u.prompt_tokens.unwrap_or(0),
            completion_tokens: u.completion_tokens.unwrap_or(0),
            total_tokens: u.total_tokens.unwrap_or(0),
        });

        let completion = OpenAiChatCompletion {
            id: completion_id,
            object: "chat.completion".to_string(),
            created,
            model: resolved_model_id,
            choices: vec![OpenAiChoice {
                index: 0,
                message: OpenAiResponseMessage {
                    role: "assistant".to_string(),
                    content: inference_res.content,
                },
                finish_reason: Some(finish_reason),
            }],
            usage,
        };

        Ok(Json(completion).into_response())
    }
}
