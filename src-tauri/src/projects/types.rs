use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MAX_PROJECT_NAME_LENGTH: usize = 100;
pub const MAX_PROJECT_DESC_LENGTH: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProjectInput {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProjectInput {
    pub name: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Error)]
pub enum ProjectError {
    #[error("Project '{0}' not found")]
    NotFound(String),
    #[error("Invalid project input: {0}")]
    InvalidInput(String),
    #[error("Database error: {0}")]
    Database(String),
}

impl From<sqlx::Error> for ProjectError {
    fn from(err: sqlx::Error) -> Self {
        Self::Database(err.to_string())
    }
}
