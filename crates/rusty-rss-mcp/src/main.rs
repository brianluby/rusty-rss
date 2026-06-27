//! Binary entrypoint: serve the read-only rusty-rss MCP tools over stdio.

use anyhow::{Context, Result};
use rmcp::ServiceExt;
use rmcp::transport::stdio;
use rusty_rss_mcp::config::{DbPathArgs, USAGE, ensure_db_exists, parse_db_path};
use rusty_rss_mcp::server::RustyRssServer;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let db_path = match parse_db_path(std::env::args().skip(1))? {
        DbPathArgs::HelpRequested => {
            // Diagnostics go to stderr; stdout is reserved for JSON-RPC. A help
            // request is a successful, expected invocation, so exit 0.
            eprintln!("{USAGE}");
            return Ok(());
        }
        DbPathArgs::Resolved(path) => path,
    };
    // Fail fast before binding stdio so a misconfigured path is reported on
    // stderr rather than surfacing as confusing per-call tool errors.
    ensure_db_exists(&db_path)?;

    let server = RustyRssServer::new(db_path);
    let running = server
        .serve(stdio())
        .await
        .context("failed to start MCP stdio server")?;
    running.waiting().await?;
    Ok(())
}

/// Initialize tracing to STDERR only. stdout is the JSON-RPC channel and must
/// never receive log output, or it would corrupt the protocol stream.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap_or_default();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();
}
