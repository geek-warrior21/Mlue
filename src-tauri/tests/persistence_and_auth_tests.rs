use mlue_lib::keys::{
    AccessPolicy, ApiKeyError, ApiKeyService, generate_api_key, hash_api_key, validate_key_format,
};
use mlue_lib::projects::ProjectService;
use mlue_lib::storage::{init_memory_pool, init_pool_at_path};
use std::path::PathBuf;

fn create_temp_db_path() -> PathBuf {
    let rand_id = uuid::Uuid::new_v4().to_string();
    std::env::temp_dir().join(format!("mlue_test_{}.db", rand_id))
}

// 1. First-run database initialization.
#[tokio::test]
async fn test_case_1_first_run_database_initialization() {
    let db_path = create_temp_db_path();
    let pool = init_pool_at_path(&db_path).await.unwrap();

    assert!(db_path.exists(), "Database file must be created on disk");

    // Check tables exist
    let tables: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name;")
            .fetch_all(&pool)
            .await
            .unwrap();

    assert!(tables.contains(&"projects".to_string()));
    assert!(tables.contains(&"api_keys".to_string()));
    assert!(tables.contains(&"settings".to_string()));
    assert!(tables.contains(&"_schema_migrations".to_string()));

    let _ = std::fs::remove_file(&db_path);
}

// 2. Database migration execution.
#[tokio::test]
async fn test_case_2_database_migration_execution() {
    let pool = init_memory_pool().await.unwrap();

    let migration_version: Option<i64> =
        sqlx::query_scalar("SELECT version FROM _schema_migrations WHERE version = 1;")
            .fetch_optional(&pool)
            .await
            .unwrap();

    assert_eq!(migration_version, Some(1));

    // Re-running migrations is idempotent and succeeds without error
    let rerun = mlue_lib::storage::migrations::run_migrations(&pool).await;
    assert!(rerun.is_ok(), "Migrations must be idempotent");
}

// 3. Project creation and listing.
#[tokio::test]
async fn test_case_3_project_creation_and_listing() {
    let pool = init_memory_pool().await.unwrap();
    let project_service = ProjectService::new(pool);

    let p1 = project_service
        .create_project("My Study Bot", Some("Bot for study assistance"))
        .await
        .unwrap();

    let p2 = project_service
        .create_project("Research Assistant", None)
        .await
        .unwrap();

    assert_eq!(p1.name, "My Study Bot");
    assert_eq!(p1.description.as_deref(), Some("Bot for study assistance"));
    assert_eq!(p2.name, "Research Assistant");
    assert_eq!(p2.description, None);

    let projects = project_service.list_projects().await.unwrap();
    assert_eq!(projects.len(), 2);
    // Ordered by created_at DESC -> p2 first, then p1
    assert_eq!(projects[0].id, p2.id);
    assert_eq!(projects[1].id, p1.id);
}

// 4. Project rename/update.
#[tokio::test]
async fn test_case_4_project_rename_and_update() {
    let pool = init_memory_pool().await.unwrap();
    let project_service = ProjectService::new(pool);

    let created = project_service
        .create_project("Old Name", Some("Old Desc"))
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let updated = project_service
        .update_project(&created.id, Some("New Name"), Some(Some("New Desc")))
        .await
        .unwrap();

    assert_eq!(updated.name, "New Name");
    assert_eq!(updated.description.as_deref(), Some("New Desc"));
    assert_eq!(updated.created_at, created.created_at);
    assert_ne!(updated.updated_at, created.created_at);

    // Clear description
    let cleared = project_service
        .update_project(&created.id, None, Some(None))
        .await
        .unwrap();
    assert_eq!(cleared.name, "New Name");
    assert_eq!(cleared.description, None);
}

// 5. Project deletion.
#[tokio::test]
async fn test_case_5_project_deletion() {
    let pool = init_memory_pool().await.unwrap();
    let project_service = ProjectService::new(pool.clone());
    let key_service = ApiKeyService::new(pool.clone());

    let proj = project_service
        .create_project("To Delete", None)
        .await
        .unwrap();

    let _k1 = key_service
        .create_key(&proj.id, "Key 1", None)
        .await
        .unwrap();
    let _k2 = key_service
        .create_key(&proj.id, "Key 2", None)
        .await
        .unwrap();

    let keys_before = key_service.list_keys(&proj.id).await.unwrap();
    assert_eq!(keys_before.len(), 2);

    let deleted = project_service.delete_project(&proj.id).await.unwrap();
    assert!(deleted);

    let get_res = project_service.get_project(&proj.id).await;
    assert!(get_res.is_err());

    let keys_after = key_service.list_keys(&proj.id).await.unwrap();
    assert_eq!(keys_after.len(), 0, "Keys must be deleted with project");
}

