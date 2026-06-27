//! Shared fixtures for the CLI command tests.

use rusty_rss_core::db;
use rusty_rss_core::models::SavedPost;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn test_db_path() -> PathBuf {
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "rusty_rss_cli_test_{}_{}.db",
        std::process::id(),
        id
    ));
    reset_db_file(&path);
    path
}

/// Remove a stale test database and its SQLite sidecars so a reused path
/// (process id + counter) never inherits data from an earlier run. Covers the
/// default rollback-journal mode (`-journal`) and WAL mode (`-wal`/`-shm`) so it
/// stays correct regardless of journal_mode. A missing file is fine; any other
/// I/O error is surfaced rather than silently ignored.
fn reset_db_file(path: &Path) {
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let mut target = path.as_os_str().to_owned();
        target.push(suffix);
        let target = PathBuf::from(target);
        match std::fs::remove_file(&target) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => panic!(
                "failed to remove stale test db file {}: {err}",
                target.display()
            ),
        }
    }
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
