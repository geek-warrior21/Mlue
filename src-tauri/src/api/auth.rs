use super::error::ApiError;
use crate::keys::ApiKeyService;
use crate::keys::types::{ApiKey, ApiKeyError};
use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct AuthenticatedKey(pub ApiKey);

pub async fn require_api_key(
    State(key_service): State<Arc<ApiKeyService>>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let auth_header = req
        .headers()
        .get(AUTHORIZATION)
        .ok_or_else(|| ApiError::unauthorized("Missing Authorization header", "missing_api_key"))?;

    let auth_str = auth_header.to_str().map_err(|_| {
        ApiError::unauthorized(
            "Invalid Authorization header encoding. Header must be valid ASCII/UTF-8",
            "invalid_api_key",
        )
    })?;

    // Strict parsing: must start with "Bearer "
    if !auth_str.starts_with("Bearer ") {
        return Err(ApiError::unauthorized(
            "Invalid Authorization header format. Expected 'Bearer <key>'",
            "invalid_api_key",
        ));
    }

    let raw_key = auth_str["Bearer ".len()..].trim();
    if raw_key.is_empty() {
        return Err(ApiError::unauthorized(
            "Empty API key provided in Bearer token",
            "invalid_api_key",
        ));
    }

    // Verify key with ApiKeyService (constant-time, checks revocation, enabled, and project existence)
    let verified_key = match key_service.verify_key(raw_key).await {
        Ok(k) => k,
        Err(ApiKeyError::Revoked) => {
            return Err(ApiError::unauthorized(
                "API key has been revoked",
                "revoked_api_key",
            ));
        }
        Err(ApiKeyError::Disabled) => {
            return Err(ApiError::unauthorized(
                "API key is disabled",
                "disabled_api_key",
            ));
        }
        Err(ApiKeyError::InvalidKey | ApiKeyError::NotFound) => {
            return Err(ApiError::unauthorized(
                "Incorrect API key provided",
                "invalid_api_key",
            ));
        }
        Err(err) => {
            return Err(ApiError::unauthorized(
                format!("Authentication error: {}", err),
                "invalid_api_key",
            ));
        }
    };

    req.extensions_mut().insert(AuthenticatedKey(verified_key));
    Ok(next.run(req).await)
}