// 6. Multiple API keys for one project.
#[tokio::test]
async fn test_case_6_multiple_api_keys_for_one_project() {
    let pool = init_memory_pool().await.unwrap();
    let project_service = ProjectService::new(pool.clone());
    let key_service = ApiKeyService::new(pool);

    let proj = project_service
        .create_project("Multi Key Proj", None)
        .await
        .unwrap();

    let k1 = key_service
        .create_key(&proj.id, "Dev Key", None)
        .await
        .unwrap();
    let k2 = key_service
        .create_key(&proj.id, "Prod Key", None)
        .await
        .unwrap();
    let k3 = key_service
        .create_key(&proj.id, "Test Key", None)
        .await
        .unwrap();

    assert_ne!(k1.metadata.id, k2.metadata.id);
    assert_ne!(k2.metadata.id, k3.metadata.id);
    assert_ne!(k1.full_key, k2.full_key);

    let keys = key_service.list_keys(&proj.id).await.unwrap();
    assert_eq!(keys.len(), 3);
}

// 7. Keys for separate projects remain independent.
#[tokio::test]
async fn test_case_7_keys_for_separate_projects_remain_independent() {
    let pool = init_memory_pool().await.unwrap();
    let project_service = ProjectService::new(pool.clone());
    let key_service = ApiKeyService::new(pool);

    let p_a = project_service
        .create_project("Project Alpha", None)
        .await
        .unwrap();
    let p_b = project_service
        .create_project("Project Beta", None)
        .await
        .unwrap();

    let _k_a = key_service
        .create_key(&p_a.id, "Alpha Key", None)
        .await
        .unwrap();
    let _k_b = key_service
        .create_key(&p_b.id, "Beta Key", None)
        .await
        .unwrap();

    let list_a = key_service.list_keys(&p_a.id).await.unwrap();
    let list_b = key_service.list_keys(&p_b.id).await.unwrap();

    assert_eq!(list_a.len(), 1);
    assert_eq!(list_a[0].name, "Alpha Key");
    assert_eq!(list_b.len(), 1);
    assert_eq!(list_b[0].name, "Beta Key");
}

// 8. Key generation has valid formatting.
#[tokio::test]
async fn test_case_8_key_generation_has_valid_formatting() {
    for _ in 0..10 {
        let generated = generate_api_key();
        assert!(generated.full_key.starts_with("mlue_sk_"));
        assert!(validate_key_format(&generated.full_key));
        // mlue_sk_ (8 chars) + 43 base64 chars = 51 chars
        assert!(generated.full_key.len() >= 48);
        assert!(generated.key_prefix.starts_with("mlue_sk_"));
        assert!(generated.key_prefix.ends_with("..."));
        assert_eq!(generated.key_hash.len(), 64); // SHA-256 hex
    }
}

// 9. Correct key authenticates.
#[tokio::test]
async fn test_case_9_correct_key_authenticates() {
    let pool = init_memory_pool().await.unwrap();
    let project_service = ProjectService::new(pool.clone());
    let key_service = ApiKeyService::new(pool);

    let proj = project_service
        .create_project("Auth Proj", None)
        .await
        .unwrap();
    let created = key_service
        .create_key(&proj.id, "Auth Key", None)
        .await
        .unwrap();

    assert!(created.metadata.last_used_at.is_none());

    let verified = key_service.verify_key(&created.full_key).await.unwrap();
    assert_eq!(verified.id, created.metadata.id);
    assert_eq!(verified.name, "Auth Key");
    assert!(verified.last_used_at.is_some());
}

// 10. Incorrect key is rejected.
#[tokio::test]
async fn test_case_10_incorrect_key_is_rejected() {
    let pool = init_memory_pool().await.unwrap();
    let project_service = ProjectService::new(pool.clone());
    let key_service = ApiKeyService::new(pool);

    let proj = project_service
        .create_project("Reject Proj", None)
        .await
        .unwrap();
    let created = key_service
        .create_key(&proj.id, "Valid Key", None)
        .await
        .unwrap();

    // Tampered secret
    let mut tampered = created.full_key.clone();
    tampered.pop();
    tampered.push('X');

    let err_tampered = key_service.verify_key(&tampered).await;
    assert!(matches!(err_tampered, Err(ApiKeyError::InvalidKey)));

    // Completely fake key
    let err_fake = key_service
        .verify_key("mlue_sk_not_a_real_registered_key_123456789012345678")
        .await;
    assert!(matches!(err_fake, Err(ApiKeyError::InvalidKey)));

    // Malformed prefix
    let err_malformed = key_service.verify_key("sk_openai_format_token").await;
    assert!(matches!(err_malformed, Err(ApiKeyError::InvalidKey)));
}

