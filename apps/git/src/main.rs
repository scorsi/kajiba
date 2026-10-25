mod daemon;
mod hook;
mod local;
mod metrics;
mod repositories;
mod service;
mod ssh;
mod state;

use clap::{Parser, Subcommand};
use kajiba_core::{config::GitConfig, log};
use std::{path::PathBuf, process::ExitCode};

/// kajiba-git — the forge's repository writer and host-local Git commands.
#[derive(Debug, Parser)]
#[command(version)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum Command {
    /// Start the git-side daemon.
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
    /// sshd's AuthorizedKeysCommand: print the forced-command line of an active user's
    /// auth key, or nothing for an unknown key (ADR 0004).
    SshKeys {
        /// `SHA256:…`, as sshd's `%f` gives it.
        fingerprint: String,
    },
    /// Forward a Git hook to the running daemon.
    Hook {
        #[command(subcommand)]
        command: HookCommand,
    },
    /// Validate an SSH Git command (transport hand-off is not implemented yet).
    ServeSsh {
        #[arg(long)]
        user: String,
    },
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum ConfigCommand {
    /// Validate configuration without contacting either daemon.
    Check { path: PathBuf },
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum HookCommand {
    /// Check ref updates before receive-pack applies them.
    PreReceive,
    /// Notify the daemon that receive-pack updated refs.
    PostReceive,
}

// Exit codes: 0 success (including help/version), 1 runtime error, 2 usage error.
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
    let path = match &command {
        Command::Serve { config } => config,
        Command::Config {
            command: ConfigCommand::Check { path },
        } => path,
        Command::SshKeys { fingerprint } => {
            return ssh_keys(
                std::path::Path::new(local::SOCKET_PATH),
                fingerprint.clone(),
            )
            .await;
        }
        Command::Hook { command } => {
            let kind = match command {
                HookCommand::PreReceive => hook::Kind::PreReceive,
                HookCommand::PostReceive => hook::Kind::PostReceive,
            };
            return hook::run(
                std::path::Path::new(local::SOCKET_PATH),
                kind,
                std::io::stdin(),
                &|var| std::env::var(var).ok(),
            )
            .await;
        }
        Command::ServeSsh { user } => {
            let command = std::env::var("SSH_ORIGINAL_COMMAND").unwrap_or_default();
            let message = if !kajiba_core::names::is_public_id(user) {
                "serve-ssh: invalid user public id"
            } else if ssh::parse(&command).is_none() {
                "serve-ssh: invalid SSH command"
            } else {
                "serve-ssh: SSH transport is not implemented yet"
            };
            log::error("fatal", message);
            return ExitCode::FAILURE;
        }
    };
    let config =
        match kajiba_core::config::read_config(path).and_then(|input| GitConfig::parse(&input)) {
            Ok(config) => config,
            Err(err) => {
                log::error("config-invalid", &err);
                return ExitCode::FAILURE;
            }
        };
    match command {
        Command::Config { .. } => ExitCode::SUCCESS,
        Command::SshKeys { .. } | Command::Hook { .. } | Command::ServeSsh { .. } => unreachable!(),
        Command::Serve { .. } => {
            if let Err(err) = log::init(&config.log.level) {
                log::error("fatal", &err);
                return ExitCode::FAILURE;
            }
            match daemon::serve(config).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    log::error("fatal", &err);
                    ExitCode::FAILURE
                }
            }
        }
    }
}

async fn ssh_keys(path: &std::path::Path, fingerprint: String) -> ExitCode {
    match local::auth_key(path, fingerprint).await {
        Ok(Some((user, key))) => {
            println!("{}", authorized_key(&user, &key));
            ExitCode::SUCCESS
        }
        // sshd then refuses the key; it logs the failed authentication itself.
        Ok(None) => ExitCode::SUCCESS,
        Err(err) => {
            let message = match err.downcast_ref::<tonic::Status>() {
                Some(status) => status.message().to_owned(),
                None => err.to_string(),
            };
            log::error("fatal", &format!("ssh-keys: {message}"));
            ExitCode::FAILURE
        }
    }
}

/// ADR 0004's line: the canonical stored key (no comment, no configured options).
fn authorized_key(user_public_id: &str, public_key: &str) -> String {
    format!("command=\"kajiba-git serve-ssh --user {user_public_id}\",restrict {public_key}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_ssh_keys_and_prints_adr_0004_line() {
        assert_eq!(
            Args::try_parse_from(["kajiba-git", "ssh-keys", "SHA256:abc"])
                .unwrap()
                .command,
            Command::SshKeys {
                fingerprint: "SHA256:abc".into()
            }
        );
        assert_eq!(
            authorized_key("01ARZ3NDEKTSV4RRFFQ69G5FAV", "ssh-ed25519 AAAA"),
            "command=\"kajiba-git serve-ssh --user 01ARZ3NDEKTSV4RRFFQ69G5FAV\",restrict ssh-ed25519 AAAA"
        );
    }

    #[test]
    fn parses_hook_commands() {
        for (name, command) in [
            ("pre-receive", HookCommand::PreReceive),
            ("post-receive", HookCommand::PostReceive),
        ] {
            assert_eq!(
                Args::try_parse_from(["kajiba-git", "hook", name])
                    .unwrap()
                    .command,
                Command::Hook { command }
            );
        }
    }
}

#[cfg(test)]
mod local_tests;
