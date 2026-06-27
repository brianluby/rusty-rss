//! `saved_posts` CRUD and listing.

use crate::models::SavedPost;
use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

pub fn upsert_post(conn: &Connection, post: &SavedPost) -> Result<UpsertResult> {
    let now = Utc::now().to_rfc3339();

    // The existence check and the follow-up write must be atomic. If the caller
    // is not already in a transaction, take a write lock up front (IMMEDIATE) so
    // concurrent upserts serialize instead of racing between the SELECT and the
    // write. If the caller already holds a transaction, reuse it: that provides
    // the atomicity, and starting a nested transaction would fail.
    if conn.is_autocommit() {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .context("failed to begin upsert transaction")?;
        let result = upsert_post_in_tx(&tx, post, &now)?;
        tx.commit().context("failed to commit upsert")?;
        Ok(result)
    } else {
        upsert_post_in_tx(conn, post, &now)
    }
}

/// Perform the existence check and insert/update. The caller is responsible for
/// the surrounding transaction (see [`upsert_post`]).
fn upsert_post_in_tx(conn: &Connection, post: &SavedPost, now: &str) -> Result<UpsertResult> {
    let existing = conn
        .query_row(
            "SELECT reddit_id, title, author, subreddit, permalink, outbound_url,
                    content_markdown, thumbnail_url, published_at, updated_at, source
             FROM saved_posts WHERE reddit_fullname = ?",
            params![post.reddit_fullname],
            ExistingPost::from_row,
        )
        .optional()
        .context("failed to check existing post")?;

    if existing.is_none() {
        conn.execute(
            r#"INSERT INTO saved_posts (
                reddit_fullname, reddit_id, title, author, subreddit,
                permalink, outbound_url, content_markdown, thumbnail_url,
                published_at, updated_at, first_seen_at, last_seen_at, source
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
            params![
                post.reddit_fullname,
                post.reddit_id,
                post.title,
                post.author,
                post.subreddit,
                post.permalink,
                post.outbound_url,
                post.content_markdown,
                post.thumbnail_url,
                post.published_at.as_ref().map(|d| d.to_rfc3339()),
                post.updated_at.as_ref().map(|d| d.to_rfc3339()),
                now,
                now,
                post.source,
            ],
        )
        .context("failed to insert post")?;

        Ok(UpsertResult::Inserted)
    } else if existing
        .as_ref()
        .is_some_and(|existing| existing.differs_from(post))
    {
        conn.execute(
            r#"UPDATE saved_posts SET
                reddit_id = ?, title = ?, author = ?, subreddit = ?, permalink = ?,
                outbound_url = ?, content_markdown = ?, thumbnail_url = ?,
                published_at = ?, updated_at = ?, last_seen_at = ?, source = ?
            WHERE reddit_fullname = ?"#,
            params![
                post.reddit_id,
                post.title,
                post.author,
                post.subreddit,
                post.permalink,
                post.outbound_url,
                post.content_markdown,
                post.thumbnail_url,
                post.published_at.as_ref().map(|d| d.to_rfc3339()),
                post.updated_at.as_ref().map(|d| d.to_rfc3339()),
                now,
                post.source,
                post.reddit_fullname,
            ],
        )
        .context("failed to update post")?;

        Ok(UpsertResult::Updated)
    } else {
        conn.execute(
            "UPDATE saved_posts SET last_seen_at = ? WHERE reddit_fullname = ?",
            params![now, post.reddit_fullname],
        )
        .context("failed to update last_seen_at")?;

        Ok(UpsertResult::Unchanged)
    }
}

#[derive(Debug)]
struct ExistingPost {
    reddit_id: String,
    title: String,
    author: Option<String>,
    subreddit: Option<String>,
    permalink: String,
    outbound_url: Option<String>,
    content_markdown: Option<String>,
    thumbnail_url: Option<String>,
    published_at: Option<String>,
    updated_at: Option<String>,
    source: String,
}

impl ExistingPost {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            reddit_id: row.get(0)?,
            title: row.get(1)?,
            author: row.get(2)?,
            subreddit: row.get(3)?,
            permalink: row.get(4)?,
            outbound_url: row.get(5)?,
            content_markdown: row.get(6)?,
            thumbnail_url: row.get(7)?,
            published_at: row.get(8)?,
            updated_at: row.get(9)?,
            source: row.get(10)?,
        })
    }

    fn differs_from(&self, post: &SavedPost) -> bool {
        self.reddit_id != post.reddit_id
            || self.title != post.title
            || self.author != post.author
            || self.subreddit != post.subreddit
            || self.permalink != post.permalink
            || self.outbound_url != post.outbound_url
            || self.content_markdown != post.content_markdown
            || self.thumbnail_url != post.thumbnail_url
            || self.published_at != post.published_at.as_ref().map(|date| date.to_rfc3339())
            || self.updated_at != post.updated_at.as_ref().map(|date| date.to_rfc3339())
            || self.source != post.source
    }
}

