//! Gate 1 `post_tags` persistence and FTS match helpers.

use crate::models::PostTag;
use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{BTreeMap, HashSet};

/// A post in scope for Gate 1 tagging: just the fields the evaluator needs.
#[derive(Debug, Clone)]
pub struct TaggablePost {
    pub rowid: i64,
    pub reddit_fullname: String,
    pub subreddit: Option<String>,
}

/// List posts to tag, newest first. `limit` is an optional debug cap; the
/// default (`None`) processes the whole archive, because re-tagging everything
/// on a rule change is the point of the `tag` command.
pub fn list_taggable_posts(conn: &Connection, limit: Option<usize>) -> Result<Vec<TaggablePost>> {
    let mut sql = String::from(
        "SELECT rowid, reddit_fullname, subreddit FROM saved_posts ORDER BY last_seen_at DESC",
    );
    if limit.is_some() {
        sql.push_str(" LIMIT ?");
    }
    let mut stmt = conn.prepare(&sql)?;
    let map_row = |row: &rusqlite::Row<'_>| {
        Ok(TaggablePost {
            rowid: row.get(0)?,
            reddit_fullname: row.get(1)?,
            subreddit: row.get(2)?,
        })
    };
    let rows = match limit {
        Some(limit) => stmt.query_map(params![limit], map_row),
        None => stmt.query_map([], map_row),
    }
    .context("failed to query taggable posts")?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect taggable posts")
}

/// Run one compiled FTS5 operand and return the set of matching `saved_posts`
/// rowids. Config-malformed expressions surface as a clear error.
pub fn fts_matching_rowids(conn: &Connection, fts_expr: &str) -> Result<HashSet<i64>> {
    let mut stmt = conn
        .prepare_cached("SELECT rowid FROM posts_fts WHERE posts_fts MATCH ?1")
        .context("failed to prepare FTS match query")?;
    let ids = stmt
        .query_map(params![fts_expr], |row| row.get::<_, i64>(0))
        .map_err(|err| anyhow!("invalid match expression `{fts_expr}`: {err}"))?
        .collect::<std::result::Result<HashSet<i64>, _>>()
        .map_err(|err| anyhow!("invalid match expression `{fts_expr}`: {err}"))?;
    Ok(ids)
}

/// Smoke-test a compiled FTS5 operand without scanning rows, so a malformed
/// rule fails the whole run at load time (fail-closed) rather than mid-sweep.
pub fn validate_fts_expr(conn: &Connection, fts_expr: &str) -> Result<()> {
    let mut stmt = conn
        .prepare_cached("SELECT 1 FROM posts_fts WHERE posts_fts MATCH ?1 AND rowid = -1")
        .context("failed to prepare FTS validation query")?;
    stmt.query_row(params![fts_expr], |_| Ok(()))
        .optional()
        .map_err(|err| anyhow!("invalid match expression `{fts_expr}`: {err}"))?;
    Ok(())
}

