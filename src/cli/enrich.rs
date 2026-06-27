//! LLM enrichment command.

use anyhow::Result;
use rusty_rss_core::db;
use rusty_rss_core::enrich::{self, EnrichOptions};
use rusty_rss_core::llm::{OpenAiConfig, OpenAiProvider};
use std::path::PathBuf;

pub(super) async fn run_enrich(db_path: PathBuf, limit: usize, dry_run: bool) -> Result<()> {
    let conn = db::init_db(&db_path)?;
    // Fetch candidates up front to gate the work: an empty list (or limit 0) must
    // short-circuit before the provider preflight below. run_enrichment_batch
    // re-runs this query as the authoritative source for its summary counts; the
    // extra read is negligible for a CLI and keeps the batch self-contained.
    let candidates = db::list_enrichment_candidates(&conn, limit)?;

    if dry_run {
        println!("Would enrich {} posts", candidates.len());
        return Ok(());
    }

    // No work: skip the provider entirely so an empty batch (or limit 0) never
    // triggers a config lookup or a network preflight to the LLM endpoint.
    if candidates.is_empty() {
        print_enrichment_summary(0, 0, 0);
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

    print_enrichment_summary(
        summary.selected_count,
        summary.enriched_count,
        summary.failed_count,
    );
    for failure in summary.failures {
        eprintln!("  ERROR [{}]: {}", failure.reddit_fullname, failure.error);
    }

    Ok(())
}

/// Single source of truth for the completion line so the empty-batch
/// short-circuit and the normal path can never drift apart.
fn print_enrichment_summary(selected: usize, enriched: usize, failed: usize) {
    println!("Enrichment complete: {selected} selected, {enriched} enriched, {failed} failed");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::test_support::test_db_path;

    #[tokio::test]
    async fn run_enrich_skips_provider_when_no_candidates() {
        // With no enrichment candidates there is no work, so run_enrich must
        // return without building or preflighting the LLM provider. Before the
        // reorder it preflighted first and failed trying to reach the default
        // local provider (http://127.0.0.1:8080), even with nothing to enrich.
        let db_path = test_db_path();
        run_enrich(db_path, 10, false)
            .await
            .expect("no-work enrich should succeed without contacting the provider");
    }
}
