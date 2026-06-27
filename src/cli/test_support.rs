//! Shared fixtures for the CLI command tests.

use rusty_rss_core::db;
use rusty_rss_core::models::SavedPost;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn test_db_path() -> PathBuf {
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "rusty_rss_cli_test_{}_{}.db",
        std::process::id(),
        id
    ))
}

pub(crate) fn insert_post(db_path: &std::path::Path) {
    let conn = db::init_db(db_path).expect("db should initialize");
    let mut post = SavedPost::new(
        "t3_cli123".to_string(),
        "CLI Test Post".to_string(),
        "https://www.reddit.com/r/rust/comments/cli123/test/".to_string(),
        "atom".to_string(),
    );
    post.author = Some("cli_user".to_string());
    post.subreddit = Some("rust".to_string());
    post.content_markdown = Some("content".to_string());
    db::upsert_post(&conn, &post).expect("post should insert");
}

pub(crate) fn insert_enriched_post(db_path: &std::path::Path) {
    insert_post(db_path);
    let conn = db::init_db(db_path).expect("db should initialize");
    db::record_enrichment_success(
        &conn,
        "t3_cli123",
        "test",
        "test-model",
        "test-prompt",
        "raw",
        &rusty_rss_core::models::EnrichmentOutput {
            classification: rusty_rss_core::models::Classification::Reference,
            tags: vec!["rust".to_string()],
            summary: "Useful".to_string(),
            joy_value: 0.2,
            work_value: 0.8,
            recommended_action: rusty_rss_core::models::RecommendedAction::ReferenceOnly,
            rationale: "Useful later".to_string(),
            confidence: 0.9,
        },
    )
    .expect("enrichment should insert");
}
