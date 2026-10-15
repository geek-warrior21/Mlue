use super::auth::AuthenticatedKey;
use super::error::ApiError;
use crate::keys::types::AccessPolicy;
use crate::registry::{ModelRegistry, ProviderStatus};
use axum::Json;
use axum::extract::{Extension, State};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiModel {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub owned_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelListResponse {
    pub object: String,
    pub data: Vec<OpenAiModel>,
}

pub async fn list_models_handler(
    Extension(AuthenticatedKey(key)): Extension<AuthenticatedKey>,
    State(registry): State<Arc<ModelRegistry>>,
) -> Result<Json<ModelListResponse>, ApiError> {
    let snapshot = registry.get_snapshot();

    // Unavailable models must not be presented as ready
    if snapshot.status != ProviderStatus::Ready {
        return Ok(Json(ModelListResponse {
            object: "list".to_string(),
            data: Vec::new(),
        }));
    }

    let mut filtered_models = Vec::new();

    for model in snapshot.models {
        let is_allowed = match &key.access_policy {
            AccessPolicy::LocalOnly => model.provider_id == "ollama",
            AccessPolicy::SelectedModels { models } => models.contains(&model.id),
        };

        if is_allowed {
            let created_ts = model
                .modified_at
                .as_deref()
                .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
                .map(|dt| dt.timestamp())
                .unwrap_or(0);

            filtered_models.push(OpenAiModel {
                id: model.id,
                object: "model".to_string(),
                created: created_ts,
                owned_by: "mlue".to_string(),
            });
        }
    }

    Ok(Json(ModelListResponse {
        object: "list".to_string(),
        data: filtered_models,
    }))
}
