//! Concurrent orchestration: capture every outbound candidate and persist.

use super::fetch::capture_url_with_retries;
use super::options::{CaptureOptions, CaptureSummary};
use super::security::build_capture_client;
use crate::db::{self, OutboundCaptureUpsert};
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

const CAPTURE_USER_AGENT: &str = "rusty-rss/0.1";

/// Capture outbound metadata for the pending candidate posts and persist it.
///
/// Selects up to `options.limit` posts that have an outbound URL but no recent
/// capture, fetches them concurrently (bounded by `options.max_concurrency`,
/// with per-URL retries), and upserts each result -- success or error -- into
/// the database. Always builds the guarded HTTP client internally so SSRF
/// protections cannot be bypassed by the caller. Returns a [`CaptureSummary`]
/// of the run.
pub async fn capture_outbound_metadata(
    conn: &Connection,
    options: CaptureOptions,
) -> Result<CaptureSummary> {
    // Build the client here so the DNS-rebinding guard is always installed and
    // matches `options.allow_private_hosts`; callers cannot supply an unguarded
    // client.
    let client = build_capture_client(CAPTURE_USER_AGENT, options.allow_private_hosts);
    let candidates = db::list_outbound_capture_candidates(conn, options.limit)?;
    let mut summary = CaptureSummary {
        selected_count: candidates.len(),
        ..CaptureSummary::default()
    };
    let max_concurrency = options.max_concurrency.max(1);
    let max_retries = options.max_retries.max(1);
    let semaphore = Arc::new(Semaphore::new(max_concurrency));
    let mut tasks = JoinSet::new();

    for candidate in candidates {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .context("capture semaphore closed")?;
        let client = client.clone();
        tasks.spawn(async move {
            let result =
                capture_url_with_retries(&client, &candidate.outbound_url, max_retries).await;
            drop(permit);
            (candidate, result)
        });
    }

    while let Some(joined) = tasks.join_next().await {
        let (candidate, capture_result) = match joined {
            Ok(pair) => pair,
            Err(join_err) => {
                // A spawned capture task panicked or was cancelled. Count it as a
                // single failure and keep draining the rest of the batch instead
                // of aborting all remaining (and in-flight) work.
                tracing::warn!(error = %join_err, "capture task failed to join; skipping candidate");
                summary.failed_count += 1;
                continue;
            }
        };
        match capture_result {
            Ok(metadata) => {
                db::upsert_outbound_capture(
                    conn,
                    &OutboundCaptureUpsert {
                        reddit_fullname: candidate.reddit_fullname,
                        original_url: candidate.outbound_url,
                        final_url: Some(metadata.final_url),
                        canonical_url: metadata.canonical_url,
                        title: metadata.title,
                        description: metadata.description,
                        site_name: metadata.site_name,
                        preview_image_url: metadata.preview_image_url,
                        content_markdown: metadata.content_markdown,
                        content_hash: metadata.content_hash,
                        status: "success".to_string(),
                        http_status: Some(i64::from(metadata.http_status)),
                        error: None,
                    },
                )?;
                summary.captured_count += 1;
            }
            Err(err) => {
                db::upsert_outbound_capture(
                    conn,
                    &OutboundCaptureUpsert {
                        reddit_fullname: candidate.reddit_fullname,
                        original_url: candidate.outbound_url,
                        final_url: None,
                        canonical_url: None,
                        title: None,
                        description: None,
                        site_name: None,
                        preview_image_url: None,
                        content_markdown: None,
                        content_hash: None,
                        status: "error".to_string(),
                        http_status: None,
                        error: Some(err.to_string()),
                    },
                )?;
                summary.failed_count += 1;
            }
        }
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::test_support::{serve_concurrent_html, serve_html};
    use crate::models::SavedPost;
    use crate::test_support::reset_db_file;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn capture_outbound_metadata_records_success() {
        let db_path = std::env::temp_dir().join(format!(
            "rusty_rss_capture_test_{}_{}.db",
            std::process::id(),
            1
        ));
        reset_db_file(&db_path);
        let conn = db::init_db(&db_path).expect("db should init");
        let url = serve_html("<html><head><title>Captured page</title></head></html>");
        let mut post = SavedPost::new(
            "t3_capture".to_string(),
            "Capture".to_string(),
            "https://reddit.com/r/rust/comments/capture/item/".to_string(),
            "atom".to_string(),
        );
        post.outbound_url = Some(url);
        db::upsert_post(&conn, &post).expect("post should insert");

        let summary = capture_outbound_metadata(
            &conn,
            CaptureOptions {
                limit: 10,
                allow_private_hosts: true,
                ..CaptureOptions::new(10)
            },
        )
        .await
        .expect("capture should run");

        assert_eq!(summary.selected_count, 1);
        assert_eq!(summary.captured_count, 1);
        let capture = db::latest_outbound_capture(&conn, "t3_capture")
            .expect("capture should query")
            .expect("capture should exist");
        assert_eq!(capture.title.as_deref(), Some("Captured page"));
        assert!(
            capture
                .content_markdown
                .as_deref()
                .is_some_and(|content| content.contains("Captured page"))
        );
        assert!(
            capture
                .content_hash
                .as_deref()
                .is_some_and(|hash| hash.starts_with("sha256:"))
        );
    }

    #[tokio::test]
    async fn capture_outbound_metadata_respects_concurrency_limit() {
        let db_path = std::env::temp_dir().join(format!(
            "rusty_rss_capture_concurrency_test_{}_{}.db",
            std::process::id(),
            1
        ));
        reset_db_file(&db_path);
        let conn = db::init_db(&db_path).expect("db should init");
        let current = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let base_url = serve_concurrent_html(3, Arc::clone(&current), Arc::clone(&max_seen));

        for index in 0..3 {
            let mut post = SavedPost::new(
                format!("t3_capture_{index}"),
                format!("Capture {index}"),
                format!("https://reddit.com/r/rust/comments/capture/{index}/"),
                "atom".to_string(),
            );
            post.outbound_url = Some(format!("{base_url}/article-{index}"));
            db::upsert_post(&conn, &post).expect("post should insert");
        }

        let summary = capture_outbound_metadata(
            &conn,
            CaptureOptions {
                limit: 10,
                allow_private_hosts: true,
                max_concurrency: 2,
                max_retries: 1,
            },
        )
        .await
        .expect("capture should run");

        assert_eq!(summary.captured_count, 3);
        assert!(max_seen.load(Ordering::SeqCst) <= 2);
    }
}