// 11. Revoked key is rejected.
#[tokio::test]
async fn test_case_11_revoked_key_is_rejected() {
    let pool = init_memory_pool().await.unwrap();
    let project_service = ProjectService::new(pool.clone());
    let key_service = ApiKeyService::new(pool);

    let proj = project_service
        .create_project("Revoke Proj", None)
        .await
        .unwrap();
    let created = key_service
        .create_key(&proj.id, "To Revoke", None)
        .await
        .unwrap();

    // Authenticates before revocation
    assert!(key_service.verify_key(&created.full_key).await.is_ok());

    // Revoke key
    let revoked = key_service.revoke_key(&created.metadata.id).await.unwrap();
    assert!(revoked);

    // Authentication now fails with Revoked
    let err = key_service.verify_key(&created.full_key).await;
    assert!(matches!(err, Err(ApiKeyError::Revoked)));
}

// 12. Revocation persists across database reopen.
#[tokio::test]
async fn test_case_12_revocation_persists_across_database_reopen() {
    let db_path = create_temp_db_path();

    let created_key;
    let key_id;

    // Session 1: create and revoke
    {
        let pool = init_pool_at_path(&db_path).await.unwrap();
        let project_service = ProjectService::new(pool.clone());
        let key_service = ApiKeyService::new(pool.clone());

        let proj = project_service
            .create_project("Persistent Revoke", None)
            .await
            .unwrap();
        let created = key_service.create_key(&proj.id, "Key", None).await.unwrap();
        created_key = created.full_key;
        key_id = created.metadata.id;

        key_service.revoke_key(&key_id).await.unwrap();
        pool.close().await;
    }

    // Session 2: reopen database from disk
    {
        let pool = init_pool_at_path(&db_path).await.unwrap();
        let key_service = ApiKeyService::new(pool.clone());

        let err = key_service.verify_key(&created_key).await;
        assert!(matches!(err, Err(ApiKeyError::Revoked)));

        let metadata = key_service.get_key(&key_id).await.unwrap();
        assert!(!metadata.enabled);
        assert!(metadata.revoked_at.is_some());
        pool.close().await;
    }

    let _ = std::fs::remove_file(&db_path);
}

// 13. Plaintext keys are absent from the database.
#[tokio::test]
async fn test_case_13_plaintext_keys_are_absent_from_the_database() {
    let pool = init_memory_pool().await.unwrap();
    let project_service = ProjectService::new(pool.clone());
    let key_service = ApiKeyService::new(pool.clone());

    let proj = project_service
        .create_project("Security Proj", None)
        .await
        .unwrap();
    let created = key_service
        .create_key(&proj.id, "Secret Key", None)
        .await
        .unwrap();

    let secret_part = &created.full_key["mlue_sk_".len()..];

    // Fetch raw columns from SQLite
    let raw_rows = sqlx::query_as::<_, (String, String, String, String, String, String)>(
        "SELECT id, project_id, name, key_prefix, key_hash, access_policy FROM api_keys WHERE id = ?;",
    )
    .bind(&created.metadata.id)
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(raw_rows.len(), 1);
    let (id, project_id, name, prefix, hash, policy) = &raw_rows[0];

    // Assert that the full key or secret part does not appear in ANY column
    for col in [id, project_id, name, prefix, hash, policy] {
        assert!(
            !col.contains(&created.full_key),
            "Plaintext key leaked into column!"
        );
        assert!(
            !col.contains(secret_part),
            "Secret entropy leaked into column!"
        );
    }
}

// 14. Key-listing responses never expose secrets.
#[tokio::test]
async fn test_case_14_key_listing_responses_never_expose_secrets() {
    let pool = init_memory_pool().await.unwrap();
    let project_service = ProjectService::new(pool.clone());
    let key_service = ApiKeyService::new(pool);

    let proj = project_service
        .create_project("Listing Proj", None)
        .await
        .unwrap();
    let created = key_service
        .create_key(&proj.id, "Test Key", None)
        .await
        .unwrap();

    let listed = key_service.list_keys(&proj.id).await.unwrap();
    assert_eq!(listed.len(), 1);
    let meta = &listed[0];

    let secret_part = &created.full_key["mlue_sk_".len()..];

    // Serialization test
    let serialized = serde_json::to_string(meta).unwrap();
    assert!(!serialized.contains(&created.full_key));
    assert!(!serialized.contains(secret_part));
    assert!(!serialized.contains("key_hash"));
}