#[derive(Debug, Clone, Copy)]
pub enum UpsertResult {
    Inserted,
    Updated,
    Unchanged,
}

pub fn list_posts(conn: &Connection, limit: usize, offset: usize) -> Result<Vec<SavedPostRow>> {
    let mut stmt = conn.prepare(
        "SELECT reddit_fullname, title, author, subreddit, permalink, published_at, last_seen_at
         FROM saved_posts
         ORDER BY last_seen_at DESC, reddit_fullname DESC
         LIMIT ? OFFSET ?",
    )?;

    let rows = stmt
        .query_map(params![limit, offset], |row| {
            Ok(SavedPostRow {
                fullname: row.get(0)?,
                title: row.get(1)?,
                author: row.get(2)?,
                subreddit: row.get(3)?,
                permalink: row.get(4)?,
                published_at: row.get(5)?,
                last_seen_at: row.get(6)?,
            })
        })
        .context("failed to query posts")?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect posts")
}

#[derive(Debug, serde::Serialize)]
pub struct SavedPostRow {
    pub fullname: String,
    pub title: String,
    pub author: Option<String>,
    pub subreddit: Option<String>,
    pub permalink: String,
    pub published_at: Option<String>,
    pub last_seen_at: String,
}

pub fn get_post(conn: &Connection, fullname: &str) -> Result<Option<SavedPost>> {
    let row = conn
        .query_row(
            "SELECT reddit_fullname, reddit_id, title, author, subreddit, permalink,
                    outbound_url, content_markdown, thumbnail_url, published_at, updated_at,
                    first_seen_at, last_seen_at, source
             FROM saved_posts WHERE reddit_fullname = ?",
            params![fullname],
            |row| {
                Ok(SavedPost {
                    reddit_fullname: row.get(0)?,
                    reddit_id: row.get(1)?,
                    title: row.get(2)?,
                    author: row.get(3)?,
                    subreddit: row.get(4)?,
                    permalink: row.get(5)?,
                    outbound_url: row.get(6)?,
                    content_markdown: row.get(7)?,
                    thumbnail_url: row.get(8)?,
                    published_at: parse_optional_timestamp(row, 9)?,
                    updated_at: parse_optional_timestamp(row, 10)?,
                    source: row.get(13)?,
                })
            },
        )
        .optional();

    match row {
        Ok(post) => Ok(post),
        Err(e) => Err(e).context("failed to query post"),
    }
}

pub fn count_posts(conn: &Connection) -> Result<usize> {
    conn.query_row("SELECT COUNT(*) FROM saved_posts", [], |row| {
        row.get::<_, usize>(0)
    })
    .context("failed to count posts")
}

pub(super) fn saved_post_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SavedPost> {
    Ok(SavedPost {
        reddit_fullname: row.get(0)?,
        reddit_id: row.get(1)?,
        title: row.get(2)?,
        author: row.get(3)?,
        subreddit: row.get(4)?,
        permalink: row.get(5)?,
        outbound_url: row.get(6)?,
        content_markdown: row.get(7)?,
        thumbnail_url: row.get(8)?,
        published_at: parse_optional_timestamp(row, 9)?,
        updated_at: parse_optional_timestamp(row, 10)?,
        source: row.get(13)?,
    })
}

