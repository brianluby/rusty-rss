//! Shared fixtures for the `db` submodule tests.

use crate::db::init_db;
use crate::models::{Classification, EnrichmentOutput, RecommendedAction, SavedPost};
use chrono::Utc;
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique temp-file path per call, tagged for readability in `/tmp`.
pub(crate) fn unique_db_path(tag: &str) -> PathBuf {
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("rusty_rss_{tag}_{}_{}.db", std::process::id(), id))
}

pub(crate) fn test_db() -> Connection {
    let path = unique_db_path("test");
    let _ = std::fs::remove_file(&path);
    init_db(&path).expect("init db should succeed")
}

pub(crate) fn test_post() -> SavedPost {
    let mut post = SavedPost::new(
        "t3_test123".to_string(),
        "Test Post".to_string(),
        "https://reddit.com/r/test/comments/test123/".to_string(),
        "atom".to_string(),
    );
    post.author = Some("testuser".to_string());
    post.subreddit = Some("test".to_string());
    post.published_at = Some(Utc::now());
    post.content_markdown = Some("Test markdown".to_string());
    post
}

pub(crate) fn test_output(action: RecommendedAction, summary: &str) -> EnrichmentOutput {
    EnrichmentOutput {
        classification: Classification::Reference,
        tags: vec!["rust".to_string()],
        summary: summary.to_string(),
        joy_value: 0.2,
        work_value: 0.8,
        recommended_action: action,
        rationale: "Useful later".to_string(),
        confidence: 0.9,
    }
}
