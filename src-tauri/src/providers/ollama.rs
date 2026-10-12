use crate::providers::{DiscoveryError, ModelDiscovery};
use crate::registry::types::{ModelCapabilities, ModelInfo};
use serde::Deserialize;
use std::time::Duration;

pub const DEFAULT_OLLAMA_ENDPOINT: &str = "http://127.0.0.1:11434";
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_millis(3000);

#[derive(Debug, Clone)]
pub struct OllamaProvider {
    endpoint: String,
    client: reqwest::Client,
}

impl OllamaProvider {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self::with_timeouts(endpoint, DEFAULT_CONNECT_TIMEOUT, DEFAULT_REQUEST_TIMEOUT)
    }

    pub fn with_timeouts(
        endpoint: impl Into<String>,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            endpoint: endpoint.into(),
            client,
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
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
    parameter_size: Option<String>,
    quantization_level: Option<String>,
}

impl ModelDiscovery for OllamaProvider {
    fn provider_id(&self) -> &str {
        "ollama"
    }

    async fn discover(&self) -> Result<Vec<ModelInfo>, DiscoveryError> {
        let url = format!("{}/api/tags", self.endpoint.trim_end_matches('/'));

        let response = match self.client.get(&url).send().await {
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
