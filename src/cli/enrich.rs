//! LLM enrichment command.

use anyhow::Result;
use rusty_rss_core::db;
use rusty_rss_core::enrich::{self, EnrichOptions};
use rusty_rss_core::llm::{OpenAiConfig, OpenAiProvider};
use std::path::PathBuf;

pub(super) async fn run_enrich(db_path: PathBuf, limit: usize, dry_run: bool) -> Result<()> {
    let conn = db::init_db(&db_path)?;

    if dry_run {
        let selected_count = db::list_enrichment_candidates(&conn, limit)?.len();
        println!("Would enrich {} posts", selected_count);
        return Ok(());
    }

    let provider = OpenAiProvider::new(OpenAiConfig::from_env()?);
    provider.preflight().await?;

    let summary = enrich::run_enrichment_batch(
        &conn,
        &provider,
        "openai-compatible",
        provider.model(),
        EnrichOptions::new(limit, dry_run),
    )
    .await?;

    println!(
        "Enrichment complete: {} selected, {} enriched, {} failed",
        summary.selected_count, summary.enriched_count, summary.failed_count
    );
    for failure in summary.failures {
        eprintln!("  ERROR [{}]: {}", failure.reddit_fullname, failure.error);
    }

    Ok(())
}
