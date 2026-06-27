//! Agent-ready export records (saved post + latest enrichment + capture).

use crate::models::{Classification, ExportRecord, RecommendedAction};
use anyhow::{Context, Result};
use rusqlite::{Connection, params};

use super::captures::outbound_capture_from_row_with_offset;
use super::enrichment::enrichment_record_from_row_with_offset;
use super::posts::saved_post_from_row;

#[derive(Debug, Clone, Default)]
pub struct ExportFilters {
    pub classification: Option<Classification>,
    pub recommended_action: Option<RecommendedAction>,
    pub min_joy_value: Option<f32>,
    pub min_work_value: Option<f32>,
}

pub fn list_export_records(
    conn: &Connection,
    filters: &ExportFilters,
    limit: usize,
    offset: usize,
) -> Result<Vec<ExportRecord>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let classification = filters.classification.map(|value| value.as_str());
    let action = filters.recommended_action.map(|value| value.as_str());
    let mut stmt = conn.prepare(
        "SELECT p.reddit_fullname, p.reddit_id, p.title, p.author, p.subreddit,
                p.permalink, p.outbound_url, p.content_markdown, p.thumbnail_url,
                p.published_at, p.updated_at, p.first_seen_at, p.last_seen_at, p.source,
                e.id, e.reddit_fullname, e.provider, e.model, e.prompt_version, e.status,
                e.raw_response, e.classification, e.tags_json, e.summary, e.joy_value,
                e.work_value, e.recommended_action, e.rationale, e.confidence, e.error,
                e.created_at,
                c.reddit_fullname, c.original_url, c.final_url, c.canonical_url, c.title,
                c.description, c.site_name, c.preview_image_url, c.content_markdown,
                c.content_hash, c.status, c.http_status, c.error, c.fetched_at,
                c.attempt_count
         FROM saved_posts p
         LEFT JOIN enrichment_runs e ON e.id = (
             SELECT id FROM enrichment_runs latest
             WHERE latest.reddit_fullname = p.reddit_fullname
             ORDER BY latest.id DESC
             LIMIT 1
         )
         LEFT JOIN outbound_captures c ON c.reddit_fullname = p.reddit_fullname
         WHERE (? IS NULL OR e.classification = ?)
           AND (? IS NULL OR e.recommended_action = ?)
           AND (? IS NULL OR e.joy_value >= ?)
           AND (? IS NULL OR e.work_value >= ?)
         ORDER BY p.last_seen_at DESC, p.reddit_fullname DESC
         LIMIT ? OFFSET ?",
    )?;

    let rows = stmt
        .query_map(
            params![
                classification,
                classification,
                action,
                action,
                filters.min_joy_value,
                filters.min_joy_value,
                filters.min_work_value,
                filters.min_work_value,
                limit,
                offset,
            ],
            export_record_from_row,
        )
        .context("failed to query export records")?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect export records")
}

fn export_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExportRecord> {
    let latest_enrichment = if row.get::<_, Option<i64>>(14)?.is_some() {
        Some(enrichment_record_from_row_with_offset(row, 14)?)
    } else {
        None
    };
    let outbound_capture = if row.get::<_, Option<String>>(31)?.is_some() {
        Some(outbound_capture_from_row_with_offset(row, 31)?)
    } else {
        None
    };

    Ok(ExportRecord {
        schema_version: "rusty-rss.export.v1".to_string(),
        saved_post: saved_post_from_row(row)?,
        latest_enrichment,
        outbound_capture,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{test_db, test_output, test_post};
    use crate::db::{
        OutboundCaptureUpsert, record_enrichment_success, upsert_outbound_capture, upsert_post,
    };

    #[test]
    fn export_records_include_latest_enrichment_and_capture() {
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
            &test_output(RecommendedAction::ShouldBuild, "export summary"),
        )
        .expect("enrichment should insert");
        upsert_outbound_capture(
            &conn,
            &OutboundCaptureUpsert {
                reddit_fullname: "t3_test123".to_string(),
                original_url: "https://example.com/original".to_string(),
                final_url: Some("https://example.com/final".to_string()),
                canonical_url: Some("https://example.com/canonical".to_string()),
                title: Some("Captured title".to_string()),
                description: Some("Captured description".to_string()),
                site_name: Some("Example".to_string()),
                preview_image_url: Some("https://example.com/image.png".to_string()),
                content_markdown: Some("Captured snapshot".to_string()),
                content_hash: Some("sha256:test".to_string()),
                status: "success".to_string(),
                http_status: Some(200),
                error: None,
            },
        )
        .expect("capture should insert");

        let records = list_export_records(
            &conn,
            &ExportFilters {
                recommended_action: Some(RecommendedAction::ShouldBuild),
                ..ExportFilters::default()
            },
            10,
            0,
        )
        .expect("export should query");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].schema_version, "rusty-rss.export.v1");
        assert_eq!(records[0].saved_post.reddit_fullname, "t3_test123");
        assert_eq!(
            records[0]
                .latest_enrichment
                .as_ref()
                .and_then(|record| record.output.as_ref())
                .map(|output| output.recommended_action),
            Some(RecommendedAction::ShouldBuild)
        );
        assert_eq!(
            records[0]
                .outbound_capture
                .as_ref()
                .and_then(|capture| capture.title.as_deref()),
            Some("Captured title")
        );
    }

    #[test]
    fn export_pagination_is_deterministic_on_tied_timestamps() {
        let conn = test_db();
        for fullname in ["t3_aaa", "t3_bbb"] {
            let mut post = test_post();
            post.reddit_fullname = fullname.to_string();
            post.reddit_id = fullname.trim_start_matches("t3_").to_string();
            upsert_post(&conn, &post).expect("post should insert");
        }
        // Force identical last_seen_at so page order depends on the tiebreaker.
        conn.execute(
            "UPDATE saved_posts SET last_seen_at = '2026-01-01T00:00:00Z'",
            [],
        )
        .expect("tying timestamps should succeed");

        let page = |offset| {
            list_export_records(&conn, &ExportFilters::default(), 1, offset)
                .expect("export should query")
        };
        let first = page(0);
        let second = page(1);

        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        // Tiebreaker is reddit_fullname DESC, so t3_bbb precedes t3_aaa.
        assert_eq!(first[0].saved_post.reddit_fullname, "t3_bbb");
        assert_eq!(second[0].saved_post.reddit_fullname, "t3_aaa");
    }
}
