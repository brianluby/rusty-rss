//! `enrichment_runs` writes/reads and triage views.

use crate::models::{
    Classification, EnrichmentOutput, EnrichmentRecord, RecommendedAction, SavedPost, TriageItem,
};
use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use std::time::Duration;

use super::posts::saved_post_from_row;

/// Select posts that need enrichment, resumably and idempotently.
///
/// A post is a candidate when it has no *current* successful enrichment run,
/// where "current" means a `status = 'success'` row whose `prompt_version`
/// matches `prompt_version` and (when `stale_after` is set) whose `created_at`
/// is newer than `now - stale_after`. This single `NOT EXISTS` predicate covers
/// all three cases at once:
///
/// - **unenriched** — the post has no successful run at all;
/// - **prompt changed** — every successful run used an older `prompt_version`,
///   so the stored output predates the current rubric and is re-enriched;
/// - **stale** — the newest successful run is older than the freshness window.
///
/// Because the predicate keys off persisted rows, re-running the batch never
/// reselects a post that was just enriched under the current prompt within the
/// window, so the selection is naturally resumable and idempotent.
pub fn list_enrichment_candidates(
    conn: &Connection,
    limit: usize,
    prompt_version: &str,
    stale_after: Option<Duration>,
) -> Result<Vec<SavedPost>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    // Both timestamps are written as UTC RFC 3339 (`Utc::now().to_rfc3339()`),
    // so a lexicographic `>=` over the stored strings is a correct chronological
    // comparison. A `None` window binds SQL NULL and disables the age check.
    let cutoff = stale_after.map(|window| {
        let secs = i64::try_from(window.as_secs()).unwrap_or(i64::MAX);
        (Utc::now() - chrono::Duration::seconds(secs)).to_rfc3339()
    });

    let mut stmt = conn.prepare(
        "SELECT reddit_fullname, reddit_id, title, author, subreddit, permalink,
                outbound_url, content_markdown, thumbnail_url, published_at, updated_at,
                first_seen_at, last_seen_at, source
         FROM saved_posts p
         WHERE NOT EXISTS (
             SELECT 1 FROM enrichment_runs e
             WHERE e.reddit_fullname = p.reddit_fullname
               AND e.status = 'success'
               AND e.prompt_version = ?1
               AND (?2 IS NULL OR e.created_at >= ?2)
         )
         ORDER BY last_seen_at DESC
         LIMIT ?3",
    )?;

    let rows = stmt
        .query_map(params![prompt_version, cutoff, limit], saved_post_from_row)
        .context("failed to query enrichment candidates")?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect enrichment candidates")
}

/// Record a successful enrichment run and return its new row id.
///
/// Validates `output` before persisting (rejecting out-of-range scores etc.) and
/// stores both the raw model response and the normalized fields. The post may
/// already have prior runs; each call inserts a new row.
pub fn record_enrichment_success(
    conn: &Connection,
    reddit_fullname: &str,
    provider: &str,
    model: &str,
    prompt_version: &str,
    raw_response: &str,
    output: &EnrichmentOutput,
) -> Result<i64> {
    output
        .validate()
        .map_err(|err| anyhow::anyhow!("invalid enrichment output: {err}"))?;

    let tags_json = serde_json::to_string(&output.tags).context("failed to serialize tags")?;
    conn.execute(
        r#"INSERT INTO enrichment_runs (
            reddit_fullname, provider, model, prompt_version, status, raw_response,
            classification, tags_json, summary, joy_value, work_value,
            recommended_action, rationale, confidence, created_at
        ) VALUES (?, ?, ?, ?, 'success', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        params![
            reddit_fullname,
            provider,
            model,
            prompt_version,
            raw_response,
            output.classification.as_str(),
            tags_json,
            output.summary,
            output.joy_value,
            output.work_value,
            output.recommended_action.as_str(),
            output.rationale,
            output.confidence,
            Utc::now().to_rfc3339(),
        ],
    )
    .context("failed to record enrichment success")?;

    Ok(conn.last_insert_rowid())
}

