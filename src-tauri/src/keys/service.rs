use super::generator::{generate_api_key, hash_api_key, validate_key_format, verify_hash};
use super::types::{
    AccessPolicy, ApiKey, ApiKeyError, ApiKeyMetadata, CreatedApiKey, MAX_KEY_NAME_LENGTH,
};
use sqlx::SqlitePool;
use uuid::Uuid;

#[derive(Clone)]
pub struct ApiKeyService {
    pool: SqlitePool,
}

impl ApiKeyService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn create_key(
        &self,
        project_id: &str,
        name: &str,
        access_policy: Option<AccessPolicy>,
    ) -> Result<CreatedApiKey, ApiKeyError> {
        let trimmed_name = name.trim();
        if trimmed_name.is_empty() {
            return Err(ApiKeyError::InvalidInput(
                "API key name cannot be empty".to_string(),
            ));
        }
        if trimmed_name.len() > MAX_KEY_NAME_LENGTH {
            return Err(ApiKeyError::InvalidInput(format!(
                "API key name exceeds maximum length of {} characters",
                MAX_KEY_NAME_LENGTH
            )));
        }

        // Verify project exists
        let project_exists: Option<String> =
            sqlx::query_scalar("SELECT id FROM projects WHERE id = ?;")
                .bind(project_id)
                .fetch_optional(&self.pool)
                .await?;

        if project_exists.is_none() {
            return Err(ApiKeyError::ProjectNotFound(project_id.to_string()));
        }

        let policy = access_policy.unwrap_or_default();
        let policy_json =
            serde_json::to_string(&policy).map_err(|e| ApiKeyError::InvalidInput(e.to_string()))?;

        let generated = generate_api_key();
        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();

        sqlx::query(
            r#"
            INSERT INTO api_keys (
                id, project_id, name, key_prefix, key_hash, access_policy, enabled, created_at, last_used_at, revoked_at
            )
            VALUES (?, ?, ?, ?, ?, ?, 1, ?, NULL, NULL);
            "#,
        )
        .bind(&id)
        .bind(project_id)
        .bind(trimmed_name)
        .bind(&generated.key_prefix)
        .bind(&generated.key_hash)
        .bind(&policy_json)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        let metadata = ApiKeyMetadata {
            id,
            project_id: project_id.to_string(),
            name: trimmed_name.to_string(),
            key_prefix: generated.key_prefix,
            access_policy: policy,
            enabled: true,
            created_at: now,
            last_used_at: None,
            revoked_at: None,
        };

        Ok(CreatedApiKey {
            metadata,
            full_key: generated.full_key,
        })
    }

    pub async fn list_keys(&self, project_id: &str) -> Result<Vec<ApiKeyMetadata>, ApiKeyError> {
        let rows = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                String,
                String,
                i64,
                String,
                Option<String>,
                Option<String>,
            ),
        >(
            r#"
            SELECT id, project_id, name, key_prefix, access_policy, enabled, created_at, last_used_at, revoked_at
            FROM api_keys
            WHERE project_id = ?
            ORDER BY created_at DESC;
            "#,
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;

        let keys = rows
            .into_iter()
            .map(
                |(
                    id,
                    proj_id,
                    name,
                    key_prefix,
                    policy_str,
                    enabled,
                    created_at,
                    last_used_at,
                    revoked_at,
                )| {
                    let access_policy: AccessPolicy =
                        serde_json::from_str(&policy_str).unwrap_or_default();
                    ApiKeyMetadata {
                        id,
                        project_id: proj_id,
                        name,
                        key_prefix,
                        access_policy,
                        enabled: enabled != 0,
                        created_at,
                        last_used_at,
                        revoked_at,
                    }
                },
            )
            .collect();

        Ok(keys)
    }

    pub async fn get_key(&self, id: &str) -> Result<ApiKey, ApiKeyError> {
        let row = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                String,
                String,
                String,
                i64,
                String,
                Option<String>,
                Option<String>,
            ),
        >(
            r#"
            SELECT id, project_id, name, key_prefix, key_hash, access_policy, enabled, created_at, last_used_at, revoked_at
            FROM api_keys
            WHERE id = ?;
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some((
                id,
                project_id,
                name,
                key_prefix,
                key_hash,
                policy_str,
                enabled,
                created_at,
                last_used_at,
                revoked_at,
            )) => {
                let access_policy: AccessPolicy =
                    serde_json::from_str(&policy_str).unwrap_or_default();
                Ok(ApiKey {
                    id,
                    project_id,
                    name,
                    key_prefix,
                    key_hash,
                    access_policy,
                    enabled: enabled != 0,
                    created_at,
                    last_used_at,
                    revoked_at,
                })
            }
            None => Err(ApiKeyError::NotFound),
        }
    }

    pub async fn verify_key(&self, raw_key: &str) -> Result<ApiKey, ApiKeyError> {
        if !validate_key_format(raw_key) {
            return Err(ApiKeyError::InvalidKey);
        }

        let candidate_hash = hash_api_key(raw_key);

        let row = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                String,
                String,
                String,
                i64,
                String,
                Option<String>,
                Option<String>,
            ),
        >(
            r#"
            SELECT id, project_id, name, key_prefix, key_hash, access_policy, enabled, created_at, last_used_at, revoked_at
            FROM api_keys
            WHERE key_hash = ?;
            "#,
        )
        .bind(&candidate_hash)
        .fetch_optional(&self.pool)
        .await?;

        let (
            id,
            project_id,
            name,
            key_prefix,
            stored_hash,
            policy_str,
            enabled,
            created_at,
            _last_used_at,
            revoked_at,
        ) = match row {
            Some(r) => r,
            None => return Err(ApiKeyError::InvalidKey),
        };

        // Constant-time hash verification to prevent timing side channels
        if !verify_hash(&candidate_hash, &stored_hash) {
            return Err(ApiKeyError::InvalidKey);
        }

        // Check revocation
        if revoked_at.is_some() {
            return Err(ApiKeyError::Revoked);
        }

        // Check enabled state
        if enabled == 0 {
            return Err(ApiKeyError::Disabled);
        }

        // Update last_used_at timestamp
        let now = chrono::Utc::now().to_rfc3339();
        let _ = sqlx::query("UPDATE api_keys SET last_used_at = ? WHERE id = ?;")
            .bind(&now)
            .bind(&id)
            .execute(&self.pool)
            .await;

        let access_policy: AccessPolicy = serde_json::from_str(&policy_str).unwrap_or_default();

        Ok(ApiKey {
            id,
            project_id,
            name,
            key_prefix,
            key_hash: stored_hash,
            access_policy,
            enabled: true,
            created_at,
            last_used_at: Some(now),
            revoked_at: None,
        })
    }

    pub async fn revoke_key(&self, key_id: &str) -> Result<bool, ApiKeyError> {
        let now = chrono::Utc::now().to_rfc3339();
        let res = sqlx::query(
            "UPDATE api_keys SET revoked_at = ?, enabled = 0 WHERE id = ? AND revoked_at IS NULL;",
        )
        .bind(&now)
        .bind(key_id)
        .execute(&self.pool)
        .await?;

        if res.rows_affected() == 0 {
            // Check if key existed
            let exists: Option<String> =
                sqlx::query_scalar("SELECT id FROM api_keys WHERE id = ?;")
                    .bind(key_id)
                    .fetch_optional(&self.pool)
                    .await?;
            if exists.is_none() {
                return Err(ApiKeyError::NotFound);
            }
            // Key already revoked
            return Ok(false);
        }

        Ok(true)
    }

    pub fn check_model_access(&self, api_key: &ApiKey, model_id: &str) -> Result<(), ApiKeyError> {
        match &api_key.access_policy {
            AccessPolicy::LocalOnly => {
                // Local only permits Ollama, local models, or the "auto" local alias
                let is_local = model_id.starts_with("ollama/")
                    || model_id.starts_with("local/")
                    || model_id == "auto";

                let is_explicitly_nonlocal = model_id.starts_with("openai/")
                    || model_id.starts_with("gemini/")
                    || model_id.starts_with("anthropic/")
                    || model_id.starts_with("cloud/")
                    || model_id.starts_with("remote/");

                if is_local && !is_explicitly_nonlocal {
                    Ok(())
                } else {
                    Err(ApiKeyError::UnauthorizedModel(format!(
                        "Model '{}' is not permitted under local_only access policy",
                        model_id
                    )))
                }
            }
            AccessPolicy::SelectedModels { models } => {
                if model_id == "auto" {
                    if !models.is_empty() {
                        return Ok(());
                    } else {
                        return Err(ApiKeyError::UnauthorizedModel(
                            "No models configured in selected_models policy".to_string(),
                        ));
                    }
                }

                if models.iter().any(|m| m == model_id) {
                    Ok(())
                } else {
                    Err(ApiKeyError::UnauthorizedModel(format!(
                        "Model '{}' is not in the allowed models list: {:?}",
                        model_id, models
                    )))
                }
            }
        }
    }
}
