use crate::inference::types::{
    InferenceEvent, InferenceRequest, InferenceResponse, MessageRole, UsageInfo,
};
use crate::inference::{InferenceError, ModelInference};
use crate::providers::{DiscoveryError, ModelDiscovery};
use crate::registry::types::{ModelCapabilities, ModelInfo};
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

pub const DEFAULT_OLLAMA_ENDPOINT: &str = "http://127.0.0.1:11434";
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);
pub const DEFAULT_DISCOVERY_TIMEOUT: Duration = Duration::from_millis(3000);
pub const DEFAULT_INFERENCE_TIMEOUT: Duration = Duration::from_secs(60);

pub const DEFAULT_STREAM_INITIAL_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(15);
pub const DEFAULT_STREAM_OVERALL_TIMEOUT: Duration = Duration::from_secs(300);

pub const MAX_LINE_BUFFER_BYTES: usize = 64 * 1024; // 64 KiB

pub struct ReceiverStream<T>(pub tokio::sync::mpsc::Receiver<T>);

impl<T> Stream for ReceiverStream<T> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}

#[derive(Debug, Clone)]
pub struct OllamaProvider {
    endpoint: String,
    discovery_client: reqwest::Client,
    inference_client: reqwest::Client,
    inference_timeout: Duration,
    stream_initial_timeout: Duration,
    stream_idle_timeout: Duration,
    stream_overall_timeout: Duration,
    capabilities_cache: Arc<RwLock<HashMap<String, ModelCapabilities>>>,
}