/// Replace tags for the processed scope authoritatively: within a transaction,
/// delete the rows the run is responsible for, then insert the freshly computed
/// rows. Posts that no longer match (or topics removed from the ruleset) lose
/// their stale tags.
///
/// The delete scope is exactly what this run re-evaluated:
/// - `topic_filter = None` (all topics): the run owns every topic, so an
///   unprocessed-post row for a now-removed topic must also be cleared.
/// - `topic_filter = Some(t)`: only topic `t` is touched; other topics' tags
///   are preserved.
/// - `full_archive = true`: every post was processed, so the delete is
///   unscoped by post. Otherwise (a `--limit` debug run) only the processed
///   posts' rows are deleted, preserving tags for unprocessed posts.
pub fn replace_post_tags(
    conn: &Connection,
    topic_filter: Option<&str>,
    full_archive: bool,
    processed_fullnames: &[&str],
    tags: &[PostTag],
) -> Result<usize> {
    let tx = conn
        .unchecked_transaction()
        .context("failed to begin post_tags transaction")?;
    {
        match (topic_filter, full_archive) {
            (None, true) => {
                tx.execute("DELETE FROM post_tags", [])
                    .context("failed to clear post_tags")?;
            }
            (None, false) => {
                let mut delete = tx
                    .prepare("DELETE FROM post_tags WHERE reddit_fullname = ?1")
                    .context("failed to prepare post_tags delete")?;
                for fullname in processed_fullnames {
                    delete
                        .execute(params![fullname])
                        .context("failed to delete stale post_tags rows")?;
                }
            }
            (Some(topic), true) => {
                tx.execute("DELETE FROM post_tags WHERE topic = ?1", params![topic])
                    .context("failed to delete stale post_tags rows")?;
            }
            (Some(topic), false) => {
                let mut delete = tx
                    .prepare("DELETE FROM post_tags WHERE topic = ?1 AND reddit_fullname = ?2")
                    .context("failed to prepare post_tags delete")?;
                for fullname in processed_fullnames {
                    delete
                        .execute(params![topic, fullname])
                        .context("failed to delete stale post_tags row")?;
                }
            }
        }

        let mut insert = tx
            .prepare(
                r#"INSERT INTO post_tags (
                    reddit_fullname, topic, score, threshold, passed,
                    matched_rules, signals, ruleset_version, tagged_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
            )
            .context("failed to prepare post_tags insert")?;
        for tag in tags {
            let matched_rules = serde_json::to_string(&tag.matched_rules)
                .context("failed to serialize matched_rules")?;
            let signals =
                serde_json::to_string(&tag.signals).context("failed to serialize signals")?;
            insert
                .execute(params![
                    tag.reddit_fullname,
                    tag.topic,
                    tag.score,
                    tag.threshold,
                    tag.passed as i64,
                    matched_rules,
                    signals,
                    tag.ruleset_version,
                    tag.tagged_at,
                ])
                .context("failed to insert post_tags row")?;
        }
    }
    tx.commit().context("failed to commit post_tags")?;
    Ok(tags.len())
}

/// List materialized tags, newest-scoring first, optionally one topic and/or
/// only passing rows. Powers `tag --json` and read queries.
pub fn list_post_tags(
    conn: &Connection,
    topic: Option<&str>,
    passed_only: bool,
    limit: usize,
    offset: usize,
) -> Result<Vec<PostTag>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT reddit_fullname, topic, score, threshold, passed,
                matched_rules, signals, ruleset_version, tagged_at
         FROM post_tags
         WHERE (?1 IS NULL OR topic = ?1)
           AND (?2 = 0 OR passed = 1)
         ORDER BY topic ASC, score DESC, reddit_fullname ASC
         LIMIT ?3 OFFSET ?4",
    )?;
    let rows = stmt
        .query_map(
            params![topic, passed_only as i64, limit, offset],
            post_tag_from_row,
        )
        .context("failed to query post_tags")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect post_tags")
}

/// All tags for a single post, ordered by topic. Useful for inspection/tests.
pub fn post_tags_for(conn: &Connection, reddit_fullname: &str) -> Result<Vec<PostTag>> {
    let mut stmt = conn.prepare(
        "SELECT reddit_fullname, topic, score, threshold, passed,
                matched_rules, signals, ruleset_version, tagged_at
         FROM post_tags
         WHERE reddit_fullname = ?1
         ORDER BY topic ASC",
    )?;
    let rows = stmt
        .query_map(params![reddit_fullname], post_tag_from_row)
        .context("failed to query post_tags for post")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect post_tags for post")
}

fn post_tag_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PostTag> {
    let matched_rules_raw: String = row.get(5)?;
    let signals_raw: Option<String> = row.get(6)?;
    // Propagate corruption rather than silently returning empty provenance.
    let matched_rules: Vec<String> = serde_json::from_str(&matched_rules_raw).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(err))
    })?;
    let signals: BTreeMap<String, f32> = signals_raw
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(err))
        })?
        .unwrap_or_default();
    Ok(PostTag {
        reddit_fullname: row.get(0)?,
        topic: row.get(1)?,
        score: row.get(2)?,
        threshold: row.get(3)?,
        passed: row.get::<_, i64>(4)? != 0,
        matched_rules,
        signals,
        ruleset_version: row.get(7)?,
        tagged_at: row.get(8)?,
    })
}
