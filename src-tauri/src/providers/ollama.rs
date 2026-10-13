use crate::inference::types::{InferenceRequest, InferenceResponse, MessageRole, UsageInfo};
use crate::inference::{InferenceError, ModelInference};
use crate::providers::{DiscoveryError, ModelDiscovery};
use crate::registry::types::{ModelCapabilities, ModelInfo};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

pub const DEFAULT_OLLAMA_ENDPOINT: &str = "http://127.0.0.1:11434";
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);
pub const DEFAULT_DISCOVERY_TIMEOUT: Duration = Duration::from_millis(3000);
pub const DEFAULT_INFERENCE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct OllamaProvider {
    endpoint: String,
    discovery_client: reqwest::Client,
    inference_client: reqwest::Client,
    inference_timeout: Duration,
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

        let start = std::time::Instant::now();

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

#[derive(Debug, Deserialize)]
struct OllamaChatResponse {
    message: Option<OllamaChatMessage>,
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
}
