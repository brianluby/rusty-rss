mod cli;
mod config;
mod db;
mod fetch;
mod models;
mod parse;
mod sync;

use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    cli::run(cli).await
}
