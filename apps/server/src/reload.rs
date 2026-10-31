//! Configuration reload on `SIGHUP` (ADR 0141): the declared users, runners, repositories
//! and mirrors go through the startup reconciliation again, under the writer lock.
use crate::{db, metrics::Registry, transport::Health, transport::git_side::Repositories};
use kajiba_core::config::ServerConfig;
use std::{path::PathBuf, sync::Arc};

pub struct Reload {
    pub path: PathBuf,
    /// The configuration in effect: its reloadable part is the last one applied.
    pub running: ServerConfig,
    /// 1 at startup, plus one per applied reload.
    pub generation: u64,
    pub writer: db::Writer,
    pub health: Health,
    pub registry: Arc<Registry>,
}

impl Reload {
    /// Reload on each `SIGHUP`, one at a time: signals received during a reload are
    /// coalesced into one more reload.
    pub async fn run(mut self, mut hangup: tokio::signal::unix::Signal) {
        self.registry.set_config_generation(self.generation);
        while hangup.recv().await.is_some() {
            let _ = self.reload().await;
        }
        std::future::pending().await
    }

    /// Apply the file's reloadable part; on success, the keys that still need a restart.
    /// A file that does not load, or a reconciliation that fails, changes nothing.
    pub async fn reload(&mut self) -> Result<Vec<&'static str>, String> {
        let result = self.apply().await;
        match &result {
            Err(err) => tracing::error!(event = "config-reload-failed", err),
            Ok(keys) => {
                let generation = self.generation;
                tracing::info!(event = "config-reloaded", generation);
                if !keys.is_empty() {
                    let keys = keys.join(",");
                    tracing::warn!(event = "config-reload-partial", generation, keys);
                }
            }
        }
        result
    }

    async fn apply(&mut self) -> Result<Vec<&'static str>, String> {
        let next = crate::load_config(&self.path, true)?;
        let restart = self.running.restart_keys(&next);
        let mut applied = self.running.clone();
        applied.users = next.users;
        applied.runners = next.runners;
        applied.repos = next.repos;
        applied.mirror = next.mirror;
        let mut git = Repositories::new(&applied.git.socket);
        db::reconcile_declared(&mut *self.writer.lock().await, &applied, &mut git).await?;
        self.health.reloaded(applied.runners.clone());
        self.running = applied;
        self.generation += 1;
        self.registry.set_config_generation(self.generation);
        Ok(restart)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const TOKEN: &str = "9f2c1e0a4b7d3f8e6a5c2b1d0e9f8a7b6c5d4e3f2a1b0c9d8e7f6a5b4c3d2e1f";

    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn config(dir: &std::path::Path, extra: &str) -> String {
        format!(
            "[http]\npublic_url = 'https://forge.example.org'\n[git]\nstate_dir = '{0}'\nsocket = '{0}/git.sock'\n[users.alice]\n[repos.demo]\nowner = 'alice'\ndescription = ''\n{extra}",
            dir.display()
        )
    }

    async fn count(writer: &db::Writer, query: &str) -> i64 {
        sqlx::query_scalar(query)
            .fetch_one(&mut *writer.lock().await)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn reload_applies_declarations_keeps_invalid_files_and_reports_restart_keys() {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        // Short: the git side's socket path is bounded.
        let dir = Dir(std::env::temp_dir().join(format!(
            "kj-reload-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )));
        std::fs::create_dir(&dir.0).unwrap();
        let path = dir.0.join("server.toml");
        let token =
            format!("[users.alice.tokens.ci]\nsha256 = '{TOKEN}'\nscopes = ['repo:read']\n");
        std::fs::write(&path, config(&dir.0, &token)).unwrap();
        let running = crate::load_config(&path, true).unwrap();
        let mut git = crate::transport::git_side::fake::Fake::start(&dir.0).await;
        let _daemon = crate::transport::git_side::fake::Fake::serve(&dir.0, &running.git.socket);
        let (mut writer, _) = db::open(&dir.0.join("kajiba.sqlite")).await.unwrap();
        db::reconcile_declared(&mut writer, &running, &mut git.client)
            .await
            .unwrap();
        let writer = db::Writer::new(writer.into());
        let registry = Arc::new(Registry::default());
        let mut reload = Reload {
            path: path.clone(),
            running,
            generation: 1,
            writer: writer.clone(),
            health: Health::new(dir.0.clone(), dir.0.clone(), None),
            registry: registry.clone(),
        };
        let tokens = "SELECT count(*) FROM api_token WHERE source = 'config'";
        let repos = "SELECT count(*) FROM repo WHERE archived_at IS NULL";
        assert_eq!(
            (count(&writer, tokens).await, count(&writer, repos).await),
            (1, 1)
        );

        // A declared repository added and a token removed take effect without a restart.
        std::fs::write(
            &path,
            config(&dir.0, "[repos.tools]\nowner = 'alice'\ndescription = ''\n"),
        )
        .unwrap();
        assert_eq!(reload.reload().await, Ok(vec![]));
        assert_eq!(
            (count(&writer, tokens).await, count(&writer, repos).await),
            (0, 2)
        );
        let tools: String = sqlx::query_scalar("SELECT public_id FROM repo WHERE name = 'tools'")
            .fetch_one(&mut *writer.lock().await)
            .await
            .unwrap();
        assert!(dir.0.join(format!("repos/{tools}.git")).is_dir());
        assert_eq!(reload.generation, 2);
        let exposed = |line: &str| {
            registry
                .exposition("test", [0; 4], [0; 4])
                .lines()
                .any(|l| l == line)
        };
        assert!(exposed("kajiba_config_generation 2"));

        // An invalid file keeps the running configuration.
        for invalid in [
            "not = [toml".to_owned(),
            config(
                &dir.0,
                "[repos.other]\nowner = 'nobody'\ndescription = ''\n",
            ),
        ] {
            std::fs::write(&path, invalid).unwrap();
            assert!(reload.reload().await.is_err());
            assert_eq!(reload.generation, 2);
            assert_eq!(count(&writer, repos).await, 2);
        }
        assert!(exposed("kajiba_config_generation 2"));

        // A non-reloadable change is reported; the reloadable rest is applied.
        std::fs::write(
            &path,
            config(&dir.0, "[log]\nlevel = 'debug'\n").replace(
                "public_url = 'https://forge.example.org'\n",
                "public_url = 'https://forge.example.org'\nlisten = '127.0.0.1:9090'\n",
            ),
        )
        .unwrap();
        assert_eq!(reload.reload().await, Ok(vec!["http", "log"]));
        assert_eq!(count(&writer, repos).await, 1);
        assert_eq!(reload.generation, 3);
        // The running configuration keeps the values in effect.
        assert_eq!(reload.running.log.level, "info");
    }
}
