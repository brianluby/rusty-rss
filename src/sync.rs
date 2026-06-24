use crate::config::Config;
use crate::db::{self, UpsertResult};
use crate::fetch;
use crate::models::SyncResult;
use crate::parse;
use anyhow::Result;
use chrono::Utc;
use rusqlite::params;

pub async fn run_sync(config: &Config) -> Result<SyncResult> {
    let client = fetch::build_http_client(&config.user_agent);
    let now = Utc::now();

    let run_id = record_sync_start(&config.db_path, &config.feed_url, &now)?;

    let result = do_sync(&client, config).await;

    match &result {
        Ok(sr) => {
            record_sync_end(
                &config.db_path,
                run_id,
                "success",
                sr.fetched_count,
                sr.inserted_count,
                sr.updated_count,
                None,
            )?;
        }
        Err(e) => {
            record_sync_end(
                &config.db_path,
                run_id,
                "error",
                0,
                0,
                0,
                Some(&e.to_string()),
            )?;
        }
    }

    result
}

async fn do_sync(client: &reqwest::Client, config: &Config) -> Result<SyncResult> {
    let body = fetch::fetch_feed(client, &config.feed_url).await?;
    let parsed = parse::parse_atom(&body)?;

    let conn = db::init_db(&config.db_path)?;
    let mut result = SyncResult::new();
    result.fetched_count = parsed.posts.len() + parsed.errors.len();
    result.parse_errors = parsed.errors;

    for post in &parsed.posts {
        match db::upsert_post(&conn, post) {
            Ok(UpsertResult::Inserted) => result.inserted_count += 1,
            Ok(UpsertResult::Updated) => result.updated_count += 1,
            Ok(UpsertResult::Unchanged) => result.unchanged_count += 1,
            Err(e) => {
                result
                    .parse_errors
                    .push(format!("failed to upsert {}: {}", post.reddit_fullname, e));
            }
        }
    }

    Ok(result)
}

fn record_sync_start(
    db_path: &std::path::Path,
    source_url: &str,
    now: &chrono::DateTime<Utc>,
) -> Result<i64> {
    let conn = db::init_db(db_path)?;
    conn.execute(
        "INSERT INTO sync_runs (started_at, source_url, status) VALUES (?, ?, ?)",
        params![now.to_rfc3339(), source_url, "running"],
    )?;
    Ok(conn.last_insert_rowid())
}

fn record_sync_end(
    db_path: &std::path::Path,
    run_id: i64,
    status: &str,
    fetched: usize,
    inserted: usize,
    updated: usize,
    error: Option<&str>,
) -> Result<()> {
    let conn = db::init_db(db_path)?;
    conn.execute(
        "UPDATE sync_runs SET finished_at = ?, status = ?, fetched_count = ?, inserted_count = ?, updated_count = ?, error = ? WHERE id = ?",
        params![
            Utc::now().to_rfc3339(),
            status,
            fetched,
            inserted,
            updated,
            error,
            run_id
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn test_db_path() -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "rusty_rss_sync_test_{}_{}.db",
            std::process::id(),
            id
        ))
    }

    fn serve_feed(body: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("server should bind");
        let addr = listener.local_addr().expect("local address should exist");

        std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("server should accept request");
                let mut request = [0u8; 2048];
                let _ = stream
                    .read(&mut request)
                    .expect("request should be readable");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/atom+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("response should be written");
            }
        });

        format!("http://{addr}/feed")
    }

    fn config_for(feed_url: String, db_path: PathBuf) -> Config {
        Config {
            feed_url,
            db_path,
            user_agent: "rusty-rss-test/1.0".to_string(),
        }
    }

    #[tokio::test]
    async fn run_sync_inserts_then_reports_unchanged() {
        let feed =
            std::fs::read_to_string("test-fixtures/atom-feed.xml").expect("fixture should exist");
        let db_path = test_db_path();
        let config = config_for(serve_feed(feed), db_path.clone());

        let first = run_sync(&config).await.expect("first sync should succeed");
        let second = run_sync(&config).await.expect("second sync should succeed");

        assert_eq!(first.fetched_count, 3);
        assert_eq!(first.inserted_count, 3);
        assert_eq!(first.updated_count, 0);
        assert_eq!(first.unchanged_count, 0);
        assert!(first.parse_errors.is_empty());

        assert_eq!(second.fetched_count, 3);
        assert_eq!(second.inserted_count, 0);
        assert_eq!(second.updated_count, 0);
        assert_eq!(second.unchanged_count, 3);

        let conn = db::init_db(&db_path).expect("db should open");
        let count = db::count_posts(&conn).expect("count should work");
        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn run_sync_returns_entry_parse_errors() {
        let feed = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Invalid</title>
  <link href="https://example.com" rel="self"/>
  <id>tag:example,2026:invalid</id>
  <updated>2026-01-01T00:00:00Z</updated>
  <entry>
    <id>t3_valid</id>
    <title>Valid</title>
    <link href="https://www.reddit.com/r/rust/comments/valid/" rel="alternate"/>
    <updated>2026-01-01T00:00:00Z</updated>
  </entry>
  <entry>
    <id>t3_invalid</id>
    <link href="https://www.reddit.com/r/rust/comments/invalid/" rel="alternate"/>
    <updated>2026-01-01T00:00:00Z</updated>
  </entry>
</feed>"#;

        let db_path = test_db_path();
        let config = config_for(serve_feed(feed.to_string()), db_path);

        let result = run_sync(&config).await.expect("sync should succeed");

        assert_eq!(result.fetched_count, 2);
        assert_eq!(result.inserted_count, 1);
        assert_eq!(result.parse_errors.len(), 1);
        assert!(result.parse_errors[0].contains("missing entry/title"));
    }

    #[test]
    fn records_failed_sync_end() {
        let db_path = test_db_path();
        let now = Utc::now();
        let run_id = record_sync_start(&db_path, "https://example.com/feed", &now)
            .expect("sync start should be recorded");

        record_sync_end(&db_path, run_id, "error", 0, 0, 0, Some("boom"))
            .expect("sync end should be recorded");

        let conn = db::init_db(&db_path).expect("db should open");
        let status: String = conn
            .query_row(
                "SELECT status FROM sync_runs WHERE id = ?",
                [run_id],
                |row| row.get(0),
            )
            .expect("status should exist");
        let error: String = conn
            .query_row(
                "SELECT error FROM sync_runs WHERE id = ?",
                [run_id],
                |row| row.get(0),
            )
            .expect("error should exist");

        assert_eq!(status, "error");
        assert_eq!(error, "boom");
    }
}
