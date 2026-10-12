pub mod ollama;

pub use ollama::OllamaProvider;

use crate::registry::types::ModelInfo;

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("Provider unreachable: {0}")]
    Unreachable(String),

    #[error("HTTP error {code}: {message}")]
    HttpStatus { code: u16, message: String },

    #[error("Malformed JSON response: {0}")]
    MalformedJson(String),

    #[error("Discovery failure: {0}")]
    Other(String),
}

pub trait ModelDiscovery: Send + Sync {
    fn provider_id(&self) -> &str;
    fn discover(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<ModelInfo>, DiscoveryError>> + Send;
}

impl<T: ModelDiscovery + ?Sized> ModelDiscovery for std::sync::Arc<T> {
    fn provider_id(&self) -> &str {
        (**self).provider_id()
    }

    fn discover(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<ModelInfo>, DiscoveryError>> + Send {
        (**self).discover()
    }
}
