use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use clap::Parser;
use muzanci_cli::Cli;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() {
    let command = Cli::parse();
    if let Err(e) = command.run().await {
        eprintln!("error: {e:#}");
    }
}
