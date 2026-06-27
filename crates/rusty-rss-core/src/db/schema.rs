//! Database initialization and full-text-search index repair.
//!
//! Schema creation and column/data migrations live in [`super::migrations`],
//! which `init_db` drives through a `PRAGMA user_version`-gated, transactional
//! runner. This module owns connection setup (busy timeout, foreign-key
//! enforcement) and the per-init FTS drift repair that runs on every open.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;
use std::time::Duration;

use super::migrations;

pub fn init_db(db_path: &Path) -> Result<Connection> {
    let mut conn = Connection::open(db_path)
        .context(format!("failed to open database at {}", db_path.display()))?;

    // Wait on a contended write lock instead of failing immediately with
    // SQLITE_BUSY, so the IMMEDIATE transaction in `upsert_post` queues behind a
    // concurrent writer rather than erroring.
    conn.busy_timeout(Duration::from_secs(5))
        .context("failed to configure busy timeout")?;

    // SQLite leaves foreign keys off by default and the setting is per
    // connection, so enable it before any access for the schema's FK
    // constraints (enrichment_runs / outbound_captures / post_tags ->
    // saved_posts) to actually be enforced. This must precede the migration
    // runner so foreign keys are enforced during migrations too.
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .context("failed to enable foreign key enforcement")?;

    // Apply pending schema/data migrations (idempotent on an up-to-date DB).
    migrations::run_migrations(&mut conn)?;

    // Repair any external-content FTS index that drifted from its content table
    // (rows predating the index, or a partially populated index). This runs on
    // every open, independent of the migration version, so it also backfills a
    // freshly created index for rows a baseline migration just imported.
    rebuild_stale_fts_index(&conn)?;

    Ok(conn)
}

fn rebuild_stale_fts_index(conn: &Connection) -> Result<()> {
    // Backfill/repair each external-content index whenever it drifts from its
    // content table (e.g. rows that predate the index, or a partially populated
    // index), not only when it is empty.
    rebuild_index_if_stale(conn, "saved_posts", "posts_fts")?;
    rebuild_index_if_stale(conn, "outbound_captures", "capture_fts")?;
    rebuild_index_if_stale(conn, "enrichment_runs", "enrichment_fts")?;

    Ok(())
}

