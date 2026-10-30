use clap::{Parser, Subcommand};
use kajiba_core::{config::ServerConfig, log};
use std::{path::PathBuf, process::ExitCode};

mod db;
mod import;
mod metrics;
mod state;
mod transport;

// Exit codes: 0 success (including help/version), 1 runtime error, 2 usage error (clap).

/// kajiba-server — the forge's HTTP/gRPC server and host-local commands.
#[derive(Debug, Parser)]
#[command(version)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum Command {
    /// Start the HTTP/gRPC server.
    Serve {
        /// Configuration file.
        #[arg(long)]
        config: PathBuf,
    },
    /// Inspect configuration offline.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Administer the running server over its local socket.
    Admin {
        #[command(subcommand)]
        command: AdminCommand,
    },
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum ConfigCommand {
    /// Validate configuration without contacting the server.
    Check {
        #[arg(long)]
        files: bool,
        path: PathBuf,
    },
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum AdminCommand {
    /// Revoke a user's passkeys, sessions and runtime PATs, then print an enrollment link.
    Enroll { user: String },
    /// Import the branches and tags of a path, bundle or URL into a declared, empty repository.
    Import {
        repo: String,
        /// An absolute path, or a URL without credentials (no userinfo, query or fragment);
        /// visible to every local user like any command argument.
        #[arg(required_unless_present = "source_file")]
        source: Option<String>,
        /// File holding the complete source instead of the argument; not readable by
        /// others. A secret in the source's path still reaches git's transport helper
        /// as an argument (ADR 0049): prefer a credential-free source with
        /// --credentials-file.
        #[arg(long, conflicts_with = "source")]
        source_file: Option<PathBuf>,
        /// File holding `user:token` or a token for an http(s) source; not readable by others.
        #[arg(long)]
        credentials_file: Option<PathBuf>,
    },
    /// Administer repositories (not implemented yet).
    Repo {
        #[command(subcommand)]
        command: RepoCommand,
    },
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum RepoCommand {
    /// Map repository IDs (directories under repos/) to names; archived ones are marked.
    List,
}

#[tokio::main]
async fn main() -> ExitCode {
    log::install_panic_hook();
    let command = match Args::try_parse() {
        Ok(args) => args.command,
        Err(err) if err.use_stderr() => {
            log::error("fatal", "invalid command line; use --help");
            return ExitCode::from(2);
        }
        Err(err) => {
            print!("{err}");
            return ExitCode::SUCCESS;
        }
    };
    match command {
        Command::Serve { config } => {
            let config = match load_config(&config, true) {
                Ok(config) => config,
                Err(err) => {
                    log::error("config-invalid", &err);
                    return ExitCode::FAILURE;
                }
            };
            if let Err(err) = log::init(&config.log.level) {
                log::error("fatal", &err);
                return ExitCode::FAILURE;
            }
            tracing::info!(event = "start", version = env!("CARGO_PKG_VERSION"));
            let _state_lock = match state::prepare(&config.state_dir) {
                Ok(lock) => lock,
                Err(err) => {
                    log::error("fatal", &err);
                    return ExitCode::FAILURE;
                }
            };
            let health = transport::Health::new(
                config.state_dir.clone(),
                config.git.state_dir.clone(),
                config.ssh.probe,
            );
            let (mut writer, readers) =
                match db::open(&config.state_dir.join("db/kajiba.sqlite")).await {
                    Ok(database) => database,
                    Err(err) => {
                        log::error("fatal", &err);
                        return ExitCode::FAILURE;
                    }
                };
            let mut git = transport::git_side::Repositories::new(&config.git.socket);
            if let Err(err) = state::cleanup(
                &config.state_dir,
                &config.git.state_dir,
                &mut git,
                &mut writer,
            )
            .await
            {
                log::error("fatal", &err);
                return ExitCode::FAILURE;
            }
            if let Err(err) = db::reconcile_declared(&mut writer, &config, &mut git).await {
                log::error("fatal", &err);
                return ExitCode::FAILURE;
            }
            let repos_root = config.git.state_dir.join("repos");
            let store = kajiba_gitstore::Gix::shared(&repos_root);
            let bus = std::sync::Arc::new(db::events::EventBus::default());
            let gate = std::sync::Arc::new(db::pushes::Gate::default());
            if let Ok(mut client) = git.client().await.cloned() {
                if let Err(err) =
                    transport::git_side::snapshot(&mut client, &gate, &mut writer, &git.generation)
                        .await
                {
                    tracing::warn!(event = "git-side-unavailable", err = %err);
                }
            }
            if let Err(err) = gate
                .reconcile(&mut writer, &bus, &config.git.state_dir, &store)
                .await
            {
                log::error("fatal", &err);
                return ExitCode::FAILURE;
            }
            // Request handlers that write (enrollment, push hooks) take a clone of this handle.
            let writer = db::Writer::new(writer.into());
            // CI and mirror register here when their handlers are implemented.
            let mut dispatcher = db::events::Dispatcher::default();
            let admin = transport::local::Admin {
                git_socket: config.git.socket.clone(),
                writer: writer.clone(),
                public_url: config.http.public_url.clone(),
                enrollment_ttl: config.auth.enrollment_ttl,
                gate: gate.clone(),
                bus: bus.clone(),
                state_dir: config.git.state_dir.clone(),
            };
            health.started(readers.clone(), writer.clone());
            return match tokio::select! {
                result = transport::serve(&config.http, &config.metrics, &config.git, health, readers, bus.clone(), admin) => result,
                () = dispatcher.run(&bus, &writer) => unreachable!("dispatcher runs until shutdown"),
                () = db::sessions::purge_periodically(&writer) => unreachable!("purge runs until shutdown"),
                () = gate.recover_periodically(&db::events::SystemClock, &writer, &bus, &config.git.state_dir, &store) => unreachable!("recovery runs until shutdown"),
            } {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    log::error("fatal", &err.to_string());
                    ExitCode::FAILURE
                }
            };
        }
        Command::Config {
            command: ConfigCommand::Check { files, path },
        } => {
            return match load_config(&path, files) {
                Ok(_) => ExitCode::SUCCESS,
                Err(err) => {
                    log::error("config-invalid", &err);
                    ExitCode::FAILURE
                }
            };
        }
        Command::Admin { command } => match command {
            AdminCommand::Enroll { user } => {
                return admin_enroll(std::path::Path::new(transport::local::SOCKET_PATH), user)
                    .await;
            }
            AdminCommand::Import {
                repo,
                source,
                source_file,
                credentials_file,
            } => {
                return admin_import(
                    std::path::Path::new(transport::local::SOCKET_PATH),
                    repo,
                    source,
                    source_file,
                    credentials_file,
                )
                .await;
            }
            AdminCommand::Repo {
                command: RepoCommand::List,
            } => {
                return admin_repo_list(std::path::Path::new(transport::local::SOCKET_PATH)).await;
            }
        },
    }
}

async fn admin_enroll(path: &std::path::Path, user: String) -> ExitCode {
    match transport::local::enroll(path, user).await {
        Ok(url) => {
            println!("{url}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            // A refusal's message names the reason, and the user only once it is a valid name.
            let message = match err.downcast_ref::<tonic::Status>() {
                Some(status) => status.message().to_owned(),
                None => err.to_string(),
            };
            log::error("fatal", &format!("admin enroll: {message}"));
            ExitCode::FAILURE
        }
    }
}

async fn admin_import(
    path: &std::path::Path,
    repo: String,
    source: Option<String>,
    source_file: Option<PathBuf>,
    credentials_file: Option<PathBuf>,
) -> ExitCode {
    // Read here, sent in the request: neither appears in a command line of the product
    // (ADR 0048); git's transport helper still receives the source (ADR 0049).
    let secret = |file: &Option<PathBuf>, flag| match file {
        Some(file) => kajiba_core::config::read_secret(file, flag),
        None => Ok(String::new()),
    };
    let (source, credentials) = match (
        secret(&source_file, "--source-file"),
        secret(&credentials_file, "--credentials-file"),
    ) {
        (Ok(from_file), Ok(credentials)) => (source.unwrap_or(from_file), credentials),
        (Err(err), _) | (_, Err(err)) => {
            log::error("fatal", &format!("admin import {repo}: {err}"));
            return ExitCode::FAILURE;
        }
    };
    // The server resolves paths from its own working directory: pass them absolute.
    let source = match std::fs::canonicalize(&source) {
        Ok(absolute) => absolute.to_string_lossy().into_owned(),
        Err(_) => source,
    };
    match transport::local::import(path, repo.clone(), source, credentials).await {
        Ok(_) => ExitCode::SUCCESS,
        Err(err) => {
            let message = match err.downcast_ref::<tonic::Status>() {
                Some(status) => status.message().to_owned(),
                None => err.to_string(),
            };
            log::error("fatal", &format!("admin import {repo}: {message}"));
            ExitCode::FAILURE
        }
    }
}

async fn admin_repo_list(path: &std::path::Path) -> ExitCode {
    match transport::local::list_repos(path).await {
        Ok(repos) => {
            for repo in repos {
                println!("{}", repo_line(&repo));
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            let message = match err.downcast_ref::<tonic::Status>() {
                Some(status) => status.message().to_owned(),
                None => err.to_string(),
            };
            log::error("fatal", &format!("admin repo list: {message}"));
            ExitCode::FAILURE
        }
    }
}

/// `<public id>\t<owner>/<name>`, then `\tarchived` for an archived repository.
fn repo_line(repo: &kajiba_proto::admin::v1::Repo) -> String {
    let archived = if repo.archived { "\tarchived" } else { "" };
    format!("{}\t{}/{}{archived}", repo.public_id, repo.owner, repo.name)
}

fn load_config(path: &std::path::Path, files: bool) -> Result<ServerConfig, String> {
    let config = ServerConfig::parse(&kajiba_core::config::read_config(path)?)?;
    if files {
        config.check_files()?;
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_serve_config_path() {
        assert_eq!(
            Args::try_parse_from(["kajiba-server", "serve", "--config", "server.toml"])
                .unwrap()
                .command,
            Command::Serve {
                config: PathBuf::from("server.toml")
            }
        );
    }

    #[test]
    fn parses_config_check_with_and_without_files() {
        for files in [false, true] {
            let mut args = vec!["kajiba-server", "config", "check"];
            if files {
                args.push("--files");
            }
            args.push("server.toml");
            assert_eq!(
                Args::try_parse_from(args).unwrap().command,
                Command::Config {
                    command: ConfigCommand::Check {
                        files,
                        path: PathBuf::from("server.toml")
                    }
                }
            );
        }
    }

    #[test]
    fn parses_admin_commands() {
        for (args, command) in [
            (
                vec!["enroll", "alice"],
                AdminCommand::Enroll {
                    user: "alice".into(),
                },
            ),
            (
                vec!["import", "alice/demo", "https://example.org/demo.git"],
                AdminCommand::Import {
                    repo: "alice/demo".into(),
                    source: Some("https://example.org/demo.git".into()),
                    source_file: None,
                    credentials_file: None,
                },
            ),
            (
                vec![
                    "import",
                    "alice/demo",
                    "https://example.org/demo.git",
                    "--credentials-file",
                    "/run/secrets/import",
                ],
                AdminCommand::Import {
                    repo: "alice/demo".into(),
                    source: Some("https://example.org/demo.git".into()),
                    source_file: None,
                    credentials_file: Some("/run/secrets/import".into()),
                },
            ),
            (
                vec![
                    "import",
                    "alice/demo",
                    "--source-file",
                    "/run/secrets/source",
                ],
                AdminCommand::Import {
                    repo: "alice/demo".into(),
                    source: None,
                    source_file: Some("/run/secrets/source".into()),
                    credentials_file: None,
                },
            ),
            (
                vec!["repo", "list"],
                AdminCommand::Repo {
                    command: RepoCommand::List,
                },
            ),
        ] {
            assert_eq!(
                Args::try_parse_from(["kajiba-server", "admin"].into_iter().chain(args))
                    .unwrap()
                    .command,
                Command::Admin { command }
            );
        }
        // Exactly one of the source argument and --source-file.
        for args in [
            vec!["import", "alice/demo"],
            vec!["import", "alice/demo", "/srv/a.git", "--source-file", "/s"],
        ] {
            assert!(
                Args::try_parse_from(["kajiba-server", "admin"].into_iter().chain(args)).is_err()
            );
        }
    }

    #[test]
    fn repo_lines_map_ids_to_names_and_mark_archived() {
        let mut repo = kajiba_proto::admin::v1::Repo {
            public_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
            owner: "alice".into(),
            name: "demo".into(),
            archived: false,
        };
        assert_eq!(repo_line(&repo), "01ARZ3NDEKTSV4RRFFQ69G5FAV\talice/demo");
        repo.archived = true;
        assert_eq!(
            repo_line(&repo),
            "01ARZ3NDEKTSV4RRFFQ69G5FAV\talice/demo\tarchived"
        );
    }

    #[test]
    fn ssh_helpers_are_unknown_subcommands() {
        for command in ["ssh-keys", "hook", "serve-ssh"] {
            let error = Args::try_parse_from(["kajiba-server", command]).unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::InvalidSubcommand);
        }
    }
}
