pub mod types;

use crate::inference::InferenceError;
use crate::inference::types::{InferenceEvent, InferenceRequest, InferenceResponse};
use crate::providers::ollama::OllamaProvider;
use crate::providers::{DiscoveryError, ModelDiscovery};
use chrono::Utc;
use futures_util::Stream;
use std::pin::Pin;
use std::sync::RwLock;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub use types::{DiscoveredModelsResponse, ModelCapabilities, ModelInfo, ProviderStatus};

pub const MAX_MESSAGES_COUNT: usize = 50;
pub const MAX_SINGLE_MESSAGE_BYTES: usize = 64 * 1024; // 64 KiB
pub const MAX_TOTAL_CONTENT_BYTES: usize = 128 * 1024; // 128 KiB

pub struct ModelRegistry {
    snapshot: RwLock<DiscoveredModelsResponse>,
    refresh_lock: Mutex<()>,
}

impl Default for ModelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelRegistry {
    pub fn new() -> Self {
        Self {
            snapshot: RwLock::new(DiscoveredModelsResponse::initial("ollama")),
            refresh_lock: Mutex::new(()),
        }
    }

    pub fn get_snapshot(&self) -> DiscoveredModelsResponse {
        self.snapshot
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub async fn refresh<D: ModelDiscovery>(&self, provider: &D) -> DiscoveredModelsResponse {
        let _guard = self.refresh_lock.lock().await;

        {
            let mut lock = self.snapshot.write().unwrap_or_else(|p| p.into_inner());
            lock.status = ProviderStatus::Checking;
            lock.error = None;
        }

        // Perform network discovery asynchronously without holding the state lock
        let discovery_result = provider.discover().await;
        let checked_at = Utc::now().to_rfc3339();

        let mut lock = self.snapshot.write().unwrap_or_else(|p| p.into_inner());
        lock.last_checked_at = Some(checked_at.clone());

        match discovery_result {
            Ok(models) => {
                if models.is_empty() {
                    lock.status = ProviderStatus::NoModels;
                    lock.models = Vec::new();
                    lock.last_successful_at = Some(checked_at);
                    lock.error = None;
                } else {
                    lock.status = ProviderStatus::Ready;
                    lock.models = models;
                    lock.last_successful_at = Some(checked_at);
                    lock.error = None;
                }
            }
            Err(DiscoveryError::Unreachable(msg)) => {
                lock.status = ProviderStatus::Unreachable;
                lock.models = Vec::new();
                lock.error = Some(msg);
            }
            Err(DiscoveryError::HttpStatus { code, message }) => {
                lock.status = ProviderStatus::Error;
                lock.models = Vec::new();
                lock.error = Some(format!("HTTP error {}: {}", code, message));
            }
            Err(DiscoveryError::MalformedJson(msg)) => {
                lock.status = ProviderStatus::Error;
                lock.models = Vec::new();
                lock.error = Some(format!("Invalid response format: {}", msg));
            }
            Err(DiscoveryError::Other(msg)) => {
                lock.status = ProviderStatus::Error;
                lock.models = Vec::new();
                lock.error = Some(msg);
            }
        }

        lock.clone()
    }

    pub fn validate_request_boundaries(
        &self,
        request: &InferenceRequest,
    ) -> Result<(), InferenceError> {
        if request.messages.is_empty() {
            return Err(InferenceError::InvalidRequest(
                "Messages list cannot be empty".to_string(),
            ));
        }
        if request.messages.len() > MAX_MESSAGES_COUNT {
            return Err(InferenceError::InvalidRequest(format!(
                "Too many messages: maximum is {} (received {})",
                MAX_MESSAGES_COUNT,
                request.messages.len()
            )));
        }
        let mut total_bytes = 0;
        for msg in &request.messages {
            let msg_bytes = msg.content.len();
            if msg_bytes > MAX_SINGLE_MESSAGE_BYTES {
                return Err(InferenceError::InvalidRequest(format!(
                    "A single message exceeds the maximum allowed size of 64 KiB ({} bytes)",
                    msg_bytes
                )));
            }
            total_bytes += msg_bytes;
            if total_bytes > MAX_TOTAL_CONTENT_BYTES {
                return Err(InferenceError::InvalidRequest(format!(
                    "Total prompt content exceeds the maximum allowed size of 128 KiB ({} bytes)",
                    total_bytes
                )));
            }
        }
        Ok(())
    }

    pub async fn execute_inference(
        &self,
        provider: &OllamaProvider,
        request: InferenceRequest,
    ) -> Result<InferenceResponse, InferenceError> {
        // Step 0: Request security & resource validation
        self.validate_request_boundaries(&request)?;

        // Step 1 & 2: Validate model exists in registry and provider status
        // NOTE: Registry lock is acquired briefly and released immediately before network I/O
        let (upstream_id, provider_status) = {
            let snapshot = self.snapshot.read().unwrap_or_else(|p| p.into_inner());
            let found_model = snapshot
                .models
                .iter()
                .find(|m| m.id == request.model_id)
                .cloned();
            (found_model.map(|m| m.upstream_id), snapshot.status)
        };

        let upstream_id = match upstream_id {
            Some(id) => id,
            None => {
                return Err(InferenceError::ModelNotFound(format!(
                    "Model '{}' is not registered or installed",
                    request.model_id
                )));
            }
        };

        if provider_status == ProviderStatus::Unreachable {
            return Err(InferenceError::ProviderUnreachable(
                "Ollama is currently unreachable".to_string(),
            ));
        }

        // Step 3 & 4: Validate text-completion capability via /api/show (cached)
        let capabilities = match provider.get_or_inspect_capabilities(&upstream_id).await {
            Ok(caps) => caps,
            Err(DiscoveryError::Unreachable(msg)) => {
                return Err(InferenceError::ProviderUnreachable(msg));
            }
            Err(DiscoveryError::HttpStatus { code, message }) => {
                if code == 404 {
                    return Err(InferenceError::ModelNotFound(format!(
                        "Model '{}' was not found by Ollama",
                        upstream_id
                    )));
                }
                return Err(InferenceError::HttpStatus { code, message });
            }
            Err(err) => return Err(InferenceError::Other(err.to_string())),
        };

        match capabilities.chat {
            Some(true) => {}
            Some(false) => {
                return Err(InferenceError::UnsupportedCapability(format!(
                    "Model '{}' does not support text chat/completion (it may be an embedding-only model)",
                    upstream_id
                )));
            }
            None => {
                return Err(InferenceError::UnsupportedCapability(format!(
                    "Model '{}' capabilities could not be verified to support text completion",
                    upstream_id
                )));
            }
        }

        // Step 5 & 6: Execute provider request without holding any registry lock
        provider.generate_with_upstream(&upstream_id, request).await
    }

    pub async fn execute_inference_stream(
        &self,
        provider: &OllamaProvider,
        request: InferenceRequest,
        cancellation_token: CancellationToken,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<InferenceEvent, InferenceError>> + Send>>,
        InferenceError,
    > {
        // Step 0: Request security & resource validation
        self.validate_request_boundaries(&request)?;

        // Step 1 & 2: Validate model exists in registry and provider status
        // NOTE: Registry lock is acquired briefly and released immediately before network I/O
        let (upstream_id, provider_status) = {
            let snapshot = self.snapshot.read().unwrap_or_else(|p| p.into_inner());
            let found_model = snapshot
                .models
                .iter()
                .find(|m| m.id == request.model_id)
                .cloned();
            (found_model.map(|m| m.upstream_id), snapshot.status)
        };

        let upstream_id = match upstream_id {
            Some(id) => id,
            None => {
                return Err(InferenceError::ModelNotFound(format!(
                    "Model '{}' is not registered or installed",
                    request.model_id
                )));
            }
        };

        if provider_status == ProviderStatus::Unreachable {
            return Err(InferenceError::ProviderUnreachable(
                "Ollama is currently unreachable".to_string(),
            ));
        }

        // Step 3 & 4: Validate text-completion capability via /api/show (cached)
        let capabilities = match provider.get_or_inspect_capabilities(&upstream_id).await {
            Ok(caps) => caps,
            Err(DiscoveryError::Unreachable(msg)) => {
                return Err(InferenceError::ProviderUnreachable(msg));
            }
            Err(DiscoveryError::HttpStatus { code, message }) => {
                if code == 404 {
                    return Err(InferenceError::ModelNotFound(format!(
                        "Model '{}' was not found by Ollama",
                        upstream_id
                    )));
                }
                return Err(InferenceError::HttpStatus { code, message });
            }
            Err(err) => return Err(InferenceError::Other(err.to_string())),
        };

        match capabilities.chat {
            Some(true) => {}
            Some(false) => {
                return Err(InferenceError::UnsupportedCapability(format!(
                    "Model '{}' does not support text chat/completion (it may be an embedding-only model)",
                    upstream_id
                )));
            }
            None => {
                return Err(InferenceError::UnsupportedCapability(format!(
                    "Model '{}' capabilities could not be verified to support text completion",
                    upstream_id
                )));
            }
        }

        // Step 5 & 6: Execute provider stream without holding any registry lock
        provider
            .stream_with_upstream(&upstream_id, request, cancellation_token)
            .await
    }
}
