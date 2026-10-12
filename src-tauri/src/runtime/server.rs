use crate::api::create_router;
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

pub fn build_app_router() -> axum::Router {
    // Explicit CORS configuration restricting origins and methods; avoids wildcard origins
    let cors = CorsLayer::new()
        .allow_origin([
            "http://localhost:1420".parse::<HeaderValue>().unwrap(),
            "http://tauri.localhost".parse::<HeaderValue>().unwrap(),
            "tauri://localhost".parse::<HeaderValue>().unwrap(),
            "http://127.0.0.1:1420".parse::<HeaderValue>().unwrap(),
        ])
        .allow_methods([Method::GET]);

    create_router().layer(cors)
}

pub async fn run_server(
    host: &str,
    port: u16,
    state: Arc<AppState>,
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

    let router = build_app_router();

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
