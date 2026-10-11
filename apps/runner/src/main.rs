use clap::Parser;

/// kajiba-runner — connects to a kajiba-server and executes jobs. Empty shell until J6.
#[derive(Parser)]
#[command(version)]
struct Args {}

fn main() {
    Args::parse();
}
