//! Rule-based tagging command.

use anyhow::Result;
use rusty_rss_core::db;
use rusty_rss_core::rules::RuleSet;
use rusty_rss_core::tag::{self, TagOptions};
use std::path::PathBuf;

pub(super) fn run_tag(
    db_path: PathBuf,
    rules_path: String,
    topic: Option<String>,
    limit: Option<usize>,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    let ruleset = RuleSet::load(std::path::Path::new(&rules_path))?;
    let conn = db::init_db(&db_path)?;
    let summary = tag::run_tagging_batch(
        &conn,
        &ruleset,
        &TagOptions {
            topic,
            limit,
            dry_run,
        },
    )?;

    if json {
        for tag in &summary.tags {
            println!("{}", serde_json::to_string(tag)?);
        }
        return Ok(());
    }

    println!(
        "{}: {} posts, {} topics, {} rows {}, {} passed, {} vetoed",
        if dry_run {
            "Tag dry-run"
        } else {
            "Tagging complete"
        },
        summary.selected_posts,
        summary.topics_evaluated,
        summary.rows_written,
        if dry_run {
            "would be written"
        } else {
            "written"
        },
        summary.passed_count,
        summary.vetoed_count,
    );

    Ok(())
}
