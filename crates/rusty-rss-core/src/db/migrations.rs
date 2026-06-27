//! PRAGMA `user_version`-gated, transactional schema migrations.
//!
//! [`run_migrations`] reads the current `user_version`, then applies every
//! pending migration in ascending order, each wrapped in its own transaction so
//! a failure rolls the step back as a unit. The new `user_version` is written
//! inside the same transaction as the step it marks, so the version and the
//! schema change commit or roll back together. Re-running on an up-to-date
//! database applies nothing.
//!
//! Migrations are forward-only and append-only: never edit a shipped migration,
//! add a new numbered step instead. Baseline DDL uses `IF NOT EXISTS` and column
//! adds are guarded by [`column_exists`] so the runner is also safe on legacy
//! databases that predate `user_version` (they report version `0` while already
//! holding some tables).

use anyhow::{Context, Result};
use rusqlite::{Connection, Transaction, params};

/// The schema version a freshly initialized database lands on.
pub(crate) const LATEST_VERSION: i64 = 3;

/// A single forward-only migration step.
struct Migration {
    version: i64,
    name: &'static str,
    apply: fn(&Transaction) -> Result<()>,
}

/// The highest migration version must always equal [`LATEST_VERSION`], so a
/// fresh database lands exactly where the runner expects. Enforced at compile
/// time so adding a migration without bumping the constant fails to build.
const _: () = assert!(
    MIGRATIONS[MIGRATIONS.len() - 1].version == LATEST_VERSION,
    "LATEST_VERSION must equal the highest migration version"
);

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "baseline schema",
        apply: migrate_baseline,
    },
    Migration {
        version: 2,
        name: "add markdown and content-hash columns",
        apply: migrate_add_columns,
    },
    Migration {
        version: 3,
        name: "convert content_html to content_markdown",
        apply: migrate_content_html_to_markdown,
    },
];

/// Apply all pending migrations in ascending order, each in its own transaction.
///
/// Idempotent: when `user_version` already equals [`LATEST_VERSION`] no
/// transaction is opened and nothing changes.
pub(crate) fn run_migrations(conn: &mut Connection) -> Result<()> {
    let current: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .context("failed to read schema user_version")?;

    for migration in MIGRATIONS.iter().filter(|m| m.version > current) {
        let tx = conn
            .transaction()
            .context("failed to open migration transaction")?;

        (migration.apply)(&tx).with_context(|| {
            format!(
                "migration {} ({}) failed",
                migration.version, migration.name
            )
        })?;

        // user_version is part of the database header and participates in the
        // transaction, so the version bump commits or rolls back atomically
        // with the schema change above. The value is a compile-time constant,
        // never user input, so interpolating it is safe.
        tx.execute_batch(&format!("PRAGMA user_version = {};", migration.version))
            .with_context(|| format!("failed to set user_version = {}", migration.version))?;

        tx.commit().with_context(|| {
            format!(
                "failed to commit migration {} ({})",
                migration.version, migration.name
            )
        })?;
    }

    Ok(())
}

/// Migration 1: baseline tables, indexes, and external-content FTS5 schema.
///
/// All DDL uses `IF NOT EXISTS` so a legacy database that already holds some of
/// these objects (reporting `user_version = 0`) upgrades cleanly without
/// clobbering existing data.
fn migrate_baseline(tx: &Transaction) -> Result<()> {
    tx.execute_batch(BASELINE_SCHEMA_SQL)
        .context("failed to apply baseline schema")?;
    tx.execute_batch(FTS_SCHEMA_SQL)
        .context("failed to apply baseline full-text search schema")?;
    Ok(())
}

/// Migration 2: add columns that postdate the original baseline.
///
/// Guarded by [`column_exists`] so it is a no-op on a fresh database (where
/// migration 1 already created the columns) and adds the columns on a legacy
/// database that lacks them.
fn migrate_add_columns(tx: &Transaction) -> Result<()> {
    ensure_column(tx, "saved_posts", "content_markdown", "TEXT")?;
    ensure_column(tx, "outbound_captures", "content_markdown", "TEXT")?;
    ensure_column(tx, "outbound_captures", "content_hash", "TEXT")?;
    Ok(())
}

/// Migration 3: backfill `content_markdown` from a legacy `content_html` column.
///
/// Only legacy databases carry `content_html`; on every other database this is a
/// no-op. Each `UPDATE` fires the `saved_posts_au` trigger, so the FTS index is
/// kept in sync as rows are converted.
fn migrate_content_html_to_markdown(tx: &Transaction) -> Result<()> {
    if !column_exists(tx, "saved_posts", "content_html")? {
        return Ok(());
    }

    let rows = {
        let mut stmt = tx.prepare(
            "SELECT reddit_fullname, content_html FROM saved_posts
             WHERE content_markdown IS NULL AND content_html IS NOT NULL",
        )?;
        stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
    };

    if rows.is_empty() {
        return Ok(());
    }

    // The rows being converted predate the FTS index created in migration 1, so
    // they are not yet indexed. Populate the index before updating: otherwise
    // each UPDATE's AFTER UPDATE trigger issues a `'delete'` for an unindexed
    // rowid and corrupts the external-content index.
    tx.execute_batch("INSERT INTO posts_fts(posts_fts) VALUES ('rebuild');")
        .context("failed to prime posts_fts before content_html conversion")?;

    for (fullname, html) in rows {
        let markdown = html2md::parse_html(&html).trim().to_string();
        tx.execute(
            "UPDATE saved_posts SET content_markdown = ? WHERE reddit_fullname = ?",
            params![markdown, fullname],
        )
        .context("failed to migrate content_html to content_markdown")?;
    }

    Ok(())
}

