use crate::models::SavedPost;
use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

pub fn init_db(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path)
        .context(format!("failed to open database at {}", db_path.display()))?;

    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS saved_posts (
            reddit_fullname TEXT PRIMARY KEY,
            reddit_id TEXT,
            title TEXT NOT NULL,
            author TEXT,
            subreddit TEXT,
            permalink TEXT NOT NULL,
            outbound_url TEXT,
            content_markdown TEXT,
            thumbnail_url TEXT,
            published_at TEXT,
            updated_at TEXT,
            first_seen_at TEXT NOT NULL,
            last_seen_at TEXT NOT NULL,
            source TEXT NOT NULL DEFAULT 'atom',
            raw_entry TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_saved_posts_subreddit ON saved_posts(subreddit);
        CREATE INDEX IF NOT EXISTS idx_saved_posts_author ON saved_posts(author);
        CREATE INDEX IF NOT EXISTS idx_saved_posts_published_at ON saved_posts(published_at);
        CREATE INDEX IF NOT EXISTS idx_saved_posts_last_seen_at ON saved_posts(last_seen_at);

        CREATE TABLE IF NOT EXISTS sync_runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            started_at TEXT NOT NULL,
            finished_at TEXT,
            source_url TEXT NOT NULL,
            status TEXT NOT NULL,
            fetched_count INTEGER DEFAULT 0,
            inserted_count INTEGER DEFAULT 0,
            updated_count INTEGER DEFAULT 0,
            error TEXT
        );
        "#,
    )
    .context("failed to initialize database schema")?;

    ensure_column(&conn, "saved_posts", "content_markdown", "TEXT")?;
    migrate_content_html_to_markdown(&conn)?;

    Ok(conn)
}

fn ensure_column(conn: &Connection, table: &str, column: &str, column_type: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    if !columns.iter().any(|name| name == column) {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {column_type}"),
            [],
        )
        .context(format!("failed to add {table}.{column}"))?;
    }

    Ok(())
}

fn migrate_content_html_to_markdown(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(saved_posts)")?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    if !columns.iter().any(|name| name == "content_html") {
        return Ok(());
    }

    let mut stmt = conn.prepare(
        "SELECT reddit_fullname, content_html FROM saved_posts
         WHERE content_markdown IS NULL AND content_html IS NOT NULL",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);

    for (fullname, html) in rows {
        let markdown = html2md::parse_html(&html).trim().to_string();
        conn.execute(
            "UPDATE saved_posts SET content_markdown = ? WHERE reddit_fullname = ?",
            params![markdown, fullname],
        )
        .context("failed to migrate content_html to content_markdown")?;
    }

    Ok(())
}

pub fn upsert_post(conn: &Connection, post: &SavedPost) -> Result<UpsertResult> {
    let now = Utc::now().to_rfc3339();

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

        return Ok(UpsertResult::Inserted);
    }

    let needs_update = existing
        .as_ref()
        .is_some_and(|existing| existing.differs_from(post));

    if needs_update {
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
         ORDER BY last_seen_at DESC
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

#[derive(Debug)]
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
                    published_at: row.get::<_, Option<String>>(9)?.and_then(|s| {
                        chrono::DateTime::parse_from_rfc3339(&s)
                            .map(|dt| dt.with_timezone(&Utc))
                            .ok()
                    }),
                    updated_at: row.get::<_, Option<String>>(10)?.and_then(|s| {
                        chrono::DateTime::parse_from_rfc3339(&s)
                            .map(|dt| dt.with_timezone(&Utc))
                            .ok()
                    }),
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn test_db() -> Connection {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("rusty_rss_test_{}_{}.db", std::process::id(), id));
        let path_str = path.to_str().unwrap();
        let _ = std::fs::remove_file(path_str);
        init_db(std::path::Path::new(path_str)).expect("init db should succeed")
    }

    fn test_post() -> SavedPost {
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

        let first = get_post(&conn, "t3_test123")
            .expect("get should succeed")
            .expect("post should exist");
        let first_seen = first.reddit_fullname.clone();

        std::thread::sleep(std::time::Duration::from_millis(10));

        let mut updated = test_post();
        updated.title = "New Title".to_string();
        upsert_post(&conn, &updated).expect("update should succeed");

        let second = get_post(&conn, "t3_test123")
            .expect("get should succeed")
            .expect("post should exist");

        assert_eq!(first_seen, "t3_test123");
        assert_eq!(second.title, "New Title");
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

    #[test]
    fn init_db_adds_markdown_column_to_existing_schema() {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "rusty_rss_migration_test_{}_{}.db",
            std::process::id(),
            id
        ));
        let conn = Connection::open(&path).expect("db should open");
        conn.execute_batch(
            r#"
            CREATE TABLE saved_posts (
                reddit_fullname TEXT PRIMARY KEY,
                reddit_id TEXT,
                title TEXT NOT NULL,
                author TEXT,
                subreddit TEXT,
                permalink TEXT NOT NULL,
                outbound_url TEXT,
                content_html TEXT,
                thumbnail_url TEXT,
                published_at TEXT,
                updated_at TEXT,
                first_seen_at TEXT NOT NULL,
                last_seen_at TEXT NOT NULL,
                source TEXT NOT NULL DEFAULT 'atom',
                raw_entry TEXT
            );
            INSERT INTO saved_posts (
                reddit_fullname, reddit_id, title, permalink, content_html,
                first_seen_at, last_seen_at, source
            ) VALUES (
                't3_old', 'old', 'Old Post', 'https://reddit.com/r/rust/comments/old/',
                '<p>Hello <strong>Markdown</strong></p>',
                '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'atom'
            );
            "#,
        )
        .expect("old schema should be created");
        drop(conn);

        let conn = init_db(&path).expect("init should migrate schema");
        let mut stmt = conn
            .prepare("PRAGMA table_info(saved_posts)")
            .expect("pragma should prepare");
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .expect("columns should query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("columns should collect");

        assert!(columns.contains(&"content_markdown".to_string()));

        let migrated: String = conn
            .query_row(
                "SELECT content_markdown FROM saved_posts WHERE reddit_fullname = 't3_old'",
                [],
                |row| row.get(0),
            )
            .expect("migrated markdown should exist");
        assert_eq!(migrated, "Hello **Markdown**");
    }
}
