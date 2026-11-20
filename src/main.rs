use clap::Parser;
use montclair_cli::Cli;

#[tokio::main]
async fn main() {
    let command = Cli::parse();
    if let Err(e) = command.run().await {
        eprintln!("error: {e:#}");
    }
}
