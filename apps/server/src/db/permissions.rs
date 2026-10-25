use kajiba_core::config::Role;

/// The one role check of ADR 0014 over reconciled rows (0026): a global admin and the
/// repository's owner are `admin`, other users hold their declared permission, and every
/// user may read a public repository. External users and archived repositories are capped
/// at `read`. A disabled or unknown user, or an unknown repository, has no role. Nothing
/// here writes: permissions change only through the configuration.
pub async fn role(
    executor: impl sqlx::SqliteExecutor<'_>,
    user_id: i64,
    repo_id: i64,
) -> Result<Option<Role>, sqlx::Error> {
    let row: Option<(bool, bool, Option<String>)> = sqlx::query_as(
        "SELECT user.kind = 'external', repo.archived_at IS NOT NULL,
                CASE WHEN user.is_admin OR repo.owner_id = user.id THEN 'admin'
                     ELSE coalesce(
                         (SELECT role FROM repo_permission
                          WHERE user_id = user.id AND repo_id = repo.id),
                         CASE repo.visibility WHEN 'public' THEN 'read' END)
                END
         FROM user, repo
         WHERE user.id = ?1 AND repo.id = ?2 AND user.disabled_at IS NULL",
    )
    .bind(user_id)
    .bind(repo_id)
    .fetch_optional(executor)
    .await?;
    Ok(row.and_then(|(external, archived, role)| {
        let role = match role?.as_str() {
            "admin" => Role::Admin,
            "write" => Role::Write,
            _ => Role::Read,
        };
        Some(if external || archived {
            role.min(Role::Read)
        } else {
            role
        })
    }))
}
