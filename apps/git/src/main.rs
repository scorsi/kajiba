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

#[derive(Debug, Subcommand)]
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
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Validate configuration without contacting either daemon.
    Check { path: PathBuf },
}

// Exit codes: 0 success (including help/version), 1 runtime error, 2 usage error.
fn main() -> ExitCode {
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
        Command::Serve { .. } => {
            if let Err(err) = log::init(&config.log.level) {
                log::error("fatal", &err);
                return ExitCode::FAILURE;
            }
            // shortcut: no daemon runtime yet; J3 task 9 adds the lock, socket and readiness.
            log::error("fatal", "git-side daemon is not implemented yet");
            ExitCode::FAILURE
        }
    }
}
