//! Triage-view command over the latest enrichment data.

use anyhow::{Result, anyhow};
use rusty_rss_core::db::{self, TriageView};
use std::path::PathBuf;

pub(super) fn run_triage(
    db_path: PathBuf,
    view: &str,
    limit: usize,
    offset: usize,
    json: bool,
) -> Result<()> {
    let view = TriageView::parse(view).ok_or_else(|| anyhow!("unknown triage view: {view}"))?;
    let conn = db::init_db(&db_path)?;
    let items = db::list_triage_items(&conn, view, limit, offset)?;

    if json {
        for item in items {
            println!("{}", serde_json::to_string(&item)?);
        }
        return Ok(());
    }

    if items.is_empty() {
        println!("No matching items found.");
        return Ok(());
    }

    for (index, item) in items.iter().enumerate() {
        let sub = item.subreddit.as_deref().unwrap_or("(no subreddit)");
        let action = item
            .enrichment
            .as_ref()
            .and_then(|record| record.output.as_ref())
            .map(|output| output.recommended_action.as_str())
            .unwrap_or("unprocessed");
        println!(
            "  {}. [{}] {} in r/{} ({})",
            index + 1 + offset,
            item.reddit_fullname,
            item.title,
            sub,
            action
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::test_support::{insert_enriched_post, test_db_path};

    #[test]
    fn triage_json_returns_enriched_rows() {
        let db_path = test_db_path();
        insert_enriched_post(&db_path);

        run_triage(db_path, "reference-only", 10, 0, true).expect("triage should succeed");
    }
}
