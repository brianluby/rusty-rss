//! Database initialization, schema, FTS setup, and ad-hoc column migrations.

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use std::path::Path;
use std::time::Duration;

pub fn init_db(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path)
        .context(format!("failed to open database at {}", db_path.display()))?;

    // Wait on a contended write lock instead of failing immediately with
    // SQLITE_BUSY, so the IMMEDIATE transaction in `upsert_post` queues behind a
    // concurrent writer rather than erroring.
    conn.busy_timeout(Duration::from_secs(5))
        .context("failed to configure busy timeout")?;

    // SQLite leaves foreign keys off by default and the setting is per
    // connection, so enable it before any access for the schema's FK
    // constraints (enrichment_runs / outbound_captures / post_tags ->
    // saved_posts) to actually be enforced.
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .context("failed to enable foreign key enforcement")?;

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

        CREATE TABLE IF NOT EXISTS enrichment_runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            reddit_fullname TEXT NOT NULL,
            provider TEXT NOT NULL,
            model TEXT NOT NULL,
            prompt_version TEXT NOT NULL,
            status TEXT NOT NULL,
            raw_response TEXT,
            classification TEXT,
            tags_json TEXT,
            summary TEXT,
            joy_value REAL,
            work_value REAL,
            recommended_action TEXT,
            rationale TEXT,
            confidence REAL,
            error TEXT,
            created_at TEXT NOT NULL,
            FOREIGN KEY (reddit_fullname) REFERENCES saved_posts(reddit_fullname)
        );

        CREATE INDEX IF NOT EXISTS idx_enrichment_runs_post_created
            ON enrichment_runs(reddit_fullname, created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_enrichment_runs_post_id
            ON enrichment_runs(reddit_fullname, id DESC);
        CREATE INDEX IF NOT EXISTS idx_enrichment_runs_status
            ON enrichment_runs(status);
        CREATE INDEX IF NOT EXISTS idx_enrichment_runs_action
            ON enrichment_runs(recommended_action);

        CREATE TABLE IF NOT EXISTS outbound_captures (
            reddit_fullname TEXT PRIMARY KEY,
            original_url TEXT NOT NULL,
            final_url TEXT,
            canonical_url TEXT,
            title TEXT,
            description TEXT,
            site_name TEXT,
            preview_image_url TEXT,
            content_markdown TEXT,
            content_hash TEXT,
            status TEXT NOT NULL,
            http_status INTEGER,
            error TEXT,
            fetched_at TEXT NOT NULL,
            attempt_count INTEGER NOT NULL DEFAULT 1,
            FOREIGN KEY (reddit_fullname) REFERENCES saved_posts(reddit_fullname)
        );

        CREATE INDEX IF NOT EXISTS idx_outbound_captures_status
            ON outbound_captures(status);
        CREATE INDEX IF NOT EXISTS idx_outbound_captures_fetched_at
            ON outbound_captures(fetched_at);

        CREATE TABLE IF NOT EXISTS post_tags (
            reddit_fullname TEXT NOT NULL REFERENCES saved_posts(reddit_fullname),
            topic TEXT NOT NULL,
            score REAL NOT NULL,
            threshold REAL NOT NULL,
            passed INTEGER NOT NULL,
            matched_rules TEXT NOT NULL,
            signals TEXT,
            ruleset_version TEXT NOT NULL,
            tagged_at TEXT NOT NULL,
            PRIMARY KEY (reddit_fullname, topic)
        );

        CREATE INDEX IF NOT EXISTS idx_post_tags_topic ON post_tags(topic, passed);
        CREATE INDEX IF NOT EXISTS idx_post_tags_score ON post_tags(topic, score DESC);
        "#,
    )
    .context("failed to initialize database schema")?;

    ensure_column(&conn, "saved_posts", "content_markdown", "TEXT")?;
    ensure_column(&conn, "outbound_captures", "content_markdown", "TEXT")?;
    ensure_column(&conn, "outbound_captures", "content_hash", "TEXT")?;
    migrate_content_html_to_markdown(&conn)?;
    init_fts(&conn)?;
    rebuild_stale_fts_index(&conn)?;

    Ok(conn)
}

fn init_fts(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS posts_fts USING fts5(
            title,
            content_markdown,
            content='saved_posts',
            content_rowid='rowid',
            tokenize='porter unicode61'
        );

        CREATE TRIGGER IF NOT EXISTS saved_posts_ai AFTER INSERT ON saved_posts BEGIN
            INSERT INTO posts_fts(rowid, title, content_markdown)
            VALUES (new.rowid, new.title, new.content_markdown);
        END;

        CREATE TRIGGER IF NOT EXISTS saved_posts_ad AFTER DELETE ON saved_posts BEGIN
            INSERT INTO posts_fts(posts_fts, rowid, title, content_markdown)
            VALUES ('delete', old.rowid, old.title, old.content_markdown);
        END;

        CREATE TRIGGER IF NOT EXISTS saved_posts_au AFTER UPDATE ON saved_posts BEGIN
            INSERT INTO posts_fts(posts_fts, rowid, title, content_markdown)
            VALUES ('delete', old.rowid, old.title, old.content_markdown);
            INSERT INTO posts_fts(rowid, title, content_markdown)
            VALUES (new.rowid, new.title, new.content_markdown);
        END;
        "#,
    )
    .context("failed to initialize full-text search schema")?;

    Ok(())
}

