use super::types::{MAX_PROJECT_DESC_LENGTH, MAX_PROJECT_NAME_LENGTH, Project, ProjectError};
use sqlx::SqlitePool;
use uuid::Uuid;

#[derive(Clone)]
pub struct ProjectService {
    pool: SqlitePool,
}

impl ProjectService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn create_project(
        &self,
        name: &str,
        description: Option<&str>,
    ) -> Result<Project, ProjectError> {
        let trimmed_name = name.trim();
        if trimmed_name.is_empty() {
            return Err(ProjectError::InvalidInput(
                "Project name cannot be empty".to_string(),
            ));
        }
        if trimmed_name.len() > MAX_PROJECT_NAME_LENGTH {
            return Err(ProjectError::InvalidInput(format!(
                "Project name exceeds maximum length of {} characters",
                MAX_PROJECT_NAME_LENGTH
            )));
        }

        let desc_trimmed = description.map(|d| d.trim().to_string());
        if matches!(&desc_trimmed, Some(d) if d.len() > MAX_PROJECT_DESC_LENGTH) {
            return Err(ProjectError::InvalidInput(format!(
                "Project description exceeds maximum length of {} characters",
                MAX_PROJECT_DESC_LENGTH
            )));
        }

        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();

        sqlx::query(
            r#"
            INSERT INTO projects (id, name, description, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?)
            "#,
        )
        .bind(&id)
        .bind(trimmed_name)
        .bind(&desc_trimmed)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        Ok(Project {
            id,
            name: trimmed_name.to_string(),
            description: desc_trimmed,
            created_at: now.clone(),
            updated_at: now,
        })
    }

    pub async fn list_projects(&self) -> Result<Vec<Project>, ProjectError> {
        let rows = sqlx::query_as::<_, (String, String, Option<String>, String, String)>(
            r#"
            SELECT id, name, description, created_at, updated_at
            FROM projects
            ORDER BY created_at DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        let projects = rows
            .into_iter()
            .map(|(id, name, description, created_at, updated_at)| Project {
                id,
                name,
                description,
                created_at,
                updated_at,
            })
            .collect();

        Ok(projects)
    }

    pub async fn get_project(&self, id: &str) -> Result<Project, ProjectError> {
        let row = sqlx::query_as::<_, (String, String, Option<String>, String, String)>(
            r#"
            SELECT id, name, description, created_at, updated_at
            FROM projects
            WHERE id = ?
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some((id, name, description, created_at, updated_at)) => Ok(Project {
                id,
                name,
                description,
                created_at,
                updated_at,
            }),
            None => Err(ProjectError::NotFound(id.to_string())),
        }
    }

    pub async fn update_project(
        &self,
        id: &str,
        name: Option<&str>,
        description: Option<Option<&str>>,
    ) -> Result<Project, ProjectError> {
        let current = self.get_project(id).await?;

        let new_name = if let Some(n) = name {
            let trimmed = n.trim();
            if trimmed.is_empty() {
                return Err(ProjectError::InvalidInput(
                    "Project name cannot be empty".to_string(),
                ));
            }
            if trimmed.len() > MAX_PROJECT_NAME_LENGTH {
                return Err(ProjectError::InvalidInput(format!(
                    "Project name exceeds maximum length of {} characters",
                    MAX_PROJECT_NAME_LENGTH
                )));
            }
            trimmed.to_string()
        } else {
            current.name
        };

        let new_description = match description {
            Some(Some(d)) => {
                let trimmed = d.trim();
                if trimmed.len() > MAX_PROJECT_DESC_LENGTH {
                    return Err(ProjectError::InvalidInput(format!(
                        "Project description exceeds maximum length of {} characters",
                        MAX_PROJECT_DESC_LENGTH
                    )));
                }
                Some(trimmed.to_string())
            }
            Some(None) => None,
            None => current.description,
        };

        let now = chrono::Utc::now().to_rfc3339();

        sqlx::query(
            r#"
            UPDATE projects
            SET name = ?, description = ?, updated_at = ?
            WHERE id = ?
            "#,
        )
        .bind(&new_name)
        .bind(&new_description)
        .bind(&now)
        .bind(id)
        .execute(&self.pool)
        .await?;

        Ok(Project {
            id: id.to_string(),
            name: new_name,
            description: new_description,
            created_at: current.created_at,
            updated_at: now,
        })
    }

    pub async fn delete_project(&self, id: &str) -> Result<bool, ProjectError> {
        let mut tx = self.pool.begin().await?;

        // Explicitly delete owned api_keys first within the transaction,
        // which together with PRAGMA foreign_keys = ON ensures complete transactional safety.
        sqlx::query("DELETE FROM api_keys WHERE project_id = ?;")
            .bind(id)
            .execute(&mut *tx)
            .await?;

        let res = sqlx::query("DELETE FROM projects WHERE id = ?;")
            .bind(id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;

        Ok(res.rows_affected() > 0)
    }
}