impl OllamaProvider {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self::with_all_timeouts(
            endpoint,
            DEFAULT_CONNECT_TIMEOUT,
            DEFAULT_DISCOVERY_TIMEOUT,
            DEFAULT_INFERENCE_TIMEOUT,
        )
    }

    pub fn with_timeouts(
        endpoint: impl Into<String>,
        connect_timeout: Duration,
        discovery_timeout: Duration,
    ) -> Self {
        Self::with_all_timeouts(
            endpoint,
            connect_timeout,
            discovery_timeout,
            DEFAULT_INFERENCE_TIMEOUT,
        )
    }

    pub fn with_all_timeouts(
        endpoint: impl Into<String>,
        connect_timeout: Duration,
        discovery_timeout: Duration,
        inference_timeout: Duration,
    ) -> Self {
        Self::with_streaming_timeouts(
            endpoint,
            connect_timeout,
            discovery_timeout,
            inference_timeout,
            DEFAULT_STREAM_INITIAL_TIMEOUT,
            DEFAULT_STREAM_IDLE_TIMEOUT,
            DEFAULT_STREAM_OVERALL_TIMEOUT,
        )
    }

    pub fn with_streaming_timeouts(
        endpoint: impl Into<String>,
        connect_timeout: Duration,
        discovery_timeout: Duration,
        inference_timeout: Duration,
        stream_initial_timeout: Duration,
        stream_idle_timeout: Duration,
        stream_overall_timeout: Duration,
    ) -> Self {
        let discovery_client = reqwest::Client::builder()
            .connect_timeout(connect_timeout)
            .timeout(discovery_timeout)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let inference_client = reqwest::Client::builder()
            .connect_timeout(connect_timeout)
            .timeout(inference_timeout)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            endpoint: endpoint.into(),
            discovery_client,
            inference_client,
            inference_timeout,
            stream_initial_timeout,
            stream_idle_timeout,
            stream_overall_timeout,
            capabilities_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn clear_capabilities_cache(&self) {
        let mut lock = self
            .capabilities_cache
            .write()
            .unwrap_or_else(|p| p.into_inner());
        lock.clear();
    }

    pub async fn get_or_inspect_capabilities(
        &self,
        upstream_model: &str,
    ) -> Result<ModelCapabilities, DiscoveryError> {
        // Check cache first
        {
            let cache = self
                .capabilities_cache
                .read()
                .unwrap_or_else(|p| p.into_inner());
            if let Some(caps) = cache.get(upstream_model) {
                return Ok(caps.clone());
            }
        }

        // Issue POST /api/show
        let url = format!("{}/api/show", self.endpoint.trim_end_matches('/'));
        let show_payload = serde_json::json!({ "model": upstream_model });

        let response = match self
            .discovery_client
            .post(&url)
            .json(&show_payload)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(err) => {
                if err.is_connect() || err.is_timeout() {
                    return Err(DiscoveryError::Unreachable(err.to_string()));
                }
                return Err(DiscoveryError::Other(err.to_string()));
            }
        };

        let status = response.status();
        if !status.is_success() {
            let message = response
                .text()
                .await
                .unwrap_or_else(|_| format!("HTTP {}", status.as_u16()));
            return Err(DiscoveryError::HttpStatus {
                code: status.as_u16(),
                message,
            });
        }

        let body_bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(err) => {
                if err.is_timeout() {
                    return Err(DiscoveryError::Unreachable(err.to_string()));
                }
                return Err(DiscoveryError::Other(err.to_string()));
            }
        };

        let show_resp: OllamaShowResponse = match serde_json::from_slice(&body_bytes) {
            Ok(parsed) => parsed,
            Err(err) => {
                return Err(DiscoveryError::MalformedJson(err.to_string()));
            }
        };

        // Inspect capabilities
        let supports_chat = if let Some(caps) = &show_resp.capabilities {
            if caps.iter().any(|c| c == "chat" || c == "completion") {
                Some(true)
            } else if caps.iter().any(|c| c == "embedding") {
                Some(false)
            } else if caps.is_empty() {
                None
            } else {
                Some(false)
            }
        } else if let Some(details) = &show_resp.details {
            let fam = details.family.as_deref().unwrap_or_default().to_lowercase();
            if fam == "bert" || fam == "nomic-bert" {
                Some(false)
            } else if !fam.is_empty() {
                Some(true)
            } else {
                None
            }
        } else if let Some(template) = &show_resp.template {
            if !template.trim().is_empty() {
                Some(true)
            } else {
                None
            }
        } else if let Some(info) = &show_resp.model_info {
            let arch = info
                .get("general.architecture")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_lowercase();
            if arch == "bert" || arch == "nomic-bert" {
                Some(false)
            } else if !arch.is_empty() {
                Some(true)
            } else {
                None
            }
        } else {
            let name_lower = upstream_model.to_lowercase();
            if name_lower.contains("embed") {
                Some(false)
            } else {
                None
            }
        };

        let capabilities = ModelCapabilities {
            chat: supports_chat,
            streaming: None,
            tools: None,
            vision: None,
        };

        // Cache result
        {
            let mut cache = self
                .capabilities_cache
                .write()
                .unwrap_or_else(|p| p.into_inner());
            cache.insert(upstream_model.to_string(), capabilities.clone());
        }

        Ok(capabilities)
    }

    pub async fn generate_with_upstream(
        &self,
        upstream_model: &str,
        request: InferenceRequest,
    ) -> Result<InferenceResponse, InferenceError> {
        let url = format!("{}/api/chat", self.endpoint.trim_end_matches('/'));

        let ollama_messages: Vec<OllamaChatMessage> = request
            .messages
            .into_iter()
            .map(|m| {
                let role = match m.role {
                    MessageRole::System => "system",
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                };
                OllamaChatMessage {
                    role: role.to_string(),
                    content: m.content,
                }
            })
            .collect();

        let payload = OllamaChatPayload {
            model: upstream_model,
            messages: &ollama_messages,
            stream: false,
        };

        let start = Instant::now();

        let response = match self.inference_client.post(&url).json(&payload).send().await {
            Ok(resp) => resp,
            Err(err) => {
                if err.is_timeout() {
                    return Err(InferenceError::Timeout(self.inference_timeout));
                }
                if err.is_connect() {
                    return Err(InferenceError::ProviderUnreachable(err.to_string()));
                }
                return Err(InferenceError::Other(err.to_string()));
            }
        };

        let status = response.status();
        if !status.is_success() {
            let message = response
                .text()
                .await
                .unwrap_or_else(|_| format!("HTTP {}", status.as_u16()));

            if status.as_u16() == 404 {
                return Err(InferenceError::ModelNotFound(message));
            }
            if message.to_lowercase().contains("load") || message.to_lowercase().contains("memory")
            {
                return Err(InferenceError::ModelLoadFailure(message));
            }
            return Err(InferenceError::HttpStatus {
                code: status.as_u16(),
                message,
            });
        }

        let body_bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(err) => {
                if err.is_timeout() {
                    return Err(InferenceError::Timeout(self.inference_timeout));
                }
                return Err(InferenceError::Other(err.to_string()));
            }
        };

        let chat_resp: OllamaChatResponse = match serde_json::from_slice(&body_bytes) {
            Ok(parsed) => parsed,
            Err(err) => {
                return Err(InferenceError::MalformedResponse(err.to_string()));
            }
        };

        if let Some(err_msg) = chat_resp.error {
            let lower = err_msg.to_lowercase();
            if lower.contains("not found") {
                return Err(InferenceError::ModelNotFound(err_msg));
            }
            if lower.contains("load") || lower.contains("memory") {
                return Err(InferenceError::ModelLoadFailure(err_msg));
            }
            return Err(InferenceError::Other(err_msg));
        }

        let assistant_msg = match chat_resp.message {
            Some(msg) => msg,
            None => return Err(InferenceError::EmptyResponse),
        };

        if assistant_msg.content.trim().is_empty() {
            return Err(InferenceError::EmptyResponse);
        }

        let usage = if chat_resp.prompt_eval_count.is_some() || chat_resp.eval_count.is_some() {
            let p = chat_resp.prompt_eval_count;
            let c = chat_resp.eval_count;
            let total = match (p, c) {
                (Some(pv), Some(cv)) => Some(pv + cv),
                (Some(pv), None) => Some(pv),
                (None, Some(cv)) => Some(cv),
                (None, None) => None,
            };
            Some(UsageInfo {
                prompt_tokens: p,
                completion_tokens: c,
                total_tokens: total,
            })
        } else {
            None
        };

        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(InferenceResponse {
            model_id: request.model_id,
            content: assistant_msg.content,
            finish_reason: chat_resp.done_reason.or_else(|| {
                if chat_resp.done == Some(true) {
                    Some("stop".to_string())
                } else {
                    None
                }
            }),
            usage,
            duration_ms: Some(duration_ms),
        })
    }

    pub async fn stream_with_upstream(
        &self,
        upstream_model: &str,
        request: InferenceRequest,
        cancellation_token: CancellationToken,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<InferenceEvent, InferenceError>> + Send>>,
        InferenceError,
    > {
        let request_id = request
            .request_id
            .clone()
            .unwrap_or_else(|| format!("req_{}", chrono::Utc::now().timestamp_micros()));
        let model_id = request.model_id.clone();
        let url = format!("{}/api/chat", self.endpoint.trim_end_matches('/'));

        let ollama_messages: Vec<OllamaChatMessage> = request
            .messages
            .into_iter()
            .map(|m| {
                let role = match m.role {
                    MessageRole::System => "system",
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                };
                OllamaChatMessage {
                    role: role.to_string(),
                    content: m.content,
                }
            })
            .collect();

        let payload = OllamaChatPayload {
            model: upstream_model,
            messages: &ollama_messages,
            stream: true,
        };

        let send_future = self.inference_client.post(&url).json(&payload).send();

        // Check cancellation while establishing connection and awaiting response headers
        let response = tokio::select! {
            _ = cancellation_token.cancelled() => {
                return Err(InferenceError::Cancelled);
            }
            res = tokio::time::timeout(self.stream_initial_timeout, send_future) => {
                match res {
                    Ok(Ok(resp)) => resp,
                    Ok(Err(err)) => {
                        if err.is_connect() {
                            return Err(InferenceError::ProviderUnreachable(err.to_string()));
                        }
                        return Err(InferenceError::Other(err.to_string()));
                    }
                    Err(_) => {
                        return Err(InferenceError::Timeout(self.stream_initial_timeout));
                    }
                }
            }
        };

        let status = response.status();
        if !status.is_success() {
            let message = response
                .text()
                .await
                .unwrap_or_else(|_| format!("HTTP {}", status.as_u16()));

            if status.as_u16() == 404 {
                return Err(InferenceError::ModelNotFound(message));
            }
            if message.to_lowercase().contains("load") || message.to_lowercase().contains("memory")
            {
                return Err(InferenceError::ModelLoadFailure(message));
            }
            return Err(InferenceError::HttpStatus {
                code: status.as_u16(),
                message,
            });
        }

        // Bounded channel providing backpressure (max 32 chunks buffered)
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<InferenceEvent, InferenceError>>(32);

        let idle_timeout = self.stream_idle_timeout;
        let overall_timeout = self.stream_overall_timeout;
        let token = cancellation_token.clone();
        let mut byte_stream = response.bytes_stream();

        tokio::spawn(async move {
            let start_instant = Instant::now();
            let mut buffer = Vec::new();

            // Emit Started event first
            if tx
                .send(Ok(InferenceEvent::Started {
                    request_id: request_id.clone(),
                    model_id: model_id.clone(),
                }))
                .await
                .is_err()
            {
                return;
            }

            let overall_timer = tokio::time::sleep(overall_timeout);
            tokio::pin!(overall_timer);

            loop {
                tokio::select! {
                    _ = token.cancelled() => {
                        let _ = tx.send(Ok(InferenceEvent::Cancelled {
                            request_id: request_id.clone(),
                        })).await;
                        return;
                    }
                    _ = &mut overall_timer => {
                        let _ = tx.send(Err(InferenceError::Timeout(overall_timeout))).await;
                        return;
                    }
                    chunk_result = tokio::time::timeout(idle_timeout, byte_stream.next()) => {
                        match chunk_result {
                            Err(_) => {
                                let _ = tx.send(Err(InferenceError::Timeout(idle_timeout))).await;
                                return;
                            }
                            Ok(None) => {
                                // Stream ended
                                break;
                            }
                            Ok(Some(Err(err))) => {
                                let _ = tx.send(Err(InferenceError::Other(format!("Network stream error: {}", err)))).await;
                                return;
                            }
                            Ok(Some(Ok(bytes))) => {
                                buffer.extend_from_slice(&bytes);

                                if buffer.len() > MAX_LINE_BUFFER_BYTES && !buffer.contains(&b'\n') {
                                    let _ = tx.send(Err(InferenceError::MalformedResponse(
                                        "NDJSON line exceeds maximum allowable buffer size (64 KiB)".to_string(),
                                    ))).await;
                                    return;
                                }

                                while let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
                                    let line_bytes: Vec<u8> = buffer.drain(..=pos).collect();
                                    let trimmed = line_bytes.strip_suffix(b"\n").unwrap_or(&line_bytes);
                                    let trimmed = trimmed.strip_suffix(b"\r").unwrap_or(trimmed);

                                    if trimmed.is_empty() {
                                        continue;
                                    }

                                    let line_str = match std::str::from_utf8(trimmed) {
                                        Ok(s) => s,
                                        Err(e) => {
                                            let _ = tx.send(Err(InferenceError::MalformedResponse(
                                                format!("Invalid UTF-8 in NDJSON stream: {}", e),
                                            ))).await;
                                            return;
                                        }
                                    };

                                    let chunk: OllamaChatStreamChunk = match serde_json::from_str(line_str) {
                                        Ok(c) => c,
                                        Err(e) => {
                                            let _ = tx.send(Err(InferenceError::MalformedResponse(
                                                format!("Invalid JSON in NDJSON stream: {}", e),
                                            ))).await;
                                            return;
                                        }
                                    };

                                    if let Some(err_msg) = chunk.error {
                                        let _ = tx.send(Err(InferenceError::Other(err_msg))).await;
                                        return;
                                    }

                                    if let Some(msg) = chunk.message.filter(|m| !m.content.is_empty())
                                        && tx
                                            .send(Ok(InferenceEvent::Delta {
                                                request_id: request_id.clone(),
                                                content: msg.content,
                                            }))
                                            .await
                                            .is_err()
                                    {
                                        return; // consumer dropped
                                    }

                                    if chunk.done == Some(true) {
                                        let usage = if chunk.prompt_eval_count.is_some() || chunk.eval_count.is_some() {
                                            let p = chunk.prompt_eval_count;
                                            let c = chunk.eval_count;
                                            let total = match (p, c) {
                                                (Some(pv), Some(cv)) => Some(pv + cv),
                                                (Some(pv), None) => Some(pv),
                                                (None, Some(cv)) => Some(cv),
                                                (None, None) => None,
                                            };
                                            Some(UsageInfo {
                                                prompt_tokens: p,
                                                completion_tokens: c,
                                                total_tokens: total,
                                            })
                                        } else {
                                            None
                                        };

                                        let duration_ms = start_instant.elapsed().as_millis() as u64;

                                        let _ = tx.send(Ok(InferenceEvent::Completed {
                                            request_id: request_id.clone(),
                                            model_id: model_id.clone(),
                                            usage,
                                            duration_ms,
                                            finish_reason: chunk.done_reason.or_else(|| Some("stop".to_string())),
                                        })).await;
                                        return;
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // If EOF was reached, check if a valid done: true was left in buffer without a trailing newline
            let trimmed = buffer.strip_suffix(b"\r").unwrap_or(&buffer);
            if let Some(chunk) = std::str::from_utf8(trimmed)
                .ok()
                .and_then(|s| serde_json::from_str::<OllamaChatStreamChunk>(s).ok())
                .filter(|c| c.done == Some(true))
            {
                let duration_ms = start_instant.elapsed().as_millis() as u64;
                let _ = tx
                    .send(Ok(InferenceEvent::Completed {
                        request_id: request_id.clone(),
                        model_id: model_id.clone(),
                        usage: None,
                        duration_ms,
                        finish_reason: chunk.done_reason.or_else(|| Some("stop".to_string())),
                    }))
                    .await;
                return;
            }

            // If not completed, premature EOF
            let _ = tx
                .send(Err(InferenceError::MalformedResponse(
                    "Premature EOF without completion signal".to_string(),
                )))
                .await;
        });

        Ok(Box::pin(ReceiverStream(rx)))
    }
}

#[derive(Debug, Clone, Deserialize)]
struct OllamaTagsResponse {
    models: Option<Vec<OllamaModelItem>>,
}

#[derive(Debug, Clone, Deserialize)]
struct OllamaModelItem {
    name: String,
    modified_at: Option<String>,
    size: Option<u64>,
    details: Option<OllamaModelDetails>,
}

#[derive(Debug, Clone, Deserialize)]
struct OllamaModelDetails {
    family: Option<String>,
    parameter_size: Option<String>,
    quantization_level: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct OllamaShowResponse {
    capabilities: Option<Vec<String>>,
    details: Option<OllamaModelDetails>,
    model_info: Option<HashMap<String, serde_json::Value>>,
    template: Option<String>,
}

#[derive(Debug, Serialize)]
struct OllamaChatPayload<'a> {
    model: &'a str,
    messages: &'a [OllamaChatMessage],
    stream: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct OllamaChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Clone, Deserialize)]
struct OllamaStreamMessage {
    #[serde(default)]
    #[allow(dead_code)]
    role: Option<String>,
    #[serde(default)]
    content: String,
}

#[derive(Debug, Deserialize)]
struct OllamaChatResponse {
    message: Option<OllamaStreamMessage>,
    done: Option<bool>,
    done_reason: Option<String>,
    prompt_eval_count: Option<u64>,
    eval_count: Option<u64>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OllamaChatStreamChunk {
    message: Option<OllamaStreamMessage>,
    done: Option<bool>,
    done_reason: Option<String>,
    prompt_eval_count: Option<u64>,
    eval_count: Option<u64>,
    error: Option<String>,
}

impl ModelDiscovery for OllamaProvider {
    fn provider_id(&self) -> &str {
        "ollama"
    }

    async fn discover(&self) -> Result<Vec<ModelInfo>, DiscoveryError> {
        self.clear_capabilities_cache();
        let url = format!("{}/api/tags", self.endpoint.trim_end_matches('/'));

        let response = match self.discovery_client.get(&url).send().await {
            Ok(resp) => resp,
            Err(err) => {
                if err.is_connect() || err.is_timeout() {
                    return Err(DiscoveryError::Unreachable(err.to_string()));
                }
                return Err(DiscoveryError::Other(err.to_string()));
            }
        };

        let status = response.status();
        if !status.is_success() {
            let message = response
                .text()
                .await
                .unwrap_or_else(|_| format!("HTTP {}", status.as_u16()));
            return Err(DiscoveryError::HttpStatus {
                code: status.as_u16(),
                message,
            });
        }

        let body_bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(err) => {
                if err.is_timeout() {
                    return Err(DiscoveryError::Unreachable(err.to_string()));
                }
                return Err(DiscoveryError::Other(err.to_string()));
            }
        };

        let tags_response: OllamaTagsResponse = match serde_json::from_slice(&body_bytes) {
            Ok(parsed) => parsed,
            Err(err) => {
                return Err(DiscoveryError::MalformedJson(err.to_string()));
            }
        };

        let models = tags_response
            .models
            .unwrap_or_default()
            .into_iter()
            .map(|item| {
                let upstream_id = item.name.clone();
                let id = format!("ollama/{}", upstream_id);
                let (parameter_size, quantization_level) = match item.details {
                    Some(details) => (details.parameter_size, details.quantization_level),
                    None => (None, None),
                };

                ModelInfo {
                    id,
                    provider_id: "ollama".to_string(),
                    upstream_id: upstream_id.clone(),
                    display_name: upstream_id,
                    capabilities: ModelCapabilities {
                        chat: None,
                        streaming: None,
                        tools: None,
                        vision: None,
                    },
                    size_bytes: item.size,
                    parameter_size,
                    quantization_level,
                    modified_at: item.modified_at,
                }
            })
            .collect();

        Ok(models)
    }
}

impl ModelInference for OllamaProvider {
    async fn generate(
        &self,
        request: InferenceRequest,
    ) -> Result<InferenceResponse, InferenceError> {
        let upstream_model = request
            .model_id
            .strip_prefix("ollama/")
            .unwrap_or(&request.model_id)
            .to_string();
        self.generate_with_upstream(&upstream_model, request).await
    }

    async fn stream(
        &self,
        request: InferenceRequest,
        cancellation_token: CancellationToken,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<InferenceEvent, InferenceError>> + Send>>,
        InferenceError,
    > {
        let upstream_model = request
            .model_id
            .strip_prefix("ollama/")
            .unwrap_or(&request.model_id)
            .to_string();
        self.stream_with_upstream(&upstream_model, request, cancellation_token)
            .await
    }
}