/// Record a failed enrichment attempt and return its new row id.
///
/// Persists the error message so failures are auditable and the post can be
/// reselected on a later run.
pub fn record_enrichment_failure(
    conn: &Connection,
    reddit_fullname: &str,
    provider: &str,
    model: &str,
    prompt_version: &str,
    error: &str,
) -> Result<i64> {
    conn.execute(
        r#"INSERT INTO enrichment_runs (
            reddit_fullname, provider, model, prompt_version, status, error, created_at
        ) VALUES (?, ?, ?, ?, 'error', ?, ?)"#,
        params![
            reddit_fullname,
            provider,
            model,
            prompt_version,
            error,
            Utc::now().to_rfc3339(),
        ],
    )
    .context("failed to record enrichment failure")?;

    Ok(conn.last_insert_rowid())
}

/// Fetch the most recent enrichment run for a post (success or failure), or
/// `None` if it has never been enriched.
pub fn latest_enrichment(
    conn: &Connection,
    reddit_fullname: &str,
) -> Result<Option<EnrichmentRecord>> {
    conn.query_row(
        "SELECT id, reddit_fullname, provider, model, prompt_version, status,
                raw_response, classification, tags_json, summary, joy_value, work_value,
                recommended_action, rationale, confidence, error, created_at
         FROM enrichment_runs
         WHERE reddit_fullname = ?
         ORDER BY id DESC
         LIMIT 1",
        params![reddit_fullname],
        enrichment_record_from_row,
    )
    .optional()
    .context("failed to query latest enrichment")
}

/// A predefined filter over the latest enrichment of each post, used to drive
/// triage listings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriageView {
    /// Every post, regardless of enrichment state.
    All,
    /// Posts with no successful enrichment run yet.
    Unprocessed,
    /// Posts whose latest run scored high on joy or work value.
    HighValue,
    /// Posts whose recommended action is "should test".
    ShouldTest,
    /// Posts whose recommended action is "should build".
    ShouldBuild,
    /// Posts whose recommended action is "reading queue".
    ReadingQueue,
    /// Posts whose recommended action is "reference only".
    ReferenceOnly,
    /// Posts whose recommended action is "discard".
    Discard,
}

impl TriageView {
    /// Parse a triage view from its string name, accepting both hyphen and
    /// underscore spellings. Returns `None` for an unknown view.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "all" => Some(Self::All),
            "unprocessed" => Some(Self::Unprocessed),
            "high-value" | "high_value" => Some(Self::HighValue),
            "should-test" | "should_test" => Some(Self::ShouldTest),
            "should-build" | "should_build" => Some(Self::ShouldBuild),
            "reading-queue" | "reading_queue" => Some(Self::ReadingQueue),
            "reference-only" | "reference_only" | "reference" => Some(Self::ReferenceOnly),
            "discard" => Some(Self::Discard),
            _ => None,
        }
    }
}

/// List posts (joined with their latest enrichment) matching a [`TriageView`].
///
/// Results are ordered most-recently-seen first and paginated by `limit`/
/// `offset`. Returns an empty vector when `limit` is 0.
pub fn list_triage_items(
    conn: &Connection,
    view: TriageView,
    limit: usize,
    offset: usize,
) -> Result<Vec<TriageItem>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let where_clause = match view {
        TriageView::All => "1 = 1",
        TriageView::Unprocessed => {
            "NOT EXISTS (
                SELECT 1 FROM enrichment_runs success
                WHERE success.reddit_fullname = p.reddit_fullname AND success.status = 'success'
            )"
        }
        TriageView::HighValue => {
            "e.status = 'success' AND (e.joy_value >= 0.7 OR e.work_value >= 0.7)"
        }
        TriageView::ShouldTest => "e.status = 'success' AND e.recommended_action = 'should_test'",
        TriageView::ShouldBuild => "e.status = 'success' AND e.recommended_action = 'should_build'",
        TriageView::ReadingQueue => {
            "e.status = 'success' AND e.recommended_action = 'reading_queue'"
        }
        TriageView::ReferenceOnly => {
            "e.status = 'success' AND e.recommended_action = 'reference_only'"
        }
        TriageView::Discard => "e.status = 'success' AND e.recommended_action = 'discard'",
    };
    let query = format!(
        "SELECT p.reddit_fullname, p.title, p.subreddit, p.author, p.permalink, p.outbound_url,
                e.id, e.reddit_fullname, e.provider, e.model, e.prompt_version, e.status,
                e.raw_response, e.classification, e.tags_json, e.summary, e.joy_value, e.work_value,
                e.recommended_action, e.rationale, e.confidence, e.error, e.created_at
         FROM saved_posts p
         LEFT JOIN enrichment_runs e ON e.id = (
             SELECT id FROM enrichment_runs latest
             WHERE latest.reddit_fullname = p.reddit_fullname
             ORDER BY latest.id DESC
             LIMIT 1
         )
         WHERE {where_clause}
         ORDER BY p.last_seen_at DESC, p.reddit_fullname DESC
         LIMIT ? OFFSET ?"
    );
    let mut stmt = conn.prepare(&query)?;
    let rows = stmt
        .query_map(params![limit, offset], triage_item_from_row)
        .context("failed to query triage items")?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect triage items")
}

