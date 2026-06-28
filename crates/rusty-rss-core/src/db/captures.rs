//! `outbound_captures` candidates, upserts, and reads.

use crate::models::OutboundCapture;
use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};

/// A post that has an outbound URL still pending (or due for re-)capture.
#[derive(Debug, Clone)]
pub struct OutboundCaptureCandidate {
    /// Reddit fullname of the post owning the URL.
    pub reddit_fullname: String,
    /// Outbound URL to capture.
    pub outbound_url: String,
}

/// Input record for upserting the result of an outbound capture attempt.
#[derive(Debug, Clone)]
pub struct OutboundCaptureUpsert {
    /// Reddit fullname of the post this capture belongs to (conflict key).
    pub reddit_fullname: String,
    /// Outbound URL that was requested.
    pub original_url: String,
    /// Final URL after resolution, if the fetch succeeded.
    pub final_url: Option<String>,
    /// Canonical URL declared by the page, if any.
    pub canonical_url: Option<String>,
    /// Extracted page title, if any.
    pub title: Option<String>,
    /// Extracted page description, if any.
    pub description: Option<String>,
    /// Extracted site name, if any.
    pub site_name: Option<String>,
    /// Extracted preview image URL, if any.
    pub preview_image_url: Option<String>,
    /// Markdown snapshot of the page body, if any.
    pub content_markdown: Option<String>,
    /// `sha256:`-prefixed hash of the markdown content, if any.
    pub content_hash: Option<String>,
    /// Outcome status, e.g. `"success"` or `"error"`.
    pub status: String,
    /// HTTP status code of the response, if one was received.
    pub http_status: Option<i64>,
    /// Error message when the capture failed.
    pub error: Option<String>,
}

/// List posts with an outbound URL that has no successful, up-to-date capture.
///
/// Returns up to `limit` candidates ordered by most recently seen. A post is a
/// candidate when it has never been captured, its last capture failed, or its
/// outbound URL changed since the last capture. Returns an empty vector when
/// `limit` is 0.
pub fn list_outbound_capture_candidates(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<OutboundCaptureCandidate>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let mut stmt = conn.prepare(
        "SELECT p.reddit_fullname, p.outbound_url
         FROM saved_posts p
         LEFT JOIN outbound_captures c ON c.reddit_fullname = p.reddit_fullname
         WHERE p.outbound_url IS NOT NULL
           AND (
               c.reddit_fullname IS NULL
               OR c.status != 'success'
               OR c.original_url != p.outbound_url
           )
         ORDER BY p.last_seen_at DESC
         LIMIT ?",
    )?;

    let rows = stmt
        .query_map(params![limit], |row| {
            Ok(OutboundCaptureCandidate {
                reddit_fullname: row.get(0)?,
                outbound_url: row.get(1)?,
            })
        })
        .context("failed to query outbound capture candidates")?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect outbound capture candidates")
}

/// Insert or update the capture row for a post (keyed by `reddit_fullname`).
///
/// On conflict the existing row is overwritten with the new values and its
/// `attempt_count` is incremented, recording each capture attempt.
pub fn upsert_outbound_capture(conn: &Connection, capture: &OutboundCaptureUpsert) -> Result<()> {
    conn.execute(
        r#"INSERT INTO outbound_captures (
            reddit_fullname, original_url, final_url, canonical_url, title, description,
            site_name, preview_image_url, content_markdown, content_hash, status,
            http_status, error, fetched_at, attempt_count
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1)
        ON CONFLICT(reddit_fullname) DO UPDATE SET
            original_url = excluded.original_url,
            final_url = excluded.final_url,
            canonical_url = excluded.canonical_url,
            title = excluded.title,
            description = excluded.description,
            site_name = excluded.site_name,
            preview_image_url = excluded.preview_image_url,
            content_markdown = excluded.content_markdown,
            content_hash = excluded.content_hash,
            status = excluded.status,
            http_status = excluded.http_status,
            error = excluded.error,
            fetched_at = excluded.fetched_at,
            attempt_count = outbound_captures.attempt_count + 1"#,
        params![
            capture.reddit_fullname,
            capture.original_url,
            capture.final_url,
            capture.canonical_url,
            capture.title,
            capture.description,
            capture.site_name,
            capture.preview_image_url,
            capture.content_markdown,
            capture.content_hash,
            capture.status,
            capture.http_status,
            capture.error,
            Utc::now().to_rfc3339(),
        ],
    )
    .context("failed to upsert outbound capture")?;

    Ok(())
}