fn rebuild_stale_fts_index(conn: &Connection) -> Result<()> {
    let saved_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM saved_posts", [], |row| row.get(0))
        .context("failed to count saved posts for FTS rebuild")?;
    if saved_count == 0 {
        return Ok(());
    }

    let indexed_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM posts_fts_docsize", [], |row| {
            row.get(0)
        })
        .context("failed to count indexed FTS documents")?;
    // Rebuild whenever the index is out of sync with the table, not only when it
    // is completely empty, so a partially populated or stale index is repaired.
    if indexed_count != saved_count {
        conn.execute("INSERT INTO posts_fts(posts_fts) VALUES ('rebuild')", [])
            .context("failed to rebuild full-text search index")?;
    }

    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::unique_db_path;
    use crate::db::{SearchFilters, search_posts};

    #[test]
    fn init_db_rebuilds_fts_for_preexisting_rows() {
        let path = unique_db_path("fts_backfill");
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
                content_markdown TEXT,
                thumbnail_url TEXT,
                published_at TEXT,
                updated_at TEXT,
                first_seen_at TEXT NOT NULL,
                last_seen_at TEXT NOT NULL,
                source TEXT NOT NULL DEFAULT 'atom',
                raw_entry TEXT
            );
            INSERT INTO saved_posts (
                reddit_fullname, reddit_id, title, permalink, content_markdown,
                first_seen_at, last_seen_at, source
            ) VALUES (
                't3_backfill', 'backfill', 'Backfill Searchable Title',
                'https://reddit.com/r/rust/comments/backfill/', 'legacy markdown content',
                '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'atom'
            );
            "#,
        )
        .expect("preexisting schema should be created");
        drop(conn);

        let conn = init_db(&path).expect("init should create and rebuild FTS");
        let hits = search_posts(&conn, "searchable", &SearchFilters::default(), 10)
            .expect("backfilled rows should search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].reddit_fullname, "t3_backfill");
    }

    #[test]
    fn init_db_adds_markdown_column_to_existing_schema() {
        let path = unique_db_path("migration");
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

        let hits = search_posts(&conn, "Markdown", &SearchFilters::default(), 10)
            .expect("migrated markdown should search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].reddit_fullname, "t3_old");
    }

    #[test]
    fn init_db_enables_foreign_key_enforcement() {
        let conn = init_db(&unique_db_path("fk")).expect("init should succeed");

        // No matching saved_posts row exists, so this child insert must be
        // rejected by the FK constraint once enforcement is enabled.
        let result = conn.execute(
            "INSERT INTO enrichment_runs
                 (reddit_fullname, provider, model, prompt_version, status, created_at)
             VALUES ('t3_missing', 'p', 'm', 'v', 'error', '2026-01-01T00:00:00Z')",
            [],
        );

        assert!(
            result.is_err(),
            "insert referencing a missing saved_post should violate the FK"
        );
    }

    #[test]
    fn init_db_configures_busy_timeout() {
        let conn = init_db(&unique_db_path("busy")).expect("init should succeed");
        let timeout: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .expect("busy_timeout should query");
        assert!(
            timeout >= 1000,
            "a busy timeout should be configured so writers queue, got {timeout}"
        );
    }

    #[test]
    fn init_db_rebuilds_partially_stale_fts_index() {
        let path = unique_db_path("fts_partial");
        let conn = init_db(&path).expect("init should succeed");
        for (fullname, title) in [("t3_one", "Alpha unique"), ("t3_two", "Beta unique")] {
            conn.execute(
                "INSERT INTO saved_posts
                     (reddit_fullname, reddit_id, title, permalink, first_seen_at, last_seen_at, source)
                 VALUES (?, ?, ?, ?, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'atom')",
                params![fullname, fullname, title, "https://example.com/"],
            )
            .expect("insert should succeed");
        }

        // Drop one document from the FTS index while leaving its table row in
        // place, leaving the index partially populated (indexed < saved).
        conn.execute(
            "INSERT INTO posts_fts(posts_fts, rowid, title, content_markdown)
             SELECT 'delete', rowid, title, content_markdown
             FROM saved_posts WHERE reddit_fullname = 't3_two'",
            [],
        )
        .expect("removing one FTS doc should succeed");
        drop(conn);

        // Re-init must detect the stale index and rebuild it, not skip it.
        let conn = init_db(&path).expect("re-init should rebuild");
        let hits = search_posts(&conn, "Beta", &SearchFilters::default(), 10)
            .expect("rebuilt index should search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].reddit_fullname, "t3_two");
    }
}
