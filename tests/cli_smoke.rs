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

#[test]
fn binary_search_json_outputs_records() {
    let db_path = test_db_path();
    let conn = db::init_db(std::path::Path::new(&db_path)).expect("db should initialize");
    let mut post = SavedPost::new(
        "t3_search_json".to_string(),
        "Search JSON Item".to_string(),
        "https://reddit.com/r/rust/comments/search_json/item/".to_string(),
        "atom".to_string(),
    );
    post.subreddit = Some("rust".to_string());
    post.author = Some("searcher".to_string());
    post.content_markdown = Some("searchable markdown body".to_string());
    db::upsert_post(&conn, &post).expect("post should insert");

    let output = Command::new(binary())
        .args([
            "--db-path",
            &db_path,
            "search",
            "searchable",
            "--json",
            "--limit",
            "5",
            "--subreddit",
            "rust",
            "--author",
            "searcher",
        ])
        .output()
        .expect("binary should run");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"reddit_fullname\":\"t3_search_json\""));
    assert!(stdout.contains("<mark>searchable</mark>"));
}

#[test]
fn binary_export_jsonl_outputs_agent_records() {
    let db_path = test_db_path();
    let conn = db::init_db(std::path::Path::new(&db_path)).expect("db should initialize");
    let mut post = SavedPost::new(
        "t3_export_json".to_string(),
        "Export JSON Item".to_string(),
        "https://reddit.com/r/rust/comments/export_json/item/".to_string(),
        "atom".to_string(),
    );
    post.subreddit = Some("rust".to_string());
    db::upsert_post(&conn, &post).expect("post should insert");

    let output = Command::new(binary())
        .args(["--db-path", &db_path, "export", "--format", "jsonl"])
        .output()
        .expect("binary should run");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"schema_version\":\"rusty-rss.export.v1\""));
    assert!(stdout.contains("\"reddit_fullname\":\"t3_export_json\""));
}

#[test]
fn binary_export_formats_and_filters_work() {
    let db_path = test_db_path();
    let conn = db::init_db(std::path::Path::new(&db_path)).expect("db should initialize");
    for (fullname, title, action, work_value) in [
        (
            "t3_export_build",
            "Build Export Item",
            RecommendedAction::ShouldBuild,
            0.9,
        ),
        (
            "t3_export_read",
            "Read Export Item",
            RecommendedAction::ReadingQueue,
            0.4,
        ),
    ] {
        let post = SavedPost::new(
            fullname.to_string(),
            title.to_string(),
            format!("https://reddit.com/r/rust/comments/{fullname}/item/"),
            "atom".to_string(),
        );
        db::upsert_post(&conn, &post).expect("post should insert");
        db::record_enrichment_success(
            &conn,
            fullname,
            "test",
            "test-model",
            "test-prompt",
            "raw",
            &EnrichmentOutput {
                classification: Classification::Reference,
                tags: vec!["rust".to_string()],
                summary: format!("Summary {fullname}"),
                joy_value: 0.5,
                work_value,
                recommended_action: action,
                rationale: "Useful later".to_string(),
                confidence: 0.9,
            },
        )
        .expect("enrichment should insert");
    }

    let markdown = Command::new(binary())
        .args([
            "--db-path",
            &db_path,
            "export",
            "--format",
            "markdown",
            "--classification",
            "reference",
        ])
        .output()
        .expect("binary should run");
    assert!(markdown.status.success());
    let markdown_stdout = String::from_utf8_lossy(&markdown.stdout);
    assert!(markdown_stdout.contains("## Build Export Item"));
    assert!(markdown_stdout.contains("## Read Export Item"));

    let csv = Command::new(binary())
        .args([
            "--db-path",
            &db_path,
            "export",
            "--format",
            "csv",
            "--action",
            "should_build",
            "--min-work",
            "0.8",
        ])
        .output()
        .expect("binary should run");
    assert!(csv.status.success());
    let csv_stdout = String::from_utf8_lossy(&csv.stdout);
    assert!(csv_stdout.contains("t3_export_build"));
    assert!(!csv_stdout.contains("t3_export_read"));
}

#[test]
fn binary_capture_empty_database_succeeds() {
    let output = Command::new(binary())
        .args(["--db-path", &test_db_path(), "capture", "--limit", "5"])
        .output()
        .expect("binary should run");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Capture complete: 0 selected"));
}

fn write_rules_file(body: &str) -> String {
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "rusty_rss_rules_{}_{}.toml",
        std::process::id(),
        id
    ));
    std::fs::write(&path, body).expect("rules file should write");
    path.to_string_lossy().to_string()
}

