pub mod auth;
pub mod chat;
pub mod error;
pub mod health;
pub mod models;
pub mod security;

use crate::inference::InferenceManager;
use crate::keys::ApiKeyService;
use crate::providers::ollama::OllamaProvider;
use crate::registry::ModelRegistry;
use axum::Router;
use axum::middleware;
use axum::routing::{get, post};
use std::sync::Arc;

pub use auth::{AuthenticatedKey, require_api_key};
pub use chat::{
    ChatCompletionState, OpenAiChatCompletion, OpenAiChatCompletionChunk,
    OpenAiChatCompletionRequest, OpenAiChunkChoice, OpenAiChunkDelta, StreamOptions,
    chat_completions_handler,
};
pub use error::{ApiError, AppJson, OpenAiErrorResponse};
pub use health::health_handler;
pub use models::{ModelListResponse, OpenAiModel, list_models_handler};

#[derive(Clone)]
pub struct ApiRouterContext {
    pub model_registry: Arc<ModelRegistry>,
    pub ollama_provider: Arc<OllamaProvider>,
    pub inference_manager: Arc<InferenceManager>,
    pub api_key_service: Arc<ApiKeyService>,
}

pub fn create_router() -> Router {
    Router::new().route("/health", get(health::health_handler))
}

pub fn create_api_router(context: ApiRouterContext) -> Router {
    let chat_state = Arc::new(ChatCompletionState {
        registry: context.model_registry.clone(),
        provider: context.ollama_provider.clone(),
        inference_manager: context.inference_manager.clone(),
        key_service: context.api_key_service.clone(),
    });

    let v1_routes = Router::new()
        .route("/models", get(models::list_models_handler))
        .with_state(context.model_registry.clone())
        .route("/chat/completions", post(chat::chat_completions_handler))
        .with_state(chat_state)
        .route_layer(middleware::from_fn_with_state(
            context.api_key_service.clone(),
            auth::require_api_key,
        ));

    Router::new()
        .route("/health", get(health::health_handler))
        .nest("/v1", v1_routes)
}
