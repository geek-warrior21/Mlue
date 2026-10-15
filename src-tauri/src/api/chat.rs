use super::auth::AuthenticatedKey;
use super::error::{ApiError, AppJson};
use crate::inference::types::{ChatMessage, InferenceRequest, MessageRole};
use crate::inference::{InferenceError, InferenceManager};
use crate::keys::ApiKeyService;
use crate::keys::types::ApiKeyError;
use crate::providers::ollama::OllamaProvider;
use crate::registry::ModelRegistry;
use axum::Json;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
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

pub struct ChatCompletionState {
    pub registry: Arc<ModelRegistry>,
    pub provider: Arc<OllamaProvider>,
    pub inference_manager: Arc<InferenceManager>,
    pub key_service: Arc<ApiKeyService>,
}

pub async fn chat_completions_handler(
    Extension(AuthenticatedKey(key)): Extension<AuthenticatedKey>,
    State(state): State<Arc<ChatCompletionState>>,
    AppJson(req): AppJson<OpenAiChatCompletionRequest>,
) -> Result<Json<OpenAiChatCompletion>, ApiError> {
    // 1. Streaming check
    if req.stream == Some(true) {
        return Err(ApiError::bad_request(
            "HTTP streaming is not supported in this version. Set stream: false or omit stream.",
            "unsupported_parameter",
            Some("stream".to_string()),
        ));
    }

    // 2. Unsupported parameters check
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

    // 3. Message role and boundary validations
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

    // 4. Model resolution and authorization
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

    // 5. Concurrency limit acquisition
    let permit = state.inference_manager.try_acquire_permit().map_err(|_| {
        ApiError::service_unavailable(
            "Inference capacity exceeded. Please retry shortly.",
            "capacity_exceeded",
        )
    })?;

    // 6. Execute inference
    let inference_req = InferenceRequest::new(&resolved_model_id, canonical_messages);
    let inference_res = state
        .registry
        .execute_inference(&state.provider, inference_req)
        .await;

    drop(permit);

    let inference_res = match inference_res {
        Ok(res) => res,
        Err(InferenceError::ProviderUnreachable(msg)) => {
            return Err(ApiError::service_unavailable(msg, "provider_offline"));
        }
        Err(InferenceError::Timeout(d)) => {
            return Err(ApiError::gateway_timeout(format!(
                "Inference request timed out after {:?}",
                d
            )));
        }
        Err(InferenceError::UnsupportedCapability(msg)) => {
            return Err(ApiError::bad_request(
                msg,
                "unsupported_capability",
                Some("model".to_string()),
            ));
        }
        Err(InferenceError::ModelNotFound(msg)) => {
            return Err(ApiError::not_found(
                msg,
                "model_not_found",
                Some("model".to_string()),
            ));
        }
        Err(InferenceError::InvalidRequest(msg)) => {
            return Err(ApiError::bad_request(msg, "invalid_request_error", None));
        }
        Err(InferenceError::HttpStatus { code, message }) => {
            let error_code = match code {
                503 => "provider_offline",
                504 => "timeout",
                _ => "provider_error",
            };
            return Err(ApiError {
                status: StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY),
                response: super::error::OpenAiErrorResponse::new(
                    message,
                    "api_error",
                    None,
                    error_code,
                ),
            });
        }
        Err(err) => {
            return Err(ApiError::internal(format!(
                "Provider inference failure: {}",
                err
            )));
        }
    };

    // 7. Format OpenAI-compatible completion envelope
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

    Ok(Json(OpenAiChatCompletion {
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
    }))
}
