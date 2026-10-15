use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MAX_KEY_NAME_LENGTH: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AccessPolicy {
    #[default]
    LocalOnly,
    SelectedModels {
        models: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub key_prefix: String,
    pub key_hash: String,
    pub access_policy: AccessPolicy,
    pub enabled: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

impl ApiKey {
    pub fn is_active(&self) -> bool {
        self.enabled && self.revoked_at.is_none()
    }

    pub fn to_metadata(&self) -> ApiKeyMetadata {
        ApiKeyMetadata {
            id: self.id.clone(),
            project_id: self.project_id.clone(),
            name: self.name.clone(),
            key_prefix: self.key_prefix.clone(),
            access_policy: self.access_policy.clone(),
            enabled: self.enabled,
            created_at: self.created_at.clone(),
            last_used_at: self.last_used_at.clone(),
            revoked_at: self.revoked_at.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiKeyMetadata {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub key_prefix: String,
    pub access_policy: AccessPolicy,
    pub enabled: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

impl ApiKeyMetadata {
    pub fn is_active(&self) -> bool {
        self.enabled && self.revoked_at.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatedApiKey {
    pub metadata: ApiKeyMetadata,
    pub full_key: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateApiKeyInput {
    pub project_id: String,
    pub name: String,
    pub access_policy: Option<AccessPolicy>,
}

#[derive(Debug, Error)]
pub enum ApiKeyError {
    #[error("API key not found")]
    NotFound,
    #[error("API key has been revoked")]
    Revoked,
    #[error("API key is disabled")]
    Disabled,
    #[error("Invalid API key credentials or format")]
    InvalidKey,
    #[error("Project '{0}' not found")]
    ProjectNotFound(String),
    #[error("Invalid input: {0}")]
    InvalidInput(String),
    #[error("Model '{0}' is not authorized by this API key's access policy")]
    UnauthorizedModel(String),
    #[error("Model '{0}' not found or not registered")]
    ModelNotFound(String),
    #[error("No compatible models are available under the current access policy")]
    NoEligibleModels,
    #[error("Database error: {0}")]
    Database(String),
}

impl From<sqlx::Error> for ApiKeyError {
    fn from(err: sqlx::Error) -> Self {
        Self::Database(err.to_string())
    }
}
