use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InferenceRequest {
    #[serde(default)]
    pub request_id: Option<String>,
    pub model_id: String,
    pub messages: Vec<ChatMessage>,
}

impl InferenceRequest {
    pub fn new(model_id: impl Into<String>, messages: Vec<ChatMessage>) -> Self {
        Self {
            request_id: None,
            model_id: model_id.into(),
            messages,
        }
    }

    pub fn with_request_id(
        request_id: impl Into<String>,
        model_id: impl Into<String>,
        messages: Vec<ChatMessage>,
    ) -> Self {
        Self {
            request_id: Some(request_id.into()),
            model_id: model_id.into(),
            messages,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct UsageInfo {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InferenceResponse {
    pub model_id: String,
    pub content: String,
    pub finish_reason: Option<String>,
    pub usage: Option<UsageInfo>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InferenceEvent {
    Started {
        request_id: String,
        model_id: String,
    },
    Delta {
        request_id: String,
        content: String,
    },
    Completed {
        request_id: String,
        model_id: String,
        usage: Option<UsageInfo>,
        duration_ms: u64,
        finish_reason: Option<String>,
    },
    Cancelled {
        request_id: String,
    },
    Error {
        request_id: String,
        code: String,
        message: String,
    },
}

impl InferenceEvent {
    pub fn request_id(&self) -> &str {
        match self {
            Self::Started { request_id, .. } => request_id,
            Self::Delta { request_id, .. } => request_id,
            Self::Completed { request_id, .. } => request_id,
            Self::Cancelled { request_id } => request_id,
            Self::Error { request_id, .. } => request_id,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. } | Self::Cancelled { .. } | Self::Error { .. }
        )
    }
}
