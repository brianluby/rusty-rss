//! Feed sync and outbound metadata capture commands.

use anyhow::Result;
use rusty_rss_core::capture::{self, CaptureOptions};
use rusty_rss_core::config::Config;
use rusty_rss_core::db;
use rusty_rss_core::sync;
use std::path::PathBuf;

pub(super) async fn run_sync(config: Config) -> Result<()> {
    let result = sync::run_sync(&config).await?;

    println!(
        "Sync complete: {} pages, {} fetched, {} inserted, {} updated, {} unchanged, {} errors",
        result.page_count,
        result.fetched_count,
        result.inserted_count,
        result.updated_count,
        result.unchanged_count,
        result.parse_errors.len()
    );

    for err in &result.parse_errors {
        eprintln!("  ERROR: {}", err);
    }

    Ok(())
}

pub(super) async fn run_capture(db_path: PathBuf, limit: usize) -> Result<()> {
    let conn = db::init_db(&db_path)?;
    let summary = capture::capture_outbound_metadata(&conn, CaptureOptions::new(limit)).await?;

    println!(
        "Capture complete: {} selected, {} captured, {} failed",
        summary.selected_count, summary.captured_count, summary.failed_count
    );

    Ok(())
}