const SMOKE_RULES: &str = r#"
[meta]
version = "rules-smoke-v1"

[topics.memory]
threshold = 3.0

[[topics.memory.rules]]
id = "title_concept"
signal = "title"
kind = "fts"
weight = 2.0
match = 'memor* OR persisten*'

[topics.memory.subreddit_prior]
opencodeCLI = 2.0
"#;

#[test]
fn binary_tag_json_outputs_passed_records() {
    let db_path = test_db_path();
    let conn = db::init_db(std::path::Path::new(&db_path)).expect("db should initialize");
    let mut post = SavedPost::new(
        "t3_tag".to_string(),
        "A persistent memory plugin".to_string(),
        "https://reddit.com/r/opencodeCLI/comments/tag/item/".to_string(),
        "atom".to_string(),
    );
    post.subreddit = Some("opencodeCLI".to_string());
    db::upsert_post(&conn, &post).expect("post should insert");
    let rules_path = write_rules_file(SMOKE_RULES);

    let output = Command::new(binary())
        .args([
            "--db-path",
            &db_path,
            "tag",
            "--rules",
            &rules_path,
            "--json",
        ])
        .output()
        .expect("binary should run");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"reddit_fullname\":\"t3_tag\""),
        "got: {stdout}"
    );
    assert!(stdout.contains("\"topic\":\"memory\""));
    assert!(stdout.contains("\"passed\":true"));
}

#[test]
fn binary_tag_dry_run_writes_nothing() {
    let db_path = test_db_path();
    let conn = db::init_db(std::path::Path::new(&db_path)).expect("db should initialize");
    let mut post = SavedPost::new(
        "t3_tag_dry".to_string(),
        "persistent memory".to_string(),
        "https://reddit.com/r/opencodeCLI/comments/tagdry/item/".to_string(),
        "atom".to_string(),
    );
    post.subreddit = Some("opencodeCLI".to_string());
    db::upsert_post(&conn, &post).expect("post should insert");
    let rules_path = write_rules_file(SMOKE_RULES);

    let output = Command::new(binary())
        .args([
            "--db-path",
            &db_path,
            "tag",
            "--rules",
            &rules_path,
            "--dry-run",
        ])
        .output()
        .expect("binary should run");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Tag dry-run"));
    assert!(
        db::post_tags_for(&conn, "t3_tag_dry")
            .expect("query")
            .is_empty(),
        "dry run must not persist tags"
    );
}

#[test]
fn binary_tag_missing_rules_file_errors() {
    let output = Command::new(binary())
        .args([
            "--db-path",
            &test_db_path(),
            "tag",
            "--rules",
            "/nonexistent/rules.toml",
        ])
        .output()
        .expect("binary should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("rules file"), "got: {stderr}");
}

#[test]
fn binary_fts_subcommand_is_hidden_but_runs() {
    let db_path = test_db_path();

    // The `fts` maintenance command is hidden: it must not appear in top-level help.
    let help = Command::new(binary())
        .arg("--help")
        .output()
        .expect("binary should run");
    assert!(help.status.success());
    let help_stdout = String::from_utf8_lossy(&help.stdout);
    assert!(
        !help_stdout.contains("fts"),
        "hidden fts command should not appear in help: {help_stdout}"
    );

    // The nested `fts check` subcommand still works and reports OK on a fresh db.
    let check = Command::new(binary())
        .args(["--db-path", &db_path, "fts", "check"])
        .output()
        .expect("binary should run");
    assert!(check.status.success());
    let check_stdout = String::from_utf8_lossy(&check.stdout);
    assert!(check_stdout.contains("OK"), "got: {check_stdout}");

    // The nested `fts rebuild` subcommand also runs cleanly.
    let rebuild = Command::new(binary())
        .args(["--db-path", &db_path, "fts", "rebuild"])
        .output()
        .expect("binary should run");
    assert!(rebuild.status.success());
    let rebuild_stdout = String::from_utf8_lossy(&rebuild.stdout);
    assert!(rebuild_stdout.contains("rebuilt"), "got: {rebuild_stdout}");
}

#[test]
fn binary_enrich_dry_run_does_not_require_valid_llm_config() {
    let output = Command::new(binary())
        .args(["--db-path", &test_db_path(), "enrich", "--dry-run"])
        .env("RUSTY_RSS_OPENAI_BASE_URL", "not a url")
        .output()
        .expect("binary should run");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Would enrich 0 posts"));
}