fn enrichment_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EnrichmentRecord> {
    let status: String = row.get(5)?;
    let output = if status == "success" {
        Some(EnrichmentOutput {
            classification: parse_classification(row, 7)?,
            tags: parse_tags(row, 8)?,
            summary: row.get(9)?,
            joy_value: row.get(10)?,
            work_value: row.get(11)?,
            recommended_action: parse_recommended_action(row, 12)?,
            rationale: row.get(13)?,
            confidence: row.get(14)?,
        })
    } else {
        None
    };

    Ok(EnrichmentRecord {
        id: row.get(0)?,
        reddit_fullname: row.get(1)?,
        provider: row.get(2)?,
        model: row.get(3)?,
        prompt_version: row.get(4)?,
        status,
        raw_response: row.get(6)?,
        output,
        error: row.get(15)?,
        created_at: row.get(16)?,
    })
}

fn triage_item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TriageItem> {
    let enrichment = if row.get::<_, Option<i64>>(6)?.is_some() {
        Some(enrichment_record_from_row_with_offset(row, 6)?)
    } else {
        None
    };

    Ok(TriageItem {
        reddit_fullname: row.get(0)?,
        title: row.get(1)?,
        subreddit: row.get(2)?,
        author: row.get(3)?,
        permalink: row.get(4)?,
        outbound_url: row.get(5)?,
        enrichment,
    })
}

pub(super) fn enrichment_record_from_row_with_offset(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<EnrichmentRecord> {
    let status: String = row.get(offset + 5)?;
    let output = if status == "success" {
        Some(EnrichmentOutput {
            classification: parse_classification(row, offset + 7)?,
            tags: parse_tags(row, offset + 8)?,
            summary: row.get(offset + 9)?,
            joy_value: row.get(offset + 10)?,
            work_value: row.get(offset + 11)?,
            recommended_action: parse_recommended_action(row, offset + 12)?,
            rationale: row.get(offset + 13)?,
            confidence: row.get(offset + 14)?,
        })
    } else {
        None
    };

    Ok(EnrichmentRecord {
        id: row.get(offset)?,
        reddit_fullname: row.get(offset + 1)?,
        provider: row.get(offset + 2)?,
        model: row.get(offset + 3)?,
        prompt_version: row.get(offset + 4)?,
        status,
        raw_response: row.get(offset + 6)?,
        output,
        error: row.get(offset + 15)?,
        created_at: row.get(offset + 16)?,
    })
}

/// Parse a stored classification, surfacing an unrecognized value as a
/// row-conversion error rather than silently coercing it to a default.
fn parse_classification(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<Classification> {
    row.get::<_, String>(idx)?.parse().map_err(|err: String| {
        rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, err.into())
    })
}

/// Parse a stored recommended action, surfacing an unrecognized value as a
/// row-conversion error rather than silently coercing it to a default.
fn parse_recommended_action(
    row: &rusqlite::Row<'_>,
    idx: usize,
) -> rusqlite::Result<RecommendedAction> {
    row.get::<_, String>(idx)?.parse().map_err(|err: String| {
        rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, err.into())
    })
}

