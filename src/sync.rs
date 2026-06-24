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
