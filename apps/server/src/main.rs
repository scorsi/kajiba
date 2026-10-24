use clap::{Parser, Subcommand};
use kajiba_core::{config::ServerConfig, log};
use std::{path::PathBuf, process::ExitCode};

mod db;
mod metrics;
mod state;
mod transport;

// Exit codes: 0 success (including help/version), 1 runtime error, 2 usage error
// (clap), 3 command not implemented yet.
const NOT_IMPLEMENTED: u8 = 3;

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
    /// Forward a Git hook to the running server.
    Hook {
        #[command(subcommand)]
        command: HookCommand,
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
    /// Import a repository from a path, bundle or URL (not implemented yet).
    Import { repo: String, source: String },
    /// Administer repositories (not implemented yet).
    Repo {
        #[command(subcommand)]
        command: RepoCommand,
    },
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum RepoCommand {
    /// Create a repository (not implemented yet).
    Create { repo: String },
    /// Map repository IDs to names (not implemented yet).
    List,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum HookCommand {
    /// Check ref updates before receiving a push (not implemented yet).
    PreReceive,
    /// Notify the server after receiving a push (not implemented yet).
    PostReceive,
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
    let name = match command {
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
            let health = transport::Health::new(config.state_dir.clone());
            let (mut writer, readers) =
                match db::open(&config.state_dir.join("db/kajiba.sqlite")).await {
                    Ok(database) => database,
                    Err(err) => {
                        log::error("fatal", &err);
                        return ExitCode::FAILURE;
                    }
                };
            if let Err(err) = state::cleanup(&config.state_dir, &mut writer).await {
                log::error("fatal", &err);
                return ExitCode::FAILURE;
            }
            if let Err(err) = db::reconcile(&mut writer, &config.users).await {
                log::error("fatal", &err);
                return ExitCode::FAILURE;
            }
            if let Err(err) = db::repos::reconcile_repos(
                &mut writer,
                &config.state_dir,
                &kajiba_git::Gix,
                &config.repos,
            )
            .await
            {
                log::error("fatal", &err);
                return ExitCode::FAILURE;
            }
            let bus = std::sync::Arc::new(db::events::EventBus::default());
            let gate = std::sync::Arc::new(db::pushes::Gate::default());
            if let Err(err) = gate
                .reconcile(&mut writer, &bus, &config.state_dir, &kajiba_git::Gix)
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
                writer: writer.clone(),
                public_url: config.http.public_url.clone(),
                enrollment_ttl: config.auth.enrollment_ttl,
            };
            health.started(readers.clone());
            return match tokio::select! {
                result = transport::serve(&config.http, &config.metrics, health, readers, bus.clone(), admin) => result,
                () = dispatcher.run(&bus, &writer) => unreachable!("dispatcher runs until shutdown"),
                () = gate.recover_periodically(&writer, &bus, &config.state_dir, &kajiba_git::Gix) => unreachable!("recovery runs until shutdown"),
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
            AdminCommand::Import { .. } => "admin import",
            AdminCommand::Repo { command } => match command {
                RepoCommand::Create { .. } => "admin repo create",
                RepoCommand::List => "admin repo list",
            },
        },
        Command::Hook { command } => match command {
            HookCommand::PreReceive => "hook pre-receive",
            HookCommand::PostReceive => "hook post-receive",
        },
    };
    log::error("fatal", &format!("{name}: not implemented yet"));
    ExitCode::from(NOT_IMPLEMENTED)
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
                    source: "https://example.org/demo.git".into(),
                },
            ),
            (
                vec!["repo", "create", "alice/demo"],
                AdminCommand::Repo {
                    command: RepoCommand::Create {
                        repo: "alice/demo".into(),
                    },
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
    }

    #[test]
    fn parses_hook_commands() {
        for (name, command) in [
            ("pre-receive", HookCommand::PreReceive),
            ("post-receive", HookCommand::PostReceive),
        ] {
            assert_eq!(
                Args::try_parse_from(["kajiba-server", "hook", name])
                    .unwrap()
                    .command,
                Command::Hook { command }
            );
        }
    }
}
