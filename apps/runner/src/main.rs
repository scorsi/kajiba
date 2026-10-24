use clap::Parser;
use kajiba_core::log;
use std::process::ExitCode;

/// kajiba-runner — connects to a kajiba-server and executes jobs. Empty shell until J6.
#[derive(Parser)]
#[command(version)]
struct Args {}

fn main() -> ExitCode {
    log::install_panic_hook();
    match Args::try_parse() {
        Ok(_) => match log::init("info") {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                log::error("fatal", &err);
                ExitCode::FAILURE
            }
        },
        Err(err) if err.use_stderr() => {
            log::error("fatal", "invalid command line; use --help");
            ExitCode::from(2)
        }
        Err(err) => {
            print!("{err}");
            ExitCode::SUCCESS
        }
    }
}
