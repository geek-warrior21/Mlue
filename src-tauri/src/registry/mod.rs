pub mod types;

use crate::providers::{DiscoveryError, ModelDiscovery};
use chrono::Utc;
use std::sync::RwLock;
use tokio::sync::Mutex;

pub use types::{DiscoveredModelsResponse, ModelCapabilities, ModelInfo, ProviderStatus};

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
}
