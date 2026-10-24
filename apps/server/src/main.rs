use axum::{Router, routing::get};
use clap::{Parser, Subcommand};
use std::{path::PathBuf, process::ExitCode};

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
        /// Configuration file (loading is not implemented yet).
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
    /// Validate configuration (not implemented yet).
    Check {
        #[arg(long)]
        files: bool,
        path: PathBuf,
    },
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum AdminCommand {
    /// Issue a passkey enrollment link (not implemented yet).
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
    let command = Args::parse().command;
    let name = match command {
        Command::Serve { config: _ } => {
            // shortcut: retain J0's listener until configuration loading lands in J1 task 7.
            return match serve().await {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("kajiba-server: {err}");
                    ExitCode::FAILURE
                }
            };
        }
        Command::Config { .. } => "config check",
        Command::Admin { command } => match command {
            AdminCommand::Enroll { .. } => "admin enroll",
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
    eprintln!("kajiba-server: {name}: not implemented yet");
    ExitCode::from(NOT_IMPLEMENTED)
}

async fn serve() -> std::io::Result<()> {
    let app = Router::new().route("/healthz", get(|| async { "OK" }));
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    axum::serve(listener, app).await
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
