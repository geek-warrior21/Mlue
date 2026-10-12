use axum::Json;
use serde::{Deserialize, Serialize};

pub const SERVICE_NAME: &str = "mlue";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealthResponse {
    pub status: String,
    pub service: String,
    pub version: String,
}

pub async fn health_handler() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        service: SERVICE_NAME.to_string(),
        version: APP_VERSION.to_string(),
    })
}
