use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use rusty_rss_core::db;
use rusty_rss_core::models::{Classification, EnrichmentOutput, RecommendedAction, SavedPost};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_rusty-rss")
}

fn test_db_path() -> String {
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir()
        .join(format!(
            "rusty_rss_binary_test_{}_{}.db",
            std::process::id(),
            id
        ))
        .to_string_lossy()
        .to_string()
}

#[test]
fn binary_help_succeeds() {
    let output = Command::new(binary())
        .arg("--help")
        .output()
        .expect("binary should run");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("rusty-rss"));
    assert!(stdout.contains("sync"));
}

#[test]
fn binary_list_does_not_require_feed_url() {
    let output = Command::new(binary())
        .args(["--db-path", &test_db_path(), "list"])
        .output()
        .expect("binary should run");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("No saved posts found"));
}

#[test]
fn binary_sync_requires_feed_url() {
    let output = Command::new(binary())
        .args(["--db-path", &test_db_path(), "sync"])
        .env_remove("RUSTY_RSS_FEED_URL")
        .output()
        .expect("binary should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("feed URL is required"));
}

#[test]
fn binary_triage_json_outputs_records() {
    let db_path = test_db_path();
    let conn = db::init_db(std::path::Path::new(&db_path)).expect("db should initialize");
    let post = SavedPost::new(
        "t3_json".to_string(),
        "JSON Item".to_string(),
        "https://reddit.com/r/rust/comments/json/item/".to_string(),
        "atom".to_string(),
    );
    db::upsert_post(&conn, &post).expect("post should insert");
    db::record_enrichment_success(
        &conn,
        "t3_json",
        "test",
        "test-model",
        "test-prompt",
        "raw",
        &EnrichmentOutput {
            classification: Classification::Reference,
            tags: vec!["rust".to_string()],
            summary: "Useful".to_string(),
            joy_value: 0.2,
            work_value: 0.8,
            recommended_action: RecommendedAction::ReferenceOnly,
            rationale: "Useful later".to_string(),
            confidence: 0.9,
        },
    )
    .expect("enrichment should insert");

    let output = Command::new(binary())
        .args(["--db-path", &db_path, "triage", "reference-only", "--json"])
        .output()
        .expect("binary should run");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"reddit_fullname\":\"t3_json\""));
    assert!(stdout.contains("\"recommended_action\":\"reference_only\""));
}