/// Add `column` to `table` when it is absent. Both names are compile-time
/// constants from this module (never user input), so interpolating them is safe.
fn ensure_column(tx: &Transaction, table: &str, column: &str, column_type: &str) -> Result<()> {
    if column_exists(tx, table, column)? {
        return Ok(());
    }

    tx.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {column_type}"),
        [],
    )
    .with_context(|| format!("failed to add {table}.{column}"))?;

    Ok(())
}

/// Whether `table` already has a column named `column`.
fn column_exists(tx: &Transaction, table: &str, column: &str) -> Result<bool> {
    let mut stmt = tx.prepare(&format!("PRAGMA table_info({table})"))?;
    let exists = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == column);
    Ok(exists)
}

const BASELINE_SCHEMA_SQL: &str = r#"
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
"#;

// Each FTS5 table is external-content (`content=`/`content_rowid='rowid'`): it
// stores only the inverted index and reads document text from its content table
// by rowid, kept in sync by AFTER INSERT/DELETE/UPDATE triggers that mirror
// `old.rowid`/`new.rowid`. The aux `capture_fts`/`enrichment_fts` tables
// (RSS-36) mirror the `posts_fts` pattern exactly so multi-source search can
// UNION all three; see `search::search` and
// docs/explanation/fts-multi-source.md. NULL columns index as empty text, so
// posts without a capture or enrichment simply never appear in those indexes.
// The FTS column names must match real columns on the content table because the
// `'rebuild'` command re-reads them by name.
const FTS_SCHEMA_SQL: &str = r#"
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

CREATE VIRTUAL TABLE IF NOT EXISTS capture_fts USING fts5(
    title,
    description,
    site_name,
    content_markdown,
    content='outbound_captures',
    content_rowid='rowid',
    tokenize='porter unicode61'
);

CREATE TRIGGER IF NOT EXISTS outbound_captures_ai AFTER INSERT ON outbound_captures BEGIN
    INSERT INTO capture_fts(rowid, title, description, site_name, content_markdown)
    VALUES (new.rowid, new.title, new.description, new.site_name, new.content_markdown);
END;

CREATE TRIGGER IF NOT EXISTS outbound_captures_ad AFTER DELETE ON outbound_captures BEGIN
    INSERT INTO capture_fts(capture_fts, rowid, title, description, site_name, content_markdown)
    VALUES ('delete', old.rowid, old.title, old.description, old.site_name, old.content_markdown);
END;

CREATE TRIGGER IF NOT EXISTS outbound_captures_au AFTER UPDATE ON outbound_captures BEGIN
    INSERT INTO capture_fts(capture_fts, rowid, title, description, site_name, content_markdown)
    VALUES ('delete', old.rowid, old.title, old.description, old.site_name, old.content_markdown);
    INSERT INTO capture_fts(rowid, title, description, site_name, content_markdown)
    VALUES (new.rowid, new.title, new.description, new.site_name, new.content_markdown);
END;

CREATE VIRTUAL TABLE IF NOT EXISTS enrichment_fts USING fts5(
    classification,
    tags_json,
    summary,
    rationale,
    content='enrichment_runs',
    content_rowid='rowid',
    tokenize='porter unicode61'
);

CREATE TRIGGER IF NOT EXISTS enrichment_runs_ai AFTER INSERT ON enrichment_runs BEGIN
    INSERT INTO enrichment_fts(rowid, classification, tags_json, summary, rationale)
    VALUES (new.rowid, new.classification, new.tags_json, new.summary, new.rationale);
END;

CREATE TRIGGER IF NOT EXISTS enrichment_runs_ad AFTER DELETE ON enrichment_runs BEGIN
    INSERT INTO enrichment_fts(enrichment_fts, rowid, classification, tags_json, summary, rationale)
    VALUES ('delete', old.rowid, old.classification, old.tags_json, old.summary, old.rationale);
END;

CREATE TRIGGER IF NOT EXISTS enrichment_runs_au AFTER UPDATE ON enrichment_runs BEGIN
    INSERT INTO enrichment_fts(enrichment_fts, rowid, classification, tags_json, summary, rationale)
    VALUES ('delete', old.rowid, old.classification, old.tags_json, old.summary, old.rationale);
    INSERT INTO enrichment_fts(rowid, classification, tags_json, summary, rationale)
    VALUES (new.rowid, new.classification, new.tags_json, new.summary, new.rationale);
END;
"#;