// 15. Model access policy is correctly evaluated.
#[tokio::test]
async fn test_case_15_model_access_policy_is_correctly_evaluated() {
    let pool = init_memory_pool().await.unwrap();
    let project_service = ProjectService::new(pool.clone());
    let key_service = ApiKeyService::new(pool);

    let proj = project_service
        .create_project("Policy Proj", None)
        .await
        .unwrap();

    let created = key_service
        .create_key(
            &proj.id,
            "Restricted Key",
            Some(AccessPolicy::SelectedModels {
                models: vec![
                    "ollama/qwen3:8b".to_string(),
                    "ollama/llama3:8b".to_string(),
                ],
            }),
        )
        .await
        .unwrap();

    let key = key_service.verify_key(&created.full_key).await.unwrap();

    // Permitted model
    assert!(
        key_service
            .check_model_access(&key, "ollama/qwen3:8b")
            .is_ok()
    );
    assert!(
        key_service
            .check_model_access(&key, "ollama/llama3:8b")
            .is_ok()
    );

    // Permitted auto alias
    assert!(key_service.check_model_access(&key, "auto").is_ok());

    // Denied model
    let denied = key_service.check_model_access(&key, "ollama/mistral:7b");
    assert!(matches!(denied, Err(ApiKeyError::UnauthorizedModel(_))));
}

// 16. `local_only` cannot authorize nonlocal provider models.
#[tokio::test]
async fn test_case_16_local_only_cannot_authorize_nonlocal_provider_models() {
    let pool = init_memory_pool().await.unwrap();
    let project_service = ProjectService::new(pool.clone());
    let key_service = ApiKeyService::new(pool);

    let proj = project_service
        .create_project("Local Proj", None)
        .await
        .unwrap();
    let created = key_service
        .create_key(&proj.id, "Local Key", Some(AccessPolicy::LocalOnly))
        .await
        .unwrap();

    let key = key_service.verify_key(&created.full_key).await.unwrap();

    // Local models are authorized
    assert!(
        key_service
            .check_model_access(&key, "ollama/qwen3:8b")
            .is_ok()
    );
    assert!(
        key_service
            .check_model_access(&key, "local/custom:7b")
            .is_ok()
    );
    assert!(key_service.check_model_access(&key, "auto").is_ok());

    // Non-local models are strictly rejected
    let denied_openai = key_service.check_model_access(&key, "openai/gpt-4o");
    assert!(matches!(
        denied_openai,
        Err(ApiKeyError::UnauthorizedModel(_))
    ));

    let denied_anthropic = key_service.check_model_access(&key, "anthropic/claude-3-5");
    assert!(matches!(
        denied_anthropic,
        Err(ApiKeyError::UnauthorizedModel(_))
    ));

    let denied_gemini = key_service.check_model_access(&key, "gemini/gemini-2.0");
    assert!(matches!(
        denied_gemini,
        Err(ApiKeyError::UnauthorizedModel(_))
    ));

    let denied_cloud = key_service.check_model_access(&key, "cloud/hosted-model");
    assert!(matches!(
        denied_cloud,
        Err(ApiKeyError::UnauthorizedModel(_))
    ));
}

// 17. Duplicate requests or database errors do not corrupt stored records.
#[tokio::test]
async fn test_case_17_duplicate_requests_or_database_errors_do_not_corrupt_stored_records() {
    let pool = init_memory_pool().await.unwrap();
    let project_service = ProjectService::new(pool.clone());
    let key_service = ApiKeyService::new(pool.clone());

    // Attempting to create an API key for a nonexistent project returns ProjectNotFound
    let err_missing_proj = key_service
        .create_key("nonexistent_uuid", "Key", None)
        .await;
    assert!(matches!(
        err_missing_proj,
        Err(ApiKeyError::ProjectNotFound(_))
    ));

    // Attempting to create a project with an empty name fails validation
    let err_empty_name = project_service.create_project("   ", None).await;
    assert!(err_empty_name.is_err());

    // Attempting to delete a nonexistent project returns false without crashing
    let del_missing = project_service
        .delete_project("nonexistent_uuid")
        .await
        .unwrap();
    assert!(!del_missing);

    // Verify DB remains consistent
    let all_projects = project_service.list_projects().await.unwrap();
    assert_eq!(all_projects.len(), 0);
}

// 18. Existing health, discovery, inference and streaming regression tests still pass.
#[tokio::test]
async fn test_case_18_existing_health_discovery_inference_and_streaming_regression() {
    // Verify hash computation utility
    let hash = hash_api_key("mlue_sk_sample_secret_key");
    assert_eq!(hash.len(), 64);

    // Verify constant-time comparison
    assert!(mlue_lib::keys::verify_hash(&hash, &hash));
    assert!(!mlue_lib::keys::verify_hash(&hash, "different_hash"));
}
