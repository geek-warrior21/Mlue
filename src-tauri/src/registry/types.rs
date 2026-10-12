use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ModelCapabilities {
    pub chat: Option<bool>,
    pub streaming: Option<bool>,
    pub tools: Option<bool>,
    pub vision: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    pub provider_id: String,
    pub upstream_id: String,
    pub display_name: String,
    pub capabilities: ModelCapabilities,
    pub size_bytes: Option<u64>,
    pub parameter_size: Option<String>,
    pub quantization_level: Option<String>,
    pub modified_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStatus {
    Checking,
    Ready,
    NoModels,
    Unreachable,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiscoveredModelsResponse {
    pub provider_id: String,
    pub status: ProviderStatus,
    pub models: Vec<ModelInfo>,
    pub last_checked_at: Option<String>,
    pub last_successful_at: Option<String>,
    pub error: Option<String>,
}

impl DiscoveredModelsResponse {
    pub fn initial(provider_id: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            status: ProviderStatus::Checking,
            models: Vec::new(),
            last_checked_at: None,
            last_successful_at: None,
            error: None,
        }
    }
}
