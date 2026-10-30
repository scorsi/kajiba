//! `kajiba`, the forge's command-line client (0007).
use clap::{Parser, Subcommand};
use std::{
    io::{BufRead, IsTerminal, Write},
    path::PathBuf,
    process::ExitCode,
};

mod client;
mod login;

#[derive(Parser)]
#[command(
    name = "kajiba",
    version,
    about = "Command-line client of the kajiba forge"
)]
struct Args {
    /// The personal access token file [default: $XDG_CONFIG_HOME/kajiba/token].
    #[arg(long, global = true, value_name = "PATH")]
    token_file: Option<PathBuf>,
    /// The forge URL [default: $KAJIBA_URL, then the URL saved by `kajiba login`].
    #[arg(long, global = true)]
    url: Option<String>,
    /// Print protojson instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Read a personal access token from stdin, check it with the forge, and save it
    /// with the forge URL.
    Login,
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
        Command::Login => login::run(&token_file, args.url.as_deref(), env, input, tty, out).await,
    }
}
