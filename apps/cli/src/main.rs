//! `kajiba`, the forge's command-line client (0007).
use clap::{Parser, Subcommand};
use std::{
    io::{BufRead, IsTerminal, Write},
    path::PathBuf,
    process::ExitCode,
};

mod client;
mod login;
mod repo;

#[derive(Parser)]
#[command(
    name = "kajiba",
    version,
    about = "Command-line client of the kajiba forge"
)]
struct Args {
    /// The credential file: forge URL and personal access token
    /// [default: $XDG_CONFIG_HOME/kajiba/token].
    #[arg(long, global = true, value_name = "PATH")]
    token_file: Option<PathBuf>,
    /// The forge URL for `login` [default: $KAJIBA_URL, then the token file's]; other
    /// commands refuse one differing from the token file's.
    #[arg(long, global = true)]
    url: Option<String>,
    /// Print protojson instead of text.
    #[arg(long, global = true)]
    json: bool,
    /// Development mode: also accept a plain http forge URL on localhost, 127.0.0.1 or
    /// [::1] (otherwise only https), with a warning.
    #[arg(long, global = true)]
    development: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Read a personal access token from stdin, check it with the forge, and save it
    /// with the forge URL in the token file.
    Login,
    /// Read repositories.
    Repo {
        #[command(subcommand)]
        command: repo::Command,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let stdin = std::io::stdin();
    let tty = stdin.is_terminal();
    let env = |name: &str| std::env::var_os(name);
    match run(
        Args::parse(),
        &env,
        &mut stdin.lock(),
        tty,
        &mut std::io::stdout(),
    )
    .await
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("kajiba: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run(
    args: Args,
    env: client::Env<'_>,
    input: &mut dyn BufRead,
    tty: bool,
    out: &mut dyn Write,
) -> Result<(), client::Error> {
    let token_file = client::token_path(args.token_file.as_deref(), env)?;
    match args.command {
        Command::Login => {
            login::run(
                &token_file,
                args.url.as_deref(),
                args.development,
                env,
                input,
                tty,
                out,
            )
            .await
        }
        Command::Repo { command } => {
            let saved = client::credentials(args.url.as_deref(), &token_file, env)?;
            repo::run(
                command,
                client::connect(&saved.url, &saved.token, args.development)?,
                args.json,
                out,
            )
            .await
        }
    }
}