/// Parse an optional RFC 3339 timestamp column into UTC. A malformed value
/// surfaces as a row-conversion error instead of being silently dropped to
/// `None`, so corrupted persisted data is distinguishable from a missing value.
fn parse_optional_timestamp(
    row: &rusqlite::Row<'_>,
    idx: usize,
) -> rusqlite::Result<Option<chrono::DateTime<Utc>>> {
    row.get::<_, Option<String>>(idx)?
        .map(|s| chrono::DateTime::parse_from_rfc3339(&s).map(|dt| dt.with_timezone(&Utc)))
        .transpose()
        .map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                idx,
                rusqlite::types::Type::Text,
                Box::new(err),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{test_db, test_post};

    #[test]
    fn upsert_inserts_new_post() {
        let conn = test_db();
        let post = test_post();

        let result = upsert_post(&conn, &post).expect("upsert should succeed");
        assert!(matches!(result, UpsertResult::Inserted));

        let count = count_posts(&conn).expect("count should succeed");
        assert_eq!(count, 1);
    }

    #[test]
    fn upsert_updates_existing_post() {
        let conn = test_db();
        let mut post = test_post();
        upsert_post(&conn, &post).expect("first upsert should succeed");

        std::thread::sleep(std::time::Duration::from_millis(10));

        post.title = "Updated Title".to_string();
        let result = upsert_post(&conn, &post).expect("second upsert should succeed");
        assert!(matches!(result, UpsertResult::Updated));

        let count = count_posts(&conn).expect("count should succeed");
        assert_eq!(count, 1);

        let fetched = get_post(&conn, "t3_test123")
            .expect("get should succeed")
            .expect("post should exist");
        assert_eq!(fetched.title, "Updated Title");
    }

    #[test]
    fn upsert_updates_existing_markdown_content() {
        let conn = test_db();
        let mut post = test_post();
        upsert_post(&conn, &post).expect("first upsert should succeed");

        post.content_markdown = Some("Updated markdown".to_string());
        let result = upsert_post(&conn, &post).expect("second upsert should succeed");

        assert!(matches!(result, UpsertResult::Updated));
        let fetched = get_post(&conn, "t3_test123")
            .expect("get should succeed")
            .expect("post should exist");
        assert_eq!(
            fetched.content_markdown,
            Some("Updated markdown".to_string())
        );
    }

    #[test]
    fn upsert_unchanged_when_no_diff() {
        let conn = test_db();
        let post = test_post();
        upsert_post(&conn, &post).expect("first upsert should succeed");

        std::thread::sleep(std::time::Duration::from_millis(10));

        let result = upsert_post(&conn, &post).expect("second upsert should succeed");
        assert!(matches!(result, UpsertResult::Unchanged));
    }

    #[test]
    fn first_seen_at_is_preserved_on_update() {
        let conn = test_db();
        let post = test_post();
        upsert_post(&conn, &post).expect("first upsert should succeed");

        let read_timestamps = || {
            conn.query_row(
                "SELECT first_seen_at, last_seen_at FROM saved_posts WHERE reddit_fullname = ?",
                params!["t3_test123"],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .expect("timestamps should query")
        };
        let (first_seen, first_last_seen) = read_timestamps();

        std::thread::sleep(std::time::Duration::from_millis(10));

        let mut updated = test_post();
        updated.title = "New Title".to_string();
        upsert_post(&conn, &updated).expect("update should succeed");

        let (second_seen, second_last_seen) = read_timestamps();

        // The update must preserve first_seen_at while advancing last_seen_at.
        assert_eq!(
            first_seen, second_seen,
            "first_seen_at must not change on update"
        );
        assert_ne!(
            first_last_seen, second_last_seen,
            "last_seen_at should advance on update"
        );

        let fetched = get_post(&conn, "t3_test123")
            .expect("get should succeed")
            .expect("post should exist");
        assert_eq!(fetched.title, "New Title");
    }

    #[test]
    fn malformed_timestamp_surfaces_as_error() {
        let conn = test_db();
        let post = test_post();
        upsert_post(&conn, &post).expect("post should insert");

        conn.execute(
            "UPDATE saved_posts SET published_at = 'not-a-timestamp' WHERE reddit_fullname = ?",
            params!["t3_test123"],
        )
        .expect("corrupting the timestamp should succeed");

        let err = get_post(&conn, "t3_test123").expect_err("malformed timestamp should fail");
        assert!(err.to_string().contains("failed to query post"));
    }

    #[test]
    fn list_posts_returns_posts() {
        let conn = test_db();
        let post = test_post();
        upsert_post(&conn, &post).expect("upsert should succeed");

        let listed = list_posts(&conn, 10, 0).expect("list should succeed");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "Test Post");
    }

    #[test]
    fn idempotent_sync() {
        let conn = test_db();
        let post = test_post();

        upsert_post(&conn, &post).expect("first sync should succeed");
        upsert_post(&conn, &post).expect("second sync should succeed");
        upsert_post(&conn, &post).expect("third sync should succeed");

        let count = count_posts(&conn).expect("count should succeed");
        assert_eq!(count, 1);
    }
}