/// Parse the stored tags JSON array. A NULL column yields an empty list, but
/// malformed JSON surfaces as a row-conversion error instead of being dropped.
fn parse_tags(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<Vec<String>> {
    match row.get::<_, Option<String>>(idx)? {
        Some(tags) => serde_json::from_str(&tags).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                idx,
                rusqlite::types::Type::Text,
                Box::new(err),
            )
        }),
        None => Ok(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{test_db, test_output, test_post};
    use crate::db::upsert_post;

    #[test]
    fn enrichment_runs_keep_raw_output_and_latest_normalized_fields() {
        let conn = test_db();
        let post = test_post();
        upsert_post(&conn, &post).expect("post should insert");

        let first = record_enrichment_success(
            &conn,
            "t3_test123",
            "provider",
            "model-a",
            "prompt-v1",
            "raw one",
            &test_output(RecommendedAction::ReadingQueue, "first"),
        )
        .expect("first enrichment should insert");
        let second = record_enrichment_success(
            &conn,
            "t3_test123",
            "provider",
            "model-b",
            "prompt-v1",
            "raw two",
            &test_output(RecommendedAction::ShouldBuild, "second"),
        )
        .expect("second enrichment should insert");

        assert_ne!(first, second);

        let latest = latest_enrichment(&conn, "t3_test123")
            .expect("latest should query")
            .expect("latest should exist");
        assert_eq!(latest.id, second);
        assert_eq!(latest.raw_response, Some("raw two".to_string()));
        assert_eq!(latest.model, "model-b");
        assert_eq!(
            latest
                .output
                .expect("normalized output should exist")
                .recommended_action,
            RecommendedAction::ShouldBuild
        );
    }

    #[test]
    fn triage_views_filter_latest_enrichment() {
        let conn = test_db();
        for fullname in ["t3_build", "t3_test", "t3_read", "t3_ref", "t3_discard"] {
            let mut post = test_post();
            post.reddit_fullname = fullname.to_string();
            post.reddit_id = fullname.trim_start_matches("t3_").to_string();
            upsert_post(&conn, &post).expect("post should insert");
        }

        for (fullname, action, work_value) in [
            ("t3_build", RecommendedAction::ShouldBuild, 0.9),
            ("t3_test", RecommendedAction::ShouldTest, 0.8),
            ("t3_read", RecommendedAction::ReadingQueue, 0.4),
            ("t3_ref", RecommendedAction::ReferenceOnly, 0.71),
            ("t3_discard", RecommendedAction::Discard, 0.1),
        ] {
            let mut output = test_output(action, fullname);
            output.work_value = work_value;
            record_enrichment_success(
                &conn, fullname, "provider", "model", "prompt", "raw", &output,
            )
            .expect("enrichment should insert");
        }

        let build = list_triage_items(&conn, TriageView::ShouldBuild, 10, 0)
            .expect("build view should query");
        assert_eq!(build[0].reddit_fullname, "t3_build");

        let high_value = list_triage_items(&conn, TriageView::HighValue, 10, 0)
            .expect("high value view should query");
        assert_eq!(high_value.len(), 3);

        let discard = list_triage_items(&conn, TriageView::Discard, 10, 0)
            .expect("discard view should query");
        assert_eq!(discard[0].reddit_fullname, "t3_discard");
    }

    #[test]
    fn candidate_selection_covers_unenriched_prompt_change_and_staleness() {
        let conn = test_db();
        for fullname in ["t3_fresh", "t3_oldprompt", "t3_never"] {
            let mut post = test_post();
            post.reddit_fullname = fullname.to_string();
            post.reddit_id = fullname.trim_start_matches("t3_").to_string();
            upsert_post(&conn, &post).expect("post should insert");
        }

        // A current, successful run for t3_fresh under prompt-v2.
        record_enrichment_success(
            &conn,
            "t3_fresh",
            "provider",
            "model",
            "prompt-v2",
            "raw",
            &test_output(RecommendedAction::ReadingQueue, "fresh"),
        )
        .expect("fresh enrichment should insert");
        // A successful run for t3_oldprompt, but under the older prompt-v1.
        record_enrichment_success(
            &conn,
            "t3_oldprompt",
            "provider",
            "model",
            "prompt-v1",
            "raw",
            &test_output(RecommendedAction::ReadingQueue, "old prompt"),
        )
        .expect("old-prompt enrichment should insert");
        // t3_never has no enrichment row at all.

        // Under prompt-v2 with no staleness window, only the post enriched under
        // the current prompt is skipped; the old-prompt and never-enriched posts
        // are selected.
        let candidates = list_enrichment_candidates(&conn, 10, "prompt-v2", None)
            .expect("candidates should query");
        let names: std::collections::HashSet<_> = candidates
            .iter()
            .map(|p| p.reddit_fullname.clone())
            .collect();
        assert!(
            !names.contains("t3_fresh"),
            "current prompt should be skipped"
        );
        assert!(
            names.contains("t3_oldprompt"),
            "prompt change should re-select"
        );
        assert!(
            names.contains("t3_never"),
            "never-enriched should be selected"
        );

        // Re-running with the same prompt must not reselect a just-enriched post:
        // selection is idempotent/resumable.
        record_enrichment_success(
            &conn,
            "t3_never",
            "provider",
            "model",
            "prompt-v2",
            "raw",
            &test_output(RecommendedAction::ReadingQueue, "now enriched"),
        )
        .expect("enrichment should insert");
        let after = list_enrichment_candidates(&conn, 10, "prompt-v2", None)
            .expect("candidates should query");
        let after_names: std::collections::HashSet<_> =
            after.iter().map(|p| p.reddit_fullname.clone()).collect();
        assert!(
            !after_names.contains("t3_never"),
            "freshly enriched post must not reselect"
        );

        // A zero staleness window treats every existing run as stale, so even the
        // current-prompt post becomes a candidate again.
        let stale = list_enrichment_candidates(&conn, 10, "prompt-v2", Some(Duration::ZERO))
            .expect("candidates should query");
        let stale_names: std::collections::HashSet<_> =
            stale.iter().map(|p| p.reddit_fullname.clone()).collect();
        assert!(
            stale_names.contains("t3_fresh"),
            "zero window makes current runs stale"
        );
    }

    #[test]
    fn zero_limits_return_no_enrichment_or_triage_rows() {
        let conn = test_db();
        let post = test_post();
        upsert_post(&conn, &post).expect("post should insert");

        let candidates = list_enrichment_candidates(&conn, 0, "prompt-v1", None)
            .expect("candidates should query");
        assert!(candidates.is_empty());

        let items = list_triage_items(&conn, TriageView::All, 0, 0).expect("triage should query");
        assert!(items.is_empty());
    }

    #[test]
    fn record_enrichment_success_rejects_invalid_output() {
        let conn = test_db();
        let post = test_post();
        upsert_post(&conn, &post).expect("post should insert");
        let mut output = test_output(RecommendedAction::ReferenceOnly, "invalid");
        output.confidence = 2.0;

        let err = record_enrichment_success(
            &conn,
            "t3_test123",
            "provider",
            "model",
            "prompt",
            "raw",
            &output,
        )
        .expect_err("invalid output should not persist");

        assert!(err.to_string().contains("invalid enrichment output"));
        assert!(
            latest_enrichment(&conn, "t3_test123")
                .expect("latest should query")
                .is_none()
        );
    }

    #[test]
    fn malformed_enrichment_row_surfaces_as_error() {
        let conn = test_db();
        let post = test_post();
        upsert_post(&conn, &post).expect("post should insert");
        record_enrichment_success(
            &conn,
            "t3_test123",
            "provider",
            "model",
            "prompt",
            "raw",
            &test_output(RecommendedAction::ShouldBuild, "summary"),
        )
        .expect("enrichment should insert");

        conn.execute(
            "UPDATE enrichment_runs SET recommended_action = 'bogus_action' WHERE reddit_fullname = ?",
            params!["t3_test123"],
        )
        .expect("corrupting the action should succeed");

        let err =
            latest_enrichment(&conn, "t3_test123").expect_err("malformed action row should fail");
        assert!(
            err.to_string()
                .contains("failed to query latest enrichment")
        );
    }
}
