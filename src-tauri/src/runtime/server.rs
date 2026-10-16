use crate::api::create_router;
use crate::inference::InferenceManager;
use crate::keys::ApiKeyService;
use crate::providers::ollama::OllamaProvider;
use crate::registry::ModelRegistry;
use crate::runtime::state::{AppState, RuntimeStatus};
use axum::http::{HeaderValue, Method};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tower_http::cors::CorsLayer;

pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 4782;

pub fn default_endpoint() -> String {
    format!("http://{}:{}", DEFAULT_HOST, DEFAULT_PORT)
}

#[derive(Clone)]
pub struct ServerContext {
    pub app_state: Arc<AppState>,
    pub model_registry: Arc<ModelRegistry>,
    pub ollama_provider: Arc<OllamaProvider>,
    pub inference_manager: Arc<InferenceManager>,
    pub api_key_service: Arc<ApiKeyService>,
}

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("Failed to bind to {addr}: {source}")]
    BindError {
        addr: String,
        #[source]
        source: std::io::Error,
    },
    #[error("Server execution failure: {0}")]
    ServeError(String),
}

pub fn get_cors_layer() -> CorsLayer {
    #[cfg(debug_assertions)]
    let allowed = [
        "tauri://localhost".parse::<HeaderValue>().unwrap(),
        "http://tauri.localhost".parse::<HeaderValue>().unwrap(),
        "http://localhost:1420".parse::<HeaderValue>().unwrap(),
        "http://127.0.0.1:1420".parse::<HeaderValue>().unwrap(),
    ];

    #[cfg(not(debug_assertions))]
    let allowed = [
        "tauri://localhost".parse::<HeaderValue>().unwrap(),
        "http://tauri.localhost".parse::<HeaderValue>().unwrap(),
    ];

    CorsLayer::new()
        .allow_origin(allowed)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([
            axum::http::header::AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
        ])
}

pub fn build_app_router() -> axum::Router {
    create_router()
        .layer(get_cors_layer())
        .layer(axum::middleware::from_fn(
            crate::api::security::validate_host_and_origin,
        ))
        .layer(axum::extract::DefaultBodyLimit::max(256 * 1024))
}

pub fn build_app_router_with_context(context: ServerContext) -> axum::Router {
    let api_ctx = crate::api::ApiRouterContext {
        model_registry: context.model_registry,
        ollama_provider: context.ollama_provider,
        inference_manager: context.inference_manager,
        api_key_service: context.api_key_service,
    };

    crate::api::create_api_router(api_ctx)
        .layer(get_cors_layer())
        .layer(axum::middleware::from_fn(
            crate::api::security::validate_host_and_origin,
        ))
        .layer(axum::extract::DefaultBodyLimit::max(256 * 1024))
}

pub async fn run_server(
    host: &str,
    port: u16,
    state: Arc<AppState>,
    shutdown_rx: watch::Receiver<bool>,
) -> Result<(), ServerError> {
    run_server_with_context(host, port, state, None, shutdown_rx).await
}

pub async fn run_server_with_context(
    host: &str,
    port: u16,
    state: Arc<AppState>,
    context: Option<ServerContext>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<(), ServerError> {
    let endpoint = format!("http://{}:{}", host, port);
    let bind_addr = format!("{}:{}", host, port);

    // Enforce loopback binding: binding to 0.0.0.0 or non-loopback is forbidden
    if host != "127.0.0.1" && host != "localhost" && host != "::1" {
        let err_msg = format!("Binding to non-loopback host '{}' is prohibited", host);
        state.set_status(RuntimeStatus::error(&endpoint, &err_msg));
        return Err(ServerError::ServeError(err_msg));
    }

    let socket_addr: SocketAddr = match bind_addr.parse() {
        Ok(addr) => addr,
        Err(err) => {
            let err_msg = format!("Invalid bind address '{}': {}", bind_addr, err);
            state.set_status(RuntimeStatus::error(&endpoint, &err_msg));
            return Err(ServerError::ServeError(err_msg));
        }
    };

    let listener = match TcpListener::bind(socket_addr).await {
        Ok(l) => l,
        Err(err) => {
            let err_msg = format!("Failed to bind to {}: {}", bind_addr, err);
            state.set_status(RuntimeStatus::error(&endpoint, &err_msg));
            return Err(ServerError::BindError {
                addr: bind_addr,
                source: err,
            });
        }
    };

    // Server successfully bound port
    state.set_status(RuntimeStatus::running(&endpoint));

    let router = match context {
        Some(ctx) => build_app_router_with_context(ctx),
        None => build_app_router(),
    };

    let server_result = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.wait_for(|&is_shutdown| is_shutdown).await;
        })
        .await;

    match server_result {
        Ok(()) => {
            state.set_status(RuntimeStatus::stopped(&endpoint));
            Ok(())
        }
        Err(err) => {
            let err_msg = format!("Server error: {}", err);
            state.set_status(RuntimeStatus::error(&endpoint, &err_msg));
            Err(ServerError::ServeError(err_msg))
        }
    }
}
