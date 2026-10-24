use clap::Parser;

/// kajiba-cli — a thin client for the kajiba forge. Empty shell until J4.
#[derive(Parser)]
#[command(name = "kajiba", version)]
struct Args {}

fn main() {
    Args::parse();
}
