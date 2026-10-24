use super::users::new_public_id;
use kajiba_core::config::{RepoConfig, Role};
use kajiba_git::GitStore;
use sqlx::{Connection, SqliteConnection};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

struct Row {
    id: i64,
    public_id: String,
    key: String,
    owner: String,
    name: String,
    archived: bool,
}

enum Log {
    Created(String),
    Renamed(String, String),
    Archived(String),
    Unarchived(String),
}

/// Repository changes applied in the caller's transaction: log them once it commits,
/// or remove the directories created for its rows if it does not.
#[must_use]
pub struct Applied {
    created: Vec<PathBuf>,
    logs: Vec<Log>,
}

impl Applied {
    pub fn committed(self) {
        for log in self.logs {
            match log {
                Log::Created(repo) => tracing::info!(event = "repo-created", repo),
                Log::Renamed(repo, from) => tracing::info!(event = "repo-renamed", repo, from),
                Log::Archived(repo) => tracing::warn!(
                    event = "repo-archived",
                    repo,
                    "removed from the configuration"
                ),
                Log::Unarchived(repo) => tracing::info!(event = "repo-unarchived", repo),
            }
        }
    }

    pub fn abandoned(self) {
        // Directories of rows that were never committed: remove them, they hold nothing.
        for path in self.created {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

/// ADR 0026: make `repo`, `repo_redirect`, `repo_permission` and the declared
/// `mirror` columns equal the configuration, matching rows by key only. Run after
/// user reconciliation, under the state lock; a refusal writes nothing.
pub async fn reconcile_repos(
    writer: &mut SqliteConnection,
    state_dir: &Path,
    store: &impl GitStore,
    declared: &BTreeMap<String, RepoConfig>,
) -> Result<Applied, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| format!("repository reconciliation: {err}"))?
        .as_millis() as i64;
    let mut applied = Applied {
        created: Vec::new(),
        logs: Vec::new(),
    };
    let result = apply(
        writer,
        state_dir,
        store,
        declared,
        now,
        &mut applied.created,
        &mut applied.logs,
    )
    .await;
    match result {
        Ok(()) => Ok(applied),
        Err(err) => {
            applied.abandoned();
            Err(format!("repository reconciliation: {err}"))
        }
    }
}

async fn apply(
    writer: &mut SqliteConnection,
    state_dir: &Path,
    store: &impl GitStore,
    declared: &BTreeMap<String, RepoConfig>,
    now: i64,
    created: &mut Vec<PathBuf>,
    logs: &mut Vec<Log>,
) -> Result<(), String> {
    let sql = |err: sqlx::Error| err.to_string();
    let mut tx = writer.begin().await.map_err(sql)?;
    let users: BTreeMap<String, i64> = sqlx::query_as("SELECT name, id FROM user")
        .fetch_all(&mut *tx)
        .await
        .map_err(sql)?
        .into_iter()
        .collect();
    let rows: Vec<Row> = sqlx::query_as::<_, (i64, String, String, String, String, bool)>(
        "SELECT repo.id, repo.public_id, repo.config_key, user.name, repo.name,
                repo.archived_at IS NOT NULL
         FROM repo JOIN user ON user.id = repo.owner_id",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(sql)?
    .into_iter()
    .map(|(id, public_id, key, owner, name, archived)| Row {
        id,
        public_id,
        key,
        owner,
        name,
        archived,
    })
    .collect();
    let redirects: Vec<(i64, String)> =
        sqlx::query_as("SELECT repo_id, owner_name || '/' || name FROM repo_redirect")
            .fetch_all(&mut *tx)
            .await
            .map_err(sql)?;

    // Names of rows archived after this pass stay locked forever, redirects included.
    let mut held = BTreeMap::new();
    for row in rows.iter().filter(|row| !declared.contains_key(&row.key)) {
        held.insert(format!("{}/{}", row.owner, row.name), &row.key);
        for (_, pair) in redirects.iter().filter(|(id, _)| *id == row.id) {
            held.insert(pair.clone(), &row.key);
        }
    }
    for (key, repo) in declared {
        let current = format!("{}/{}", repo.owner, repo.name);
        for name in std::iter::once(&current).chain(&repo.previous_names) {
            if let Some(holder) = held.get(name) {
                return Err(format!(
                    "repos.{key}: {name} is held by archived repository repos.{holder}"
                ));
            }
        }
    }

    let by_key: BTreeMap<&str, &Row> = rows.iter().map(|row| (row.key.as_str(), row)).collect();
    // Free the names first so that swaps between keys never collide on UNIQUE(owner_id, name).
    for (key, repo) in declared {
        if let Some(row) = by_key
            .get(key.as_str())
            .filter(|row| row.owner != repo.owner || row.name != repo.name)
        {
            sqlx::query("UPDATE repo SET name = '#' || lower(public_id) WHERE id = ?")
                .bind(row.id)
                .execute(&mut *tx)
                .await
                .map_err(sql)?;
        }
    }
    for (key, repo) in declared {
        if let Some(row) = by_key.get(key.as_str()) {
            for (_, pair) in redirects.iter().filter(|(id, _)| *id == row.id) {
                if !repo.previous_names.contains(pair) {
                    let (owner, name) = pair.split_once('/').unwrap_or_default();
                    sqlx::query(
                        "DELETE FROM repo_redirect WHERE repo_id = ? AND owner_name = ? AND name = ?",
                    )
                    .bind(row.id)
                    .bind(owner)
                    .bind(name)
                    .execute(&mut *tx)
                    .await
                    .map_err(sql)?;
                }
            }
        }
    }

    let mut new = Vec::new();
    for (key, repo) in declared {
        let owner_id = users
            .get(&repo.owner)
            .ok_or_else(|| format!("repos.{key}.owner: not a reconciled user"))?;
        let current = format!("{}/{}", repo.owner, repo.name);
        let id = if let Some(row) = by_key.get(key.as_str()) {
            sqlx::query(
                "UPDATE repo SET owner_id = ?1, name = ?2, visibility = 'public', ci_enabled = ?3,
                        default_branch = ?4, description = ?5, archived_at = NULL
                 WHERE id = ?6 AND (owner_id != ?1 OR name != ?2 OR ci_enabled != ?3
                        OR default_branch != ?4 OR description != ?5 OR archived_at IS NOT NULL)",
            )
            .bind(owner_id)
            .bind(&repo.name)
            .bind(repo.ci_enabled)
            .bind(&repo.default_branch)
            .bind(&repo.description)
            .bind(row.id)
            .execute(&mut *tx)
            .await
            .map_err(sql)?;
            // Compare the file itself, not the row's previous value: a HEAD left by an
            // interrupted or rolled-back run converges here (set_head is idempotent).
            let path = state_dir.join(format!("repos/{}.git", row.public_id));
            store
                .set_head(&path, &repo.default_branch)
                .map_err(|err| format!("repos.{key}.default_branch: {err}"))?;
            let previous = format!("{}/{}", row.owner, row.name);
            if previous != current {
                logs.push(Log::Renamed(current.clone(), previous));
            }
            if row.archived {
                logs.push(Log::Unarchived(current.clone()));
            }
            row.id
        } else {
            let public_id = new_public_id(&mut tx, now as u128).await.map_err(sql)?;
            let id = sqlx::query_scalar(
                "INSERT INTO repo (public_id, config_key, owner_id, name, visibility, ci_enabled,
                        default_branch, description, created_at)
                 VALUES (?, ?, ?, ?, 'public', ?, ?, ?, ?) RETURNING id",
            )
            .bind(&public_id)
            .bind(key)
            .bind(owner_id)
            .bind(&repo.name)
            .bind(repo.ci_enabled)
            .bind(&repo.default_branch)
            .bind(&repo.description)
            .bind(now)
            .fetch_one(&mut *tx)
            .await
            .map_err(sql)?;
            new.push((public_id, repo.default_branch.as_str()));
            logs.push(Log::Created(current.clone()));
            id
        };

        for pair in &repo.previous_names {
            let (owner, name) = pair.split_once('/').unwrap_or_default();
            sqlx::query(
                "INSERT INTO repo_redirect (owner_name, name, repo_id, created_at) VALUES (?, ?, ?, ?)
                 ON CONFLICT (owner_name, name) DO NOTHING",
            )
            .bind(owner)
            .bind(name)
            .bind(id)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(sql)?;
        }

        let roles: BTreeMap<i64, &str> =
            repo.permissions
                .iter()
                .map(|(user, role)| {
                    let role = match role {
                        Role::Read => "read",
                        Role::Write => "write",
                        Role::Admin => "admin",
                    };
                    users.get(user).map(|id| (*id, role)).ok_or_else(|| {
                        format!("repos.{key}.permissions.{user}: not a reconciled user")
                    })
                })
                .collect::<Result<_, _>>()?;
        let existing: Vec<i64> =
            sqlx::query_scalar("SELECT user_id FROM repo_permission WHERE repo_id = ?")
                .bind(id)
                .fetch_all(&mut *tx)
                .await
                .map_err(sql)?;
        for user_id in existing.iter().filter(|user| !roles.contains_key(user)) {
            sqlx::query("DELETE FROM repo_permission WHERE repo_id = ? AND user_id = ?")
                .bind(id)
                .bind(user_id)
                .execute(&mut *tx)
                .await
                .map_err(sql)?;
        }
        for (user_id, role) in roles {
            sqlx::query(
                "INSERT INTO repo_permission (user_id, repo_id, role) VALUES (?1, ?2, ?3)
                 ON CONFLICT (user_id, repo_id) DO UPDATE SET role = ?3 WHERE role != ?3",
            )
            .bind(user_id)
            .bind(id)
            .bind(role)
            .execute(&mut *tx)
            .await
            .map_err(sql)?;
        }

        // A mirror is identified by its remote URL; its runtime columns are never touched here.
        let urls: BTreeSet<&str> = repo.mirrors.iter().map(|m| m.remote_url.as_str()).collect();
        let existing: Vec<(i64, String)> =
            sqlx::query_as("SELECT id, remote_url FROM mirror WHERE repo_id = ?")
                .bind(id)
                .fetch_all(&mut *tx)
                .await
                .map_err(sql)?;
        for (mirror_id, _) in existing
            .iter()
            .filter(|(_, url)| !urls.contains(url.as_str()))
        {
            sqlx::query("DELETE FROM mirror WHERE id = ?")
                .bind(mirror_id)
                .execute(&mut *tx)
                .await
                .map_err(sql)?;
        }
        for mirror in &repo.mirrors {
            if let Some((mirror_id, _)) = existing.iter().find(|(_, url)| *url == mirror.remote_url)
            {
                sqlx::query(
                    "UPDATE mirror SET credential = ?1, enabled = ?2
                     WHERE id = ?3 AND (credential != ?1 OR enabled != ?2)",
                )
                .bind(&mirror.credential)
                .bind(mirror.enabled)
                .bind(mirror_id)
                .execute(&mut *tx)
                .await
                .map_err(sql)?;
            } else {
                let public_id = new_public_id(&mut tx, now as u128).await.map_err(sql)?;
                sqlx::query(
                    "INSERT INTO mirror (public_id, repo_id, remote_url, credential, enabled, state, redo, attempts)
                     VALUES (?, ?, ?, ?, ?, 'idle', 0, 0)",
                )
                .bind(public_id)
                .bind(id)
                .bind(&mirror.remote_url)
                .bind(&mirror.credential)
                .bind(mirror.enabled)
                .execute(&mut *tx)
                .await
                .map_err(sql)?;
            }
        }
    }

    for row in rows
        .iter()
        .filter(|row| !row.archived && !declared.contains_key(&row.key))
    {
        // Row, directory, redirects, mirror rows and status stay; a retained pending_push too.
        for statement in [
            "UPDATE repo SET archived_at = ?1 WHERE id = ?2",
            "DELETE FROM repo_permission WHERE repo_id = ?2 AND ?1 IS NOT NULL",
            "UPDATE job_attempt SET state = 'cancelled', finished_at = ?1 WHERE state = 'running'
             AND job_id IN (SELECT job.id FROM job JOIN run ON run.id = job.run_id WHERE run.repo_id = ?2)",
            "UPDATE job SET state = 'cancelled', finished_at = ?1
             WHERE state IN ('pending', 'assigned', 'running') AND run_id IN (SELECT id FROM run WHERE repo_id = ?2)",
            "UPDATE run SET state = 'cancelled', finished_at = ?1 WHERE repo_id = ?2 AND state IN ('queued', 'running')",
            "UPDATE mirror SET state = 'idle', redo = 0, next_attempt_at = NULL
             WHERE repo_id = ?2 AND ?1 IS NOT NULL AND (state != 'idle' OR redo != 0 OR next_attempt_at IS NOT NULL)",
        ] {
            sqlx::query(statement)
                .bind(now)
                .bind(row.id)
                .execute(&mut *tx)
                .await
                .map_err(sql)?;
        }
        logs.push(Log::Archived(format!("{}/{}", row.owner, row.name)));
    }

    // ADR 0026: init in tmp/, rename into repos/, then commit the rows.
    for (public_id, branch) in new {
        let name = format!("{public_id}.git");
        let tmp = state_dir.join("tmp").join(&name);
        let path = state_dir.join("repos").join(&name);
        store
            .init_bare(&tmp, branch)
            .map_err(|err| format!("{}: {err}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|err| format!("{}: {err}", path.display()))?;
        created.push(path);
        crate::state::sync_dir(&state_dir.join("repos"))
            .map_err(|err| format!("{}/repos: {err}", state_dir.display()))?;
    }
    tx.commit().await.map_err(sql)
}

/// A repository found by `<owner>/<name>` (ADR 0021, 0026).
#[derive(Debug, PartialEq, Eq)]
pub struct Found {
    pub id: i64,
    pub public_id: String,
    pub owner: String,
    pub name: String,
    pub archived: bool,
    /// Found through a previous name, not the current one.
    pub redirected: bool,
}

/// The row holding the name, active or archived, then a redirect, which may lead to
/// an archived repository. Names resolve case-insensitively; an invalid one finds nothing.
pub async fn find(
    readers: &sqlx::SqlitePool,
    owner: &str,
    name: &str,
) -> Result<Option<Found>, sqlx::Error> {
    use kajiba_core::names::validate_name;
    let (Ok(owner), Ok(name)) = (validate_name(owner), validate_name(name)) else {
        return Ok(None);
    };
    let found: Option<(i64, String, String, String, bool, bool)> = sqlx::query_as(
        "SELECT repo.id, repo.public_id, owner.name, repo.name, repo.archived_at IS NOT NULL, 0
         FROM repo JOIN user owner ON owner.id = repo.owner_id
         WHERE owner.name = ?1 AND repo.name = ?2
         UNION ALL
         SELECT repo.id, repo.public_id, owner.name, repo.name, repo.archived_at IS NOT NULL, 1
         FROM repo_redirect JOIN repo ON repo.id = repo_redirect.repo_id
         JOIN user owner ON owner.id = repo.owner_id
         WHERE repo_redirect.owner_name = ?1 AND repo_redirect.name = ?2
         ORDER BY 6 LIMIT 1",
    )
    .bind(owner)
    .bind(name)
    .fetch_optional(readers)
    .await?;
    Ok(
        found.map(|(id, public_id, owner, name, archived, redirected)| Found {
            id,
            public_id,
            owner,
            name,
            archived,
            redirected,
        }),
    )
}
