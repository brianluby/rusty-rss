//! LLM enrichment command.

use anyhow::Result;
use rusty_rss_core::db;
use rusty_rss_core::enrich::{self, EnrichOptions};
use rusty_rss_core::llm::{OpenAiConfig, OpenAiProvider};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub(super) async fn run_enrich(db_path: PathBuf, options: EnrichOptions) -> Result<()> {
    let conn = db::init_db(&db_path)?;
    // Resolve candidates up front to gate the work: an empty list (or limit 0)
    // must short-circuit before the provider preflight below. The batch runner
    // re-runs this selection as the authoritative source for its summary counts;
    // the extra read is negligible for a CLI and keeps the batch self-contained.
    // Using the shared selector keeps the dry-run count and the batch in sync,
    // including the prompt-version and staleness gating.
    let candidates = enrich::select_candidates(&conn, options)?;

    if options.dry_run {
        println!("Would enrich {} posts", candidates.len());
        return Ok(());
    }

    // No work: skip the provider entirely so an empty batch (or limit 0) never
    // triggers a config lookup or a network preflight to the LLM endpoint.
    if candidates.is_empty() {
        print_enrichment_summary(0, 0, 0);
        return Ok(());
    }

    let provider = Arc::new(OpenAiProvider::new(OpenAiConfig::from_env()?));
    provider.preflight().await?;
    let model = provider.model().to_string();

    let summary =
        enrich::run_enrichment_batch(&conn, provider, "openai-compatible", &model, options).await?;

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

/// Build [`EnrichOptions`] from the parsed CLI flags. `stale_after_days` of
/// `None` leaves the staleness check off (selection still re-runs on a prompt
/// version change).
pub(super) fn enrich_options(
    limit: usize,
    dry_run: bool,
    concurrency: usize,
    retry_attempts: u32,
    stale_after_days: Option<u64>,
) -> EnrichOptions {
    EnrichOptions {
        concurrency,
        retry_attempts,
        stale_after: stale_after_days.map(|days| Duration::from_secs(days * 24 * 60 * 60)),
        ..EnrichOptions::new(limit, dry_run)
    }
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
        run_enrich(db_path, enrich_options(10, false, 4, 3, None))
            .await
            .expect("no-work enrich should succeed without contacting the provider");
    }

    #[test]
    fn enrich_options_maps_flags_and_staleness_window() {
        let opts = enrich_options(7, true, 2, 5, Some(3));
        assert_eq!(opts.limit, 7);
        assert!(opts.dry_run);
        assert_eq!(opts.concurrency, 2);
        assert_eq!(opts.retry_attempts, 5);
        assert_eq!(
            opts.stale_after,
            Some(Duration::from_secs(3 * 24 * 60 * 60))
        );

        let none = enrich_options(1, false, 1, 1, None);
        assert_eq!(none.stale_after, None);
    }
}
