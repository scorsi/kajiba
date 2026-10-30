//! `kajiba`, the forge's command-line client (0007).
use clap::Parser;
use std::{path::PathBuf, process::ExitCode};

// shortcut: unused outside tests until `login` and `repo` (J4 tasks 13-14) call it.
#[cfg_attr(not(test), allow(dead_code))]
mod client;

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
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run(Args::parse(), &|name| std::env::var_os(name)).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("kajiba: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run(_args: Args, _env: client::Env<'_>) -> Result<(), client::Error> {
    Ok(())
}
