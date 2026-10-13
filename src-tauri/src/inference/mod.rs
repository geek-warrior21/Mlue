pub mod manager;
pub mod types;

pub use manager::{InferenceManager, TerminalGuard};
pub use types::{
    ChatMessage, InferenceEvent, InferenceRequest, InferenceResponse, MessageRole, UsageInfo,
};

use futures_util::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum InferenceError {
    #[error("Ollama couldn't be reached. Start Ollama and try again.")]
    ProviderUnreachable(String),

    #[error("Selected model is not installed or available: {0}")]
    ModelNotFound(String),

    #[error("This model doesn't support text chat: {0}")]
    UnsupportedCapability(String),

    #[error("The AI model took too long to respond. Try again.")]
    Timeout(Duration),

    #[error("AI service error ({code}): {message}")]
    HttpStatus { code: u16, message: String },

    #[error("Received an invalid response from the AI provider: {0}")]
    MalformedResponse(String),

    #[error("The AI model returned an empty response. Try again.")]
    EmptyResponse,

    #[error("Invalid inference request: {0}")]
    InvalidRequest(String),

    #[error("The AI model failed to load or run: {0}")]
    ModelLoadFailure(String),

    #[error("Inference request was cancelled")]
    Cancelled,

    #[error("Inference failure: {0}")]
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InferenceErrorPayload {
    pub code: String,
    pub message: String,
    pub technical_details: Option<String>,
}

impl InferenceErrorPayload {
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        technical_details: Option<String>,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            technical_details,
        }
    }
}

impl std::fmt::Display for InferenceErrorPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for InferenceErrorPayload {}

impl From<InferenceError> for InferenceErrorPayload {
    fn from(err: InferenceError) -> Self {
        match err {
            InferenceError::ProviderUnreachable(details) => InferenceErrorPayload::new(
                "PROVIDER_UNREACHABLE",
                "Ollama couldn't be reached. Start Ollama and try again.",
                Some(details),
            ),
            InferenceError::ModelNotFound(details) => InferenceErrorPayload::new(
                "MODEL_NOT_FOUND",
                format!("Selected model is not installed: {}", details),
                Some(details),
            ),
            InferenceError::UnsupportedCapability(details) => InferenceErrorPayload::new(
                "UNSUPPORTED_CAPABILITY",
                "This model doesn't support text chat.",
                Some(details),
            ),
            InferenceError::Timeout(dur) => InferenceErrorPayload::new(
                "TIMEOUT",
                "The AI model took too long to respond. Try again.",
                Some(format!("Inference timed out after {:?}", dur)),
            ),
            InferenceError::HttpStatus { code, message } => InferenceErrorPayload::new(
                "HTTP_ERROR",
                format!("AI service error (HTTP {})", code),
                Some(message),
            ),
            InferenceError::MalformedResponse(details) => InferenceErrorPayload::new(
                "MALFORMED_RESPONSE",
                "Received an invalid response from the AI provider.",
                Some(details),
            ),
            InferenceError::EmptyResponse => InferenceErrorPayload::new(
                "EMPTY_RESPONSE",
                "The AI model returned an empty response. Try again.",
                None,
            ),
            InferenceError::InvalidRequest(details) => InferenceErrorPayload::new(
                "INVALID_REQUEST",
                format!("Invalid request: {}", details),
                Some(details),
            ),
            InferenceError::ModelLoadFailure(details) => InferenceErrorPayload::new(
                "MODEL_LOAD_FAILURE",
                "The AI model failed to load or run. Please verify your system resources.",
                Some(details),
            ),
            InferenceError::Cancelled => {
                InferenceErrorPayload::new("CANCELLED", "Generation was cancelled.", None)
            }
            InferenceError::Other(details) => InferenceErrorPayload::new(
                "INTERNAL_ERROR",
                "An unexpected error occurred during inference.",
                Some(details),
            ),
        }
    }
}

pub type InferenceEventStream =
    Pin<Box<dyn Stream<Item = Result<InferenceEvent, InferenceError>> + Send>>;

pub trait ModelInference: Send + Sync {
    fn generate(
        &self,
        request: InferenceRequest,
    ) -> impl std::future::Future<Output = Result<InferenceResponse, InferenceError>> + Send;

    fn stream(
        &self,
        request: InferenceRequest,
        cancellation_token: CancellationToken,
    ) -> impl std::future::Future<Output = Result<InferenceEventStream, InferenceError>> + Send;
}

impl<T: ModelInference + ?Sized> ModelInference for std::sync::Arc<T> {
    fn generate(
        &self,
        request: InferenceRequest,
    ) -> impl std::future::Future<Output = Result<InferenceResponse, InferenceError>> + Send {
        (**self).generate(request)
    }

    fn stream(
        &self,
        request: InferenceRequest,
        cancellation_token: CancellationToken,
    ) -> impl std::future::Future<Output = Result<InferenceEventStream, InferenceError>> + Send
    {
        (**self).stream(request, cancellation_token)
    }
}
