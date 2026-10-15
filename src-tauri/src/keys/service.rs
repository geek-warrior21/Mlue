use super::generator::{generate_api_key, hash_api_key, validate_key_format, verify_hash};
use super::types::{
    AccessPolicy, ApiKey, ApiKeyError, ApiKeyMetadata, CreatedApiKey, MAX_KEY_NAME_LENGTH,
};
use crate::registry::{ModelInfo, ModelRegistry, ProviderStatus};
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

        // Revalidate key and project ownership: join with projects table
        // This guarantees that keys belonging to deleted projects are rejected immediately.
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
            SELECT k.id, k.project_id, k.name, k.key_prefix, k.key_hash, k.access_policy,
                   k.enabled, k.created_at, k.last_used_at, k.revoked_at
            FROM api_keys k
            INNER JOIN projects p ON k.project_id = p.id
            WHERE k.key_hash = ?;
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
            last_used_at,
            revoked_at,
        ) = match row {
            Some(r) => r,
            None => return Err(ApiKeyError::InvalidKey),
        };

        // Constant-time hash verification to prevent timing side channels
        if !verify_hash(&candidate_hash, &stored_hash) {
            return Err(ApiKeyError::InvalidKey);
        }

        // Check revocation - immediate and never cached
        if revoked_at.is_some() {
            return Err(ApiKeyError::Revoked);
        }

        // Check enabled state
        if enabled == 0 {
            return Err(ApiKeyError::Disabled);
        }

        // Debounced last_used_at update: at most once every 60 seconds per key.
        // Best-effort execution without blocking authentication or creating write contention.
        let should_update = match &last_used_at {
            None => true,
            Some(ts_str) => {
                if let Ok(last_dt) = chrono::DateTime::parse_from_rfc3339(ts_str) {
                    let now = chrono::Utc::now();
                    now.signed_duration_since(last_dt).num_seconds() >= 60
                } else {
                    true
                }
            }
        };

        let now_str = chrono::Utc::now().to_rfc3339();
        let final_last_used = if should_update {
            let pool = self.pool.clone();
            let key_id = id.clone();
            let ts = now_str.clone();
            tokio::spawn(async move {
                let _ = sqlx::query("UPDATE api_keys SET last_used_at = ? WHERE id = ?;")
                    .bind(&ts)
                    .bind(&key_id)
                    .execute(&pool)
                    .await;
            });
            Some(now_str)
        } else {
            last_used_at
        };

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
            last_used_at: final_last_used,
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

    /// Authorizes a requested model against trusted registry metadata and the key's access policy.
    /// This is fail-closed: it enforces registry presence, provider status, trusted local provider
    /// identity, and explicit allowlists. Textual prefix tricks (e.g. "ollama/openai/gpt-4o") fail closed.
    pub fn authorize_model(
        &self,
        key: &ApiKey,
        concrete_model_id: &str,
        registry: &ModelRegistry,
    ) -> Result<ModelInfo, ApiKeyError> {
        let snapshot = registry.get_snapshot();

        // 1. Registry lookup: model must be registered
        let model_info = snapshot
            .models
            .iter()
            .find(|m| m.id == concrete_model_id)
            .cloned()
            .ok_or_else(|| ApiKeyError::ModelNotFound(concrete_model_id.to_string()))?;

        // 2. Provider must be ready
        if snapshot.status != ProviderStatus::Ready {
            return Err(ApiKeyError::ModelNotFound(concrete_model_id.to_string()));
        }

        // 3. Trusted provider identity & connection classification:
        // Ollama is currently the verified local provider connected to localhost.
        let is_trusted_local_provider = model_info.provider_id == "ollama";

        // 4. Evaluate access policy
        match &key.access_policy {
            AccessPolicy::LocalOnly => {
                if !is_trusted_local_provider {
                    return Err(ApiKeyError::UnauthorizedModel(format!(
                        "Model '{}' is from non-local provider '{}' and not permitted under local_only policy",
                        concrete_model_id, model_info.provider_id
                    )));
                }
            }
            AccessPolicy::SelectedModels { models } => {
                if !models.contains(&concrete_model_id.to_string()) {
                    return Err(ApiKeyError::UnauthorizedModel(format!(
                        "Model '{}' is not in the allowed selected_models list",
                        concrete_model_id
                    )));
                }
            }
        }

        Ok(model_info)
    }

    /// Resolves `auto` deterministically to a concrete model ID eligible under the key's policy.
    pub fn resolve_auto_model(
        &self,
        key: &ApiKey,
        registry: &ModelRegistry,
    ) -> Result<ModelInfo, ApiKeyError> {
        let snapshot = registry.get_snapshot();
        if snapshot.status != ProviderStatus::Ready {
            return Err(ApiKeyError::NoEligibleModels);
        }

        let mut eligible_models: Vec<ModelInfo> = snapshot
            .models
            .into_iter()
            .filter(|m| {
                // Must support chat (not explicitly embedding-only)
                if m.capabilities.chat == Some(false) {
                    return false;
                }
                // Check policy
                match &key.access_policy {
                    AccessPolicy::LocalOnly => m.provider_id == "ollama",
                    AccessPolicy::SelectedModels { models } => models.contains(&m.id),
                }
            })
            .collect();

        if eligible_models.is_empty() {
            return Err(ApiKeyError::NoEligibleModels);
        }

        // Deterministic ordering by model ID ascending
        eligible_models.sort_by(|a, b| a.id.cmp(&b.id));

        Ok(eligible_models.remove(0))
    }
}
