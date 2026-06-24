use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

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