/// Fetch the stored capture for a post, or `None` if it has never been captured.
pub fn latest_outbound_capture(
    conn: &Connection,
    reddit_fullname: &str,
) -> Result<Option<OutboundCapture>> {
    conn.query_row(
        "SELECT reddit_fullname, original_url, final_url, canonical_url, title,
                description, site_name, preview_image_url, content_markdown,
                content_hash, status, http_status, error, fetched_at, attempt_count
         FROM outbound_captures
         WHERE reddit_fullname = ?",
        params![reddit_fullname],
        outbound_capture_from_row,
    )
    .optional()
    .context("failed to query outbound capture")
}

fn outbound_capture_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<OutboundCapture> {
    outbound_capture_from_row_with_offset(row, 0)
}

pub(super) fn outbound_capture_from_row_with_offset(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<OutboundCapture> {
    Ok(OutboundCapture {
        reddit_fullname: row.get(offset)?,
        original_url: row.get(offset + 1)?,
        final_url: row.get(offset + 2)?,
        canonical_url: row.get(offset + 3)?,
        title: row.get(offset + 4)?,
        description: row.get(offset + 5)?,
        site_name: row.get(offset + 6)?,
        preview_image_url: row.get(offset + 7)?,
        content_markdown: row.get(offset + 8)?,
        content_hash: row.get(offset + 9)?,
        status: row.get(offset + 10)?,
        http_status: row.get(offset + 11)?,
        error: row.get(offset + 12)?,
        fetched_at: row.get(offset + 13)?,
        attempt_count: row.get(offset + 14)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{test_db, test_post};
    use crate::db::upsert_post;

    #[test]
    fn successful_outbound_capture_removes_candidate() {
        let conn = test_db();
        let mut post = test_post();
        post.outbound_url = Some("https://example.com/post".to_string());
        upsert_post(&conn, &post).expect("post should insert");

        let candidates = list_outbound_capture_candidates(&conn, 10).expect("candidates query");
        assert_eq!(candidates.len(), 1);

        upsert_outbound_capture(
            &conn,
            &OutboundCaptureUpsert {
                reddit_fullname: "t3_test123".to_string(),
                original_url: "https://example.com/post".to_string(),
                final_url: Some("https://example.com/post".to_string()),
                canonical_url: None,
                title: Some("Captured".to_string()),
                description: None,
                site_name: None,
                preview_image_url: None,
                content_markdown: None,
                content_hash: None,
                status: "success".to_string(),
                http_status: Some(200),
                error: None,
            },
        )
        .expect("capture should insert");

        let candidates = list_outbound_capture_candidates(&conn, 10).expect("candidates query");
        assert!(candidates.is_empty());
    }

    #[test]
    fn changed_outbound_url_reschedules_capture() {
        let conn = test_db();
        let mut post = test_post();
        post.outbound_url = Some("https://example.com/old".to_string());
        upsert_post(&conn, &post).expect("post should insert");
        upsert_outbound_capture(
            &conn,
            &OutboundCaptureUpsert {
                reddit_fullname: "t3_test123".to_string(),
                original_url: "https://example.com/old".to_string(),
                final_url: Some("https://example.com/old".to_string()),
                canonical_url: None,
                title: Some("Old".to_string()),
                description: None,
                site_name: None,
                preview_image_url: None,
                content_markdown: None,
                content_hash: None,
                status: "success".to_string(),
                http_status: Some(200),
                error: None,
            },
        )
        .expect("capture should insert");
        assert!(
            list_outbound_capture_candidates(&conn, 10)
                .expect("query")
                .is_empty(),
            "an unchanged URL with a success capture is not a candidate"
        );

        // The outbound URL changes, so the stale capture must be retried.
        post.outbound_url = Some("https://example.com/new".to_string());
        upsert_post(&conn, &post).expect("post should update");

        let candidates = list_outbound_capture_candidates(&conn, 10).expect("query");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].outbound_url, "https://example.com/new");
    }
}