/// Rebuild `fts_table` from `content_table` when their row counts disagree.
///
/// Both names are compile-time constants from this module (never user input), so
/// interpolating them into the SQL is safe. The `<fts>_docsize` shadow table is
/// created automatically for every FTS5 table and holds one row per indexed
/// document, so its count is the indexed-document count.
fn rebuild_index_if_stale(conn: &Connection, content_table: &str, fts_table: &str) -> Result<()> {
    let content_count: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM {content_table}"),
            [],
            |row| row.get(0),
        )
        .context(format!("failed to count {content_table} for FTS rebuild"))?;

    let indexed_count: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM {fts_table}_docsize"),
            [],
            |row| row.get(0),
        )
        .context(format!("failed to count indexed {fts_table} documents"))?;

    if indexed_count != content_count {
        conn.execute(
            &format!("INSERT INTO {fts_table}({fts_table}) VALUES ('rebuild')"),
            [],
        )
        .context(format!("failed to rebuild {fts_table} index"))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::unique_db_path;
    use crate::db::{SearchFilters, search_posts};
    use rusqlite::params;

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

    #[test]
    fn init_db_clears_orphaned_fts_index_when_table_empty() {
        let path = unique_db_path("fts_orphan");
        let conn = init_db(&path).expect("init should succeed");
        conn.execute(
            "INSERT INTO saved_posts
                 (reddit_fullname, reddit_id, title, permalink, first_seen_at, last_seen_at, source)
             VALUES ('t3_orphan', 'orphan', 'Orphan title', 'https://example.com/',
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'atom')",
            [],
        )
        .expect("insert should succeed");

        // Drop the delete trigger, then clear the table so the FTS document is
        // orphaned: saved_posts is empty but posts_fts_docsize still has a row.
        conn.execute_batch("DROP TRIGGER saved_posts_ad; DELETE FROM saved_posts;")
            .expect("orphaning the index should succeed");
        let indexed_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM posts_fts_docsize", [], |row| {
                row.get(0)
            })
            .expect("count should query");
        assert_eq!(indexed_before, 1, "precondition: FTS index is orphaned");
        drop(conn);

        // Re-init must clear the orphaned index even though the table is empty.
        let conn = init_db(&path).expect("re-init should clear orphaned index");
        let indexed_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM posts_fts_docsize", [], |row| {
                row.get(0)
            })
            .expect("count should query");
        assert_eq!(indexed_after, 0, "orphaned FTS index should be cleared");
    }

    fn user_version(conn: &Connection) -> i64 {
        conn.query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("user_version should query")
    }

    #[test]
    fn fresh_db_lands_at_latest_user_version() {
        let conn = init_db(&unique_db_path("fresh_version")).expect("init should succeed");
        assert_eq!(
            user_version(&conn),
            migrations::LATEST_VERSION,
            "a freshly initialized database should be at the latest schema version"
        );
    }

    #[test]
    fn reinit_on_up_to_date_db_is_idempotent() {
        let path = unique_db_path("idempotent");
        let conn = init_db(&path).expect("first init should succeed");
        assert_eq!(user_version(&conn), migrations::LATEST_VERSION);
        drop(conn);

        // Re-running init on an up-to-date DB must apply no migrations and leave
        // the version unchanged.
        let conn = init_db(&path).expect("re-init should succeed");
        assert_eq!(
            user_version(&conn),
            migrations::LATEST_VERSION,
            "re-running init must not advance past the latest version"
        );
    }

    #[test]
    fn upgrade_from_pre_user_version_schema_advances_version_without_data_loss() {
        // A pre-user_version database: content_html, no content_markdown/
        // content_hash, no FTS tables, and user_version still 0.
        let path = unique_db_path("upgrade_version");
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
                't3_legacy', 'legacy', 'Legacy Post',
                'https://reddit.com/r/rust/comments/legacy/',
                '<p>Durable <strong>Content</strong></p>',
                '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'atom'
            );
            "#,
        )
        .expect("legacy schema should be created");
        assert_eq!(
            user_version(&conn),
            0,
            "precondition: legacy DB is version 0"
        );
        drop(conn);

        let conn = init_db(&path).expect("init should upgrade legacy schema");

        // Version advanced to the latest.
        assert_eq!(user_version(&conn), migrations::LATEST_VERSION);

        // Columns added by later migrations now exist.
        let mut stmt = conn
            .prepare("PRAGMA table_info(saved_posts)")
            .expect("pragma should prepare");
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .expect("columns should query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("columns should collect");
        assert!(columns.contains(&"content_markdown".to_string()));

        // Original row survived and its content was converted, not lost.
        let (title, markdown): (String, String) = conn
            .query_row(
                "SELECT title, content_markdown FROM saved_posts WHERE reddit_fullname = 't3_legacy'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("legacy row should survive the upgrade");
        assert_eq!(title, "Legacy Post");
        assert_eq!(markdown, "Durable **Content**");

        // And the converted content is searchable through the new FTS index.
        let hits = search_posts(&conn, "Durable", &SearchFilters::default(), 10)
            .expect("upgraded content should search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].reddit_fullname, "t3_legacy");
    }

    /// Stage a real (current) schema, then rewind `user_version` to `version` and
    /// reintroduce a legacy `content_html` row (and, for v1, drop `content_hash`)
    /// so the migration runner has pending work to resume. Returns the path.
    fn stage_mid_migration_db(label: &str, version: i64) -> std::path::PathBuf {
        let path = unique_db_path(label);
        let conn = init_db(&path).expect("initial schema build should succeed");

        conn.execute_batch("ALTER TABLE saved_posts ADD COLUMN content_html TEXT;")
            .expect("reintroducing legacy content_html column should succeed");

        // A pre-conversion row: content stored only as HTML, content_markdown NULL.
        conn.execute(
            "INSERT INTO saved_posts (
                reddit_fullname, reddit_id, title, permalink, content_html,
                content_markdown, first_seen_at, last_seen_at, source
            ) VALUES (
                't3_mid', 'mid', 'Mid Migration Post',
                'https://reddit.com/r/rust/comments/mid/',
                '<p>Resumable <strong>State</strong></p>', NULL,
                '2026-02-02T00:00:00Z', '2026-02-02T00:00:00Z', 'atom'
            )",
            [],
        )
        .expect("legacy row should insert");

        if version == 1 {
            // At v1 the migration-2 columns are not yet present, so dropping
            // content_hash forces migration 2 to do real work on resume.
            conn.execute_batch("ALTER TABLE outbound_captures DROP COLUMN content_hash;")
                .expect("dropping content_hash to simulate pre-migration-2 state should succeed");
        }

        conn.execute_batch(&format!("PRAGMA user_version = {version};"))
            .expect("rewinding user_version should succeed");
        drop(conn);
        path
    }

    /// A column name appears in `PRAGMA table_info(table)`.
    fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .expect("pragma should prepare");
        stmt.query_map([], |row| row.get::<_, String>(1))
            .expect("columns should query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("columns should collect")
            .iter()
            .any(|name| name == column)
    }

    #[test]
    fn init_db_resumes_from_mid_migration_user_version_1() {
        let path = stage_mid_migration_db("mid_v1", 1);

        let conn = init_db(&path).expect("init should resume migrations from v1");

        assert_eq!(
            user_version(&conn),
            migrations::LATEST_VERSION,
            "a DB stalled at v1 must finish at the latest version"
        );
        // Migration 2 re-added the dropped column.
        assert!(
            has_column(&conn, "outbound_captures", "content_hash"),
            "migration 2 should restore content_hash on resume"
        );
        // Migration 3 converted the legacy HTML without losing the row.
        let markdown: String = conn
            .query_row(
                "SELECT content_markdown FROM saved_posts WHERE reddit_fullname = 't3_mid'",
                [],
                |row| row.get(0),
            )
            .expect("legacy row should survive the resumed migration");
        assert_eq!(markdown, "Resumable **State**");

        let hits = search_posts(&conn, "Resumable", &SearchFilters::default(), 10)
            .expect("converted content should search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].reddit_fullname, "t3_mid");
    }

    #[test]
    fn init_db_resumes_from_mid_migration_user_version_2() {
        let path = stage_mid_migration_db("mid_v2", 2);

        let conn = init_db(&path).expect("init should resume migrations from v2");

        assert_eq!(
            user_version(&conn),
            migrations::LATEST_VERSION,
            "a DB stalled at v2 must finish at the latest version"
        );
        // Only migration 3 remained: convert content_html, no data loss.
        let markdown: String = conn
            .query_row(
                "SELECT content_markdown FROM saved_posts WHERE reddit_fullname = 't3_mid'",
                [],
                |row| row.get(0),
            )
            .expect("legacy row should survive the resumed migration");
        assert_eq!(markdown, "Resumable **State**");

        let hits = search_posts(&conn, "Resumable", &SearchFilters::default(), 10)
            .expect("converted content should search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].reddit_fullname, "t3_mid");
    }
}
