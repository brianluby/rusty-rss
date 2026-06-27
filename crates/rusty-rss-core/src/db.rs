use crate::models::{
    Classification, EnrichmentOutput, EnrichmentRecord, ExportRecord, OutboundCapture, PostTag,
    RecommendedAction, SavedPost, TriageItem,
};
use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{BTreeMap, HashSet};
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
    rebuild_empty_fts_index(&conn)?;

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

fn rebuild_empty_fts_index(conn: &Connection) -> Result<()> {
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
    if indexed_count == 0 {
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

#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    pub subreddit: Option<String>,
    pub author: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchHit {
    pub reddit_fullname: String,
    pub title: String,
    pub author: Option<String>,
    pub subreddit: Option<String>,
    pub permalink: String,
    pub outbound_url: Option<String>,
    pub snippet: String,
    pub rank: f64,
    pub last_seen_at: String,
}

pub fn search_posts(
    conn: &Connection,
    query: &str,
    filters: &SearchFilters,
    limit: usize,
) -> Result<Vec<SearchHit>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let fts_query = normalize_fts_query(query)?;
    let mut stmt = conn
        .prepare(
            "SELECT p.reddit_fullname,
                    p.title,
                    p.author,
                    p.subreddit,
                    p.permalink,
                    p.outbound_url,
                    snippet(posts_fts, -1, '<mark>', '</mark>', '...', 32) AS snippet,
                    bm25(posts_fts, 10.0, 1.0) AS rank,
                    p.last_seen_at
             FROM posts_fts
             JOIN saved_posts p ON p.rowid = posts_fts.rowid
             WHERE posts_fts MATCH ?
               AND (? IS NULL OR p.subreddit = ? COLLATE NOCASE)
               AND (? IS NULL OR p.author = ? COLLATE NOCASE)
             ORDER BY rank ASC, p.last_seen_at DESC
             LIMIT ?",
        )
        .context("failed to prepare search query")?;

    let rows = stmt
        .query_map(
            params![
                fts_query,
                filters.subreddit.as_deref(),
                filters.subreddit.as_deref(),
                filters.author.as_deref(),
                filters.author.as_deref(),
                limit,
            ],
            |row| {
                Ok(SearchHit {
                    reddit_fullname: row.get(0)?,
                    title: row.get(1)?,
                    author: row.get(2)?,
                    subreddit: row.get(3)?,
                    permalink: row.get(4)?,
                    outbound_url: row.get(5)?,
                    snippet: row.get(6)?,
                    rank: row.get(7)?,
                    last_seen_at: row.get(8)?,
                })
            },
        )
        // The query syntax was already validated in normalize_fts_query, so an
        // error here is a database failure, not bad user input.
        .context("failed to execute search query")?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect search results")
}

fn normalize_fts_query(query: &str) -> Result<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;

    for ch in query.chars() {
        if ch == '"' {
            if in_quote {
                push_quoted_search_part(&mut parts, &current);
                current.clear();
                in_quote = false;
            } else {
                push_unquoted_search_parts(&mut parts, &current);
                current.clear();
                in_quote = true;
            }
        } else if in_quote || !ch.is_whitespace() {
            current.push(ch);
        } else {
            push_unquoted_search_parts(&mut parts, &current);
            current.clear();
        }
    }

    if in_quote {
        return Err(anyhow!("invalid search query: unterminated quoted phrase"));
    }
    push_unquoted_search_parts(&mut parts, &current);

    if parts.is_empty() {
        return Err(anyhow!(
            "invalid search query: query must contain searchable text"
        ));
    }

    Ok(parts.join(" AND "))
}

fn push_unquoted_search_parts(parts: &mut Vec<String>, value: &str) {
    for term in value.split_whitespace() {
        push_quoted_search_part(parts, term);
    }
}

fn push_quoted_search_part(parts: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if !value.chars().any(|ch| ch.is_alphanumeric()) {
        return;
    }

    parts.push(format!("\"{}\"", value.replace('"', "\"\"")));
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

#[derive(Debug, Clone, Default)]
pub struct ExportFilters {
    pub classification: Option<Classification>,
    pub recommended_action: Option<RecommendedAction>,
    pub min_joy_value: Option<f32>,
    pub min_work_value: Option<f32>,
}

pub fn list_export_records(
    conn: &Connection,
    filters: &ExportFilters,
    limit: usize,
    offset: usize,
) -> Result<Vec<ExportRecord>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let classification = filters.classification.map(|value| value.as_str());
    let action = filters.recommended_action.map(|value| value.as_str());
    let mut stmt = conn.prepare(
        "SELECT p.reddit_fullname, p.reddit_id, p.title, p.author, p.subreddit,
                p.permalink, p.outbound_url, p.content_markdown, p.thumbnail_url,
                p.published_at, p.updated_at, p.first_seen_at, p.last_seen_at, p.source,
                e.id, e.reddit_fullname, e.provider, e.model, e.prompt_version, e.status,
                e.raw_response, e.classification, e.tags_json, e.summary, e.joy_value,
                e.work_value, e.recommended_action, e.rationale, e.confidence, e.error,
                e.created_at,
                c.reddit_fullname, c.original_url, c.final_url, c.canonical_url, c.title,
                c.description, c.site_name, c.preview_image_url, c.content_markdown,
                c.content_hash, c.status, c.http_status, c.error, c.fetched_at,
                c.attempt_count
         FROM saved_posts p
         LEFT JOIN enrichment_runs e ON e.id = (
             SELECT id FROM enrichment_runs latest
             WHERE latest.reddit_fullname = p.reddit_fullname
             ORDER BY latest.id DESC
             LIMIT 1
         )
         LEFT JOIN outbound_captures c ON c.reddit_fullname = p.reddit_fullname
         WHERE (? IS NULL OR e.classification = ?)
           AND (? IS NULL OR e.recommended_action = ?)
           AND (? IS NULL OR e.joy_value >= ?)
           AND (? IS NULL OR e.work_value >= ?)
         ORDER BY p.last_seen_at DESC
         LIMIT ? OFFSET ?",
    )?;

    let rows = stmt
        .query_map(
            params![
                classification,
                classification,
                action,
                action,
                filters.min_joy_value,
                filters.min_joy_value,
                filters.min_work_value,
                filters.min_work_value,
                limit,
                offset,
            ],
            export_record_from_row,
        )
        .context("failed to query export records")?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect export records")
}

#[derive(Debug, Clone)]
pub struct OutboundCaptureCandidate {
    pub reddit_fullname: String,
    pub outbound_url: String,
}

#[derive(Debug, Clone)]
pub struct OutboundCaptureUpsert {
    pub reddit_fullname: String,
    pub original_url: String,
    pub final_url: Option<String>,
    pub canonical_url: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub site_name: Option<String>,
    pub preview_image_url: Option<String>,
    pub content_markdown: Option<String>,
    pub content_hash: Option<String>,
    pub status: String,
    pub http_status: Option<i64>,
    pub error: Option<String>,
}

pub fn list_outbound_capture_candidates(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<OutboundCaptureCandidate>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let mut stmt = conn.prepare(
        "SELECT p.reddit_fullname, p.outbound_url
         FROM saved_posts p
         LEFT JOIN outbound_captures c ON c.reddit_fullname = p.reddit_fullname
         WHERE p.outbound_url IS NOT NULL
           AND (
               c.reddit_fullname IS NULL
               OR c.status != 'success'
               OR c.original_url != p.outbound_url
           )
         ORDER BY p.last_seen_at DESC
         LIMIT ?",
    )?;

    let rows = stmt
        .query_map(params![limit], |row| {
            Ok(OutboundCaptureCandidate {
                reddit_fullname: row.get(0)?,
                outbound_url: row.get(1)?,
            })
        })
        .context("failed to query outbound capture candidates")?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect outbound capture candidates")
}

pub fn upsert_outbound_capture(conn: &Connection, capture: &OutboundCaptureUpsert) -> Result<()> {
    conn.execute(
        r#"INSERT INTO outbound_captures (
            reddit_fullname, original_url, final_url, canonical_url, title, description,
            site_name, preview_image_url, content_markdown, content_hash, status,
            http_status, error, fetched_at, attempt_count
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1)
        ON CONFLICT(reddit_fullname) DO UPDATE SET
            original_url = excluded.original_url,
            final_url = excluded.final_url,
            canonical_url = excluded.canonical_url,
            title = excluded.title,
            description = excluded.description,
            site_name = excluded.site_name,
            preview_image_url = excluded.preview_image_url,
            content_markdown = excluded.content_markdown,
            content_hash = excluded.content_hash,
            status = excluded.status,
            http_status = excluded.http_status,
            error = excluded.error,
            fetched_at = excluded.fetched_at,
            attempt_count = outbound_captures.attempt_count + 1"#,
        params![
            capture.reddit_fullname,
            capture.original_url,
            capture.final_url,
            capture.canonical_url,
            capture.title,
            capture.description,
            capture.site_name,
            capture.preview_image_url,
            capture.content_markdown,
            capture.content_hash,
            capture.status,
            capture.http_status,
            capture.error,
            Utc::now().to_rfc3339(),
        ],
    )
    .context("failed to upsert outbound capture")?;

    Ok(())
}

pub fn latest_outbound_capture(
    conn: &Connection,
    reddit_fullname: &str,
) -> Result<Option<OutboundCapture>> {
    conn.query_row(
        "SELECT reddit_fullname, original_url, final_url, canonical_url, title,
                description, site_name, preview_image_url, content_markdown,
                content_hash, status, http_status, error, fetched_at, attempt_count
         FROM outbound_captures
         WHERE reddit_fullname = ?",
        params![reddit_fullname],
        outbound_capture_from_row,
    )
    .optional()
    .context("failed to query outbound capture")
}

pub fn list_enrichment_candidates(conn: &Connection, limit: usize) -> Result<Vec<SavedPost>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let mut stmt = conn.prepare(
        "SELECT reddit_fullname, reddit_id, title, author, subreddit, permalink,
                outbound_url, content_markdown, thumbnail_url, published_at, updated_at,
                first_seen_at, last_seen_at, source
         FROM saved_posts p
         WHERE NOT EXISTS (
             SELECT 1 FROM enrichment_runs e
             WHERE e.reddit_fullname = p.reddit_fullname AND e.status = 'success'
         )
         ORDER BY last_seen_at DESC
         LIMIT ?",
    )?;

    let rows = stmt
        .query_map(params![limit], saved_post_from_row)
        .context("failed to query enrichment candidates")?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect enrichment candidates")
}

pub fn record_enrichment_success(
    conn: &Connection,
    reddit_fullname: &str,
    provider: &str,
    model: &str,
    prompt_version: &str,
    raw_response: &str,
    output: &EnrichmentOutput,
) -> Result<i64> {
    output
        .validate()
        .map_err(|err| anyhow::anyhow!("invalid enrichment output: {err}"))?;

    let tags_json = serde_json::to_string(&output.tags).context("failed to serialize tags")?;
    conn.execute(
        r#"INSERT INTO enrichment_runs (
            reddit_fullname, provider, model, prompt_version, status, raw_response,
            classification, tags_json, summary, joy_value, work_value,
            recommended_action, rationale, confidence, created_at
        ) VALUES (?, ?, ?, ?, 'success', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        params![
            reddit_fullname,
            provider,
            model,
            prompt_version,
            raw_response,
            output.classification.as_str(),
            tags_json,
            output.summary,
            output.joy_value,
            output.work_value,
            output.recommended_action.as_str(),
            output.rationale,
            output.confidence,
            Utc::now().to_rfc3339(),
        ],
    )
    .context("failed to record enrichment success")?;

    Ok(conn.last_insert_rowid())
}

pub fn record_enrichment_failure(
    conn: &Connection,
    reddit_fullname: &str,
    provider: &str,
    model: &str,
    prompt_version: &str,
    error: &str,
) -> Result<i64> {
    conn.execute(
        r#"INSERT INTO enrichment_runs (
            reddit_fullname, provider, model, prompt_version, status, error, created_at
        ) VALUES (?, ?, ?, ?, 'error', ?, ?)"#,
        params![
            reddit_fullname,
            provider,
            model,
            prompt_version,
            error,
            Utc::now().to_rfc3339(),
        ],
    )
    .context("failed to record enrichment failure")?;

    Ok(conn.last_insert_rowid())
}

pub fn latest_enrichment(
    conn: &Connection,
    reddit_fullname: &str,
) -> Result<Option<EnrichmentRecord>> {
    conn.query_row(
        "SELECT id, reddit_fullname, provider, model, prompt_version, status,
                raw_response, classification, tags_json, summary, joy_value, work_value,
                recommended_action, rationale, confidence, error, created_at
         FROM enrichment_runs
         WHERE reddit_fullname = ?
         ORDER BY id DESC
         LIMIT 1",
        params![reddit_fullname],
        enrichment_record_from_row,
    )
    .optional()
    .context("failed to query latest enrichment")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriageView {
    All,
    Unprocessed,
    HighValue,
    ShouldTest,
    ShouldBuild,
    ReadingQueue,
    ReferenceOnly,
    Discard,
}

impl TriageView {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "all" => Some(Self::All),
            "unprocessed" => Some(Self::Unprocessed),
            "high-value" | "high_value" => Some(Self::HighValue),
            "should-test" | "should_test" => Some(Self::ShouldTest),
            "should-build" | "should_build" => Some(Self::ShouldBuild),
            "reading-queue" | "reading_queue" => Some(Self::ReadingQueue),
            "reference-only" | "reference_only" | "reference" => Some(Self::ReferenceOnly),
            "discard" => Some(Self::Discard),
            _ => None,
        }
    }
}

pub fn list_triage_items(
    conn: &Connection,
    view: TriageView,
    limit: usize,
    offset: usize,
) -> Result<Vec<TriageItem>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let where_clause = match view {
        TriageView::All => "1 = 1",
        TriageView::Unprocessed => {
            "NOT EXISTS (
                SELECT 1 FROM enrichment_runs success
                WHERE success.reddit_fullname = p.reddit_fullname AND success.status = 'success'
            )"
        }
        TriageView::HighValue => {
            "e.status = 'success' AND (e.joy_value >= 0.7 OR e.work_value >= 0.7)"
        }
        TriageView::ShouldTest => "e.status = 'success' AND e.recommended_action = 'should_test'",
        TriageView::ShouldBuild => "e.status = 'success' AND e.recommended_action = 'should_build'",
        TriageView::ReadingQueue => {
            "e.status = 'success' AND e.recommended_action = 'reading_queue'"
        }
        TriageView::ReferenceOnly => {
            "e.status = 'success' AND e.recommended_action = 'reference_only'"
        }
        TriageView::Discard => "e.status = 'success' AND e.recommended_action = 'discard'",
    };
    let query = format!(
        "SELECT p.reddit_fullname, p.title, p.subreddit, p.author, p.permalink, p.outbound_url,
                e.id, e.reddit_fullname, e.provider, e.model, e.prompt_version, e.status,
                e.raw_response, e.classification, e.tags_json, e.summary, e.joy_value, e.work_value,
                e.recommended_action, e.rationale, e.confidence, e.error, e.created_at
         FROM saved_posts p
         LEFT JOIN enrichment_runs e ON e.id = (
             SELECT id FROM enrichment_runs latest
             WHERE latest.reddit_fullname = p.reddit_fullname
             ORDER BY latest.id DESC
             LIMIT 1
         )
         WHERE {where_clause}
         ORDER BY p.last_seen_at DESC
         LIMIT ? OFFSET ?"
    );
    let mut stmt = conn.prepare(&query)?;
    let rows = stmt
        .query_map(params![limit, offset], triage_item_from_row)
        .context("failed to query triage items")?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect triage items")
}

fn saved_post_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SavedPost> {
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
}

fn export_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExportRecord> {
    let latest_enrichment = if row.get::<_, Option<i64>>(14)?.is_some() {
        Some(enrichment_record_from_row_with_offset(row, 14)?)
    } else {
        None
    };
    let outbound_capture = if row.get::<_, Option<String>>(31)?.is_some() {
        Some(outbound_capture_from_row_with_offset(row, 31)?)
    } else {
        None
    };

    Ok(ExportRecord {
        schema_version: "rusty-rss.export.v1".to_string(),
        saved_post: saved_post_from_row(row)?,
        latest_enrichment,
        outbound_capture,
    })
}

fn outbound_capture_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<OutboundCapture> {
    outbound_capture_from_row_with_offset(row, 0)
}

fn outbound_capture_from_row_with_offset(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<OutboundCapture> {
    Ok(OutboundCapture {
        reddit_fullname: row.get(offset)?,
        original_url: row.get(offset + 1)?,
        final_url: row.get(offset + 2)?,
        canonical_url: row.get(offset + 3)?,
        title: row.get(offset + 4)?,
        description: row.get(offset + 5)?,
        site_name: row.get(offset + 6)?,
        preview_image_url: row.get(offset + 7)?,
        content_markdown: row.get(offset + 8)?,
        content_hash: row.get(offset + 9)?,
        status: row.get(offset + 10)?,
        http_status: row.get(offset + 11)?,
        error: row.get(offset + 12)?,
        fetched_at: row.get(offset + 13)?,
        attempt_count: row.get(offset + 14)?,
    })
}

fn enrichment_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EnrichmentRecord> {
    let status: String = row.get(5)?;
    let output = if status == "success" {
        Some(EnrichmentOutput {
            classification: row
                .get::<_, String>(7)?
                .parse()
                .unwrap_or(Classification::Other),
            tags: row
                .get::<_, Option<String>>(8)?
                .and_then(|tags| serde_json::from_str(&tags).ok())
                .unwrap_or_default(),
            summary: row.get(9)?,
            joy_value: row.get(10)?,
            work_value: row.get(11)?,
            recommended_action: row
                .get::<_, String>(12)?
                .parse()
                .unwrap_or(RecommendedAction::Other),
            rationale: row.get(13)?,
            confidence: row.get(14)?,
        })
    } else {
        None
    };

    Ok(EnrichmentRecord {
        id: row.get(0)?,
        reddit_fullname: row.get(1)?,
        provider: row.get(2)?,
        model: row.get(3)?,
        prompt_version: row.get(4)?,
        status,
        raw_response: row.get(6)?,
        output,
        error: row.get(15)?,
        created_at: row.get(16)?,
    })
}

fn triage_item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TriageItem> {
    let enrichment = if row.get::<_, Option<i64>>(6)?.is_some() {
        Some(enrichment_record_from_row_with_offset(row, 6)?)
    } else {
        None
    };

    Ok(TriageItem {
        reddit_fullname: row.get(0)?,
        title: row.get(1)?,
        subreddit: row.get(2)?,
        author: row.get(3)?,
        permalink: row.get(4)?,
        outbound_url: row.get(5)?,
        enrichment,
    })
}

fn enrichment_record_from_row_with_offset(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<EnrichmentRecord> {
    let status: String = row.get(offset + 5)?;
    let output = if status == "success" {
        Some(EnrichmentOutput {
            classification: row
                .get::<_, String>(offset + 7)?
                .parse()
                .unwrap_or(Classification::Other),
            tags: row
                .get::<_, Option<String>>(offset + 8)?
                .and_then(|tags| serde_json::from_str(&tags).ok())
                .unwrap_or_default(),
            summary: row.get(offset + 9)?,
            joy_value: row.get(offset + 10)?,
            work_value: row.get(offset + 11)?,
            recommended_action: row
                .get::<_, String>(offset + 12)?
                .parse()
                .unwrap_or(RecommendedAction::Other),
            rationale: row.get(offset + 13)?,
            confidence: row.get(offset + 14)?,
        })
    } else {
        None
    };

    Ok(EnrichmentRecord {
        id: row.get(offset)?,
        reddit_fullname: row.get(offset + 1)?,
        provider: row.get(offset + 2)?,
        model: row.get(offset + 3)?,
        prompt_version: row.get(offset + 4)?,
        status,
        raw_response: row.get(offset + 6)?,
        output,
        error: row.get(offset + 15)?,
        created_at: row.get(offset + 16)?,
    })
}

/// A post in scope for Gate 1 tagging: just the fields the evaluator needs.
#[derive(Debug, Clone)]
pub struct TaggablePost {
    pub rowid: i64,
    pub reddit_fullname: String,
    pub subreddit: Option<String>,
}

/// List posts to tag, newest first. `limit` is an optional debug cap; the
/// default (`None`) processes the whole archive, because re-tagging everything
/// on a rule change is the point of the `tag` command.
pub fn list_taggable_posts(conn: &Connection, limit: Option<usize>) -> Result<Vec<TaggablePost>> {
    let mut sql = String::from(
        "SELECT rowid, reddit_fullname, subreddit FROM saved_posts ORDER BY last_seen_at DESC",
    );
    if limit.is_some() {
        sql.push_str(" LIMIT ?");
    }
    let mut stmt = conn.prepare(&sql)?;
    let map_row = |row: &rusqlite::Row<'_>| {
        Ok(TaggablePost {
            rowid: row.get(0)?,
            reddit_fullname: row.get(1)?,
            subreddit: row.get(2)?,
        })
    };
    let rows = match limit {
        Some(limit) => stmt.query_map(params![limit], map_row),
        None => stmt.query_map([], map_row),
    }
    .context("failed to query taggable posts")?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect taggable posts")
}

/// Run one compiled FTS5 operand and return the set of matching `saved_posts`
/// rowids. Config-malformed expressions surface as a clear error.
pub fn fts_matching_rowids(conn: &Connection, fts_expr: &str) -> Result<HashSet<i64>> {
    let mut stmt = conn
        .prepare_cached("SELECT rowid FROM posts_fts WHERE posts_fts MATCH ?1")
        .context("failed to prepare FTS match query")?;
    let ids = stmt
        .query_map(params![fts_expr], |row| row.get::<_, i64>(0))
        .map_err(|err| anyhow!("invalid match expression `{fts_expr}`: {err}"))?
        .collect::<std::result::Result<HashSet<i64>, _>>()
        .map_err(|err| anyhow!("invalid match expression `{fts_expr}`: {err}"))?;
    Ok(ids)
}

/// Smoke-test a compiled FTS5 operand without scanning rows, so a malformed
/// rule fails the whole run at load time (fail-closed) rather than mid-sweep.
pub fn validate_fts_expr(conn: &Connection, fts_expr: &str) -> Result<()> {
    let mut stmt = conn
        .prepare_cached("SELECT 1 FROM posts_fts WHERE posts_fts MATCH ?1 AND rowid = -1")
        .context("failed to prepare FTS validation query")?;
    stmt.query_row(params![fts_expr], |_| Ok(()))
        .optional()
        .map_err(|err| anyhow!("invalid match expression `{fts_expr}`: {err}"))?;
    Ok(())
}

/// Replace tags for the processed scope authoritatively: within a transaction,
/// delete the rows the run is responsible for, then insert the freshly computed
/// rows. Posts that no longer match (or topics removed from the ruleset) lose
/// their stale tags.
///
/// The delete scope is exactly what this run re-evaluated:
/// - `topic_filter = None` (all topics): the run owns every topic, so an
///   unprocessed-post row for a now-removed topic must also be cleared.
/// - `topic_filter = Some(t)`: only topic `t` is touched; other topics' tags
///   are preserved.
/// - `full_archive = true`: every post was processed, so the delete is
///   unscoped by post. Otherwise (a `--limit` debug run) only the processed
///   posts' rows are deleted, preserving tags for unprocessed posts.
pub fn replace_post_tags(
    conn: &Connection,
    topic_filter: Option<&str>,
    full_archive: bool,
    processed_fullnames: &[&str],
    tags: &[PostTag],
) -> Result<usize> {
    let tx = conn
        .unchecked_transaction()
        .context("failed to begin post_tags transaction")?;
    {
        match (topic_filter, full_archive) {
            (None, true) => {
                tx.execute("DELETE FROM post_tags", [])
                    .context("failed to clear post_tags")?;
            }
            (None, false) => {
                let mut delete = tx
                    .prepare("DELETE FROM post_tags WHERE reddit_fullname = ?1")
                    .context("failed to prepare post_tags delete")?;
                for fullname in processed_fullnames {
                    delete
                        .execute(params![fullname])
                        .context("failed to delete stale post_tags rows")?;
                }
            }
            (Some(topic), true) => {
                tx.execute("DELETE FROM post_tags WHERE topic = ?1", params![topic])
                    .context("failed to delete stale post_tags rows")?;
            }
            (Some(topic), false) => {
                let mut delete = tx
                    .prepare("DELETE FROM post_tags WHERE topic = ?1 AND reddit_fullname = ?2")
                    .context("failed to prepare post_tags delete")?;
                for fullname in processed_fullnames {
                    delete
                        .execute(params![topic, fullname])
                        .context("failed to delete stale post_tags row")?;
                }
            }
        }

        let mut insert = tx
            .prepare(
                r#"INSERT INTO post_tags (
                    reddit_fullname, topic, score, threshold, passed,
                    matched_rules, signals, ruleset_version, tagged_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
            )
            .context("failed to prepare post_tags insert")?;
        for tag in tags {
            let matched_rules = serde_json::to_string(&tag.matched_rules)
                .context("failed to serialize matched_rules")?;
            let signals =
                serde_json::to_string(&tag.signals).context("failed to serialize signals")?;
            insert
                .execute(params![
                    tag.reddit_fullname,
                    tag.topic,
                    tag.score,
                    tag.threshold,
                    tag.passed as i64,
                    matched_rules,
                    signals,
                    tag.ruleset_version,
                    tag.tagged_at,
                ])
                .context("failed to insert post_tags row")?;
        }
    }
    tx.commit().context("failed to commit post_tags")?;
    Ok(tags.len())
}

/// List materialized tags, newest-scoring first, optionally one topic and/or
/// only passing rows. Powers `tag --json` and read queries.
pub fn list_post_tags(
    conn: &Connection,
    topic: Option<&str>,
    passed_only: bool,
    limit: usize,
    offset: usize,
) -> Result<Vec<PostTag>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT reddit_fullname, topic, score, threshold, passed,
                matched_rules, signals, ruleset_version, tagged_at
         FROM post_tags
         WHERE (?1 IS NULL OR topic = ?1)
           AND (?2 = 0 OR passed = 1)
         ORDER BY topic ASC, score DESC, reddit_fullname ASC
         LIMIT ?3 OFFSET ?4",
    )?;
    let rows = stmt
        .query_map(
            params![topic, passed_only as i64, limit, offset],
            post_tag_from_row,
        )
        .context("failed to query post_tags")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect post_tags")
}

/// All tags for a single post, ordered by topic. Useful for inspection/tests.
pub fn post_tags_for(conn: &Connection, reddit_fullname: &str) -> Result<Vec<PostTag>> {
    let mut stmt = conn.prepare(
        "SELECT reddit_fullname, topic, score, threshold, passed,
                matched_rules, signals, ruleset_version, tagged_at
         FROM post_tags
         WHERE reddit_fullname = ?1
         ORDER BY topic ASC",
    )?;
    let rows = stmt
        .query_map(params![reddit_fullname], post_tag_from_row)
        .context("failed to query post_tags for post")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect post_tags for post")
}

fn post_tag_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PostTag> {
    let matched_rules_raw: String = row.get(5)?;
    let signals_raw: Option<String> = row.get(6)?;
    // Propagate corruption rather than silently returning empty provenance.
    let matched_rules: Vec<String> = serde_json::from_str(&matched_rules_raw).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(err))
    })?;
    let signals: BTreeMap<String, f32> = signals_raw
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(err))
        })?
        .unwrap_or_default();
    Ok(PostTag {
        reddit_fullname: row.get(0)?,
        topic: row.get(1)?,
        score: row.get(2)?,
        threshold: row.get(3)?,
        passed: row.get::<_, i64>(4)? != 0,
        matched_rules,
        signals,
        ruleset_version: row.get(7)?,
        tagged_at: row.get(8)?,
    })
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

    fn test_output(action: RecommendedAction, summary: &str) -> EnrichmentOutput {
        EnrichmentOutput {
            classification: Classification::Reference,
            tags: vec!["rust".to_string()],
            summary: summary.to_string(),
            joy_value: 0.2,
            work_value: 0.8,
            recommended_action: action,
            rationale: "Useful later".to_string(),
            confidence: 0.9,
        }
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
    fn fts_triggers_keep_index_in_sync() {
        let conn = test_db();
        let mut post = test_post();
        post.title = "Alpha Title".to_string();
        post.content_markdown = Some("alpha body".to_string());
        upsert_post(&conn, &post).expect("post should insert");

        let hits = search_posts(&conn, "alpha", &SearchFilters::default(), 10)
            .expect("inserted post should search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].reddit_fullname, "t3_test123");

        post.title = "Beta Title".to_string();
        post.content_markdown = Some("beta body".to_string());
        upsert_post(&conn, &post).expect("post should update");

        let old_hits = search_posts(&conn, "alpha", &SearchFilters::default(), 10)
            .expect("old terms should search");
        assert!(old_hits.is_empty());
        let new_hits = search_posts(&conn, "beta", &SearchFilters::default(), 10)
            .expect("new terms should search");
        assert_eq!(new_hits.len(), 1);

        conn.execute(
            "DELETE FROM saved_posts WHERE reddit_fullname = ?",
            params![post.reddit_fullname],
        )
        .expect("post should delete");
        let deleted_hits = search_posts(&conn, "beta", &SearchFilters::default(), 10)
            .expect("deleted terms should search");
        assert!(deleted_hits.is_empty());
    }

    #[test]
    fn init_db_rebuilds_fts_for_preexisting_rows() {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "rusty_rss_fts_backfill_test_{}_{}.db",
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
    fn search_ranking_prefers_title_matches() {
        let conn = test_db();
        let mut title_match = test_post();
        title_match.reddit_fullname = "t3_title".to_string();
        title_match.reddit_id = "title".to_string();
        title_match.title = "Needle in title".to_string();
        title_match.content_markdown = Some("unrelated content".to_string());
        upsert_post(&conn, &title_match).expect("title match should insert");

        let mut body_match = test_post();
        body_match.reddit_fullname = "t3_body".to_string();
        body_match.reddit_id = "body".to_string();
        body_match.title = "Other post".to_string();
        body_match.content_markdown = Some("needle in body".to_string());
        upsert_post(&conn, &body_match).expect("body match should insert");

        let hits = search_posts(&conn, "needle", &SearchFilters::default(), 10)
            .expect("search should succeed");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].reddit_fullname, "t3_title");
        assert!(hits[0].snippet.contains("<mark>"));
    }

    #[test]
    fn search_filters_compose() {
        let conn = test_db();
        for (fullname, subreddit, author) in [
            ("t3_rust_alice", "rust", "alice"),
            ("t3_rust_bob", "rust", "bob"),
            ("t3_go_alice", "golang", "alice"),
        ] {
            let mut post = test_post();
            post.reddit_fullname = fullname.to_string();
            post.reddit_id = fullname.trim_start_matches("t3_").to_string();
            post.title = "Composed filter target".to_string();
            post.subreddit = Some(subreddit.to_string());
            post.author = Some(author.to_string());
            upsert_post(&conn, &post).expect("post should insert");
        }

        let hits = search_posts(
            &conn,
            "target",
            &SearchFilters {
                subreddit: Some("rust".to_string()),
                author: Some("alice".to_string()),
            },
            10,
        )
        .expect("filtered search should succeed");

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].reddit_fullname, "t3_rust_alice");
    }

    #[test]
    fn search_filters_are_case_insensitive() {
        let conn = test_db();
        let mut post = test_post();
        post.title = "Case target".to_string();
        post.subreddit = Some("Rust".to_string());
        post.author = Some("Alice".to_string());
        upsert_post(&conn, &post).expect("post should insert");

        let hits = search_posts(
            &conn,
            "target",
            &SearchFilters {
                subreddit: Some("rust".to_string()),
                author: Some("alice".to_string()),
            },
            10,
        )
        .expect("filtered search should succeed");

        assert_eq!(hits.len(), 1, "filters should match regardless of case");
    }

    #[test]
    fn export_records_include_latest_enrichment_and_capture() {
        let conn = test_db();
        let post = test_post();
        upsert_post(&conn, &post).expect("post should insert");
        record_enrichment_success(
            &conn,
            "t3_test123",
            "provider",
            "model",
            "prompt",
            "raw",
            &test_output(RecommendedAction::ShouldBuild, "export summary"),
        )
        .expect("enrichment should insert");
        upsert_outbound_capture(
            &conn,
            &OutboundCaptureUpsert {
                reddit_fullname: "t3_test123".to_string(),
                original_url: "https://example.com/original".to_string(),
                final_url: Some("https://example.com/final".to_string()),
                canonical_url: Some("https://example.com/canonical".to_string()),
                title: Some("Captured title".to_string()),
                description: Some("Captured description".to_string()),
                site_name: Some("Example".to_string()),
                preview_image_url: Some("https://example.com/image.png".to_string()),
                content_markdown: Some("Captured snapshot".to_string()),
                content_hash: Some("sha256:test".to_string()),
                status: "success".to_string(),
                http_status: Some(200),
                error: None,
            },
        )
        .expect("capture should insert");

        let records = list_export_records(
            &conn,
            &ExportFilters {
                recommended_action: Some(RecommendedAction::ShouldBuild),
                ..ExportFilters::default()
            },
            10,
            0,
        )
        .expect("export should query");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].schema_version, "rusty-rss.export.v1");
        assert_eq!(records[0].saved_post.reddit_fullname, "t3_test123");
        assert_eq!(
            records[0]
                .latest_enrichment
                .as_ref()
                .and_then(|record| record.output.as_ref())
                .map(|output| output.recommended_action),
            Some(RecommendedAction::ShouldBuild)
        );
        assert_eq!(
            records[0]
                .outbound_capture
                .as_ref()
                .and_then(|capture| capture.title.as_deref()),
            Some("Captured title")
        );
    }

    #[test]
    fn successful_outbound_capture_removes_candidate() {
        let conn = test_db();
        let mut post = test_post();
        post.outbound_url = Some("https://example.com/post".to_string());
        upsert_post(&conn, &post).expect("post should insert");

        let candidates = list_outbound_capture_candidates(&conn, 10).expect("candidates query");
        assert_eq!(candidates.len(), 1);

        upsert_outbound_capture(
            &conn,
            &OutboundCaptureUpsert {
                reddit_fullname: "t3_test123".to_string(),
                original_url: "https://example.com/post".to_string(),
                final_url: Some("https://example.com/post".to_string()),
                canonical_url: None,
                title: Some("Captured".to_string()),
                description: None,
                site_name: None,
                preview_image_url: None,
                content_markdown: None,
                content_hash: None,
                status: "success".to_string(),
                http_status: Some(200),
                error: None,
            },
        )
        .expect("capture should insert");

        let candidates = list_outbound_capture_candidates(&conn, 10).expect("candidates query");
        assert!(candidates.is_empty());
    }

    #[test]
    fn changed_outbound_url_reschedules_capture() {
        let conn = test_db();
        let mut post = test_post();
        post.outbound_url = Some("https://example.com/old".to_string());
        upsert_post(&conn, &post).expect("post should insert");
        upsert_outbound_capture(
            &conn,
            &OutboundCaptureUpsert {
                reddit_fullname: "t3_test123".to_string(),
                original_url: "https://example.com/old".to_string(),
                final_url: Some("https://example.com/old".to_string()),
                canonical_url: None,
                title: Some("Old".to_string()),
                description: None,
                site_name: None,
                preview_image_url: None,
                content_markdown: None,
                content_hash: None,
                status: "success".to_string(),
                http_status: Some(200),
                error: None,
            },
        )
        .expect("capture should insert");
        assert!(
            list_outbound_capture_candidates(&conn, 10)
                .expect("query")
                .is_empty(),
            "an unchanged URL with a success capture is not a candidate"
        );

        // The outbound URL changes, so the stale capture must be retried.
        post.outbound_url = Some("https://example.com/new".to_string());
        upsert_post(&conn, &post).expect("post should update");

        let candidates = list_outbound_capture_candidates(&conn, 10).expect("query");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].outbound_url, "https://example.com/new");
    }

    #[test]
    fn malformed_search_query_fails_cleanly() {
        let conn = test_db();
        let post = test_post();
        upsert_post(&conn, &post).expect("post should insert");

        let err = search_posts(&conn, "\"unterminated", &SearchFilters::default(), 10)
            .expect_err("malformed query should fail");
        assert!(err.to_string().contains("invalid search query"));
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

        let hits = search_posts(&conn, "Markdown", &SearchFilters::default(), 10)
            .expect("migrated markdown should search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].reddit_fullname, "t3_old");
    }

    #[test]
    fn enrichment_runs_keep_raw_output_and_latest_normalized_fields() {
        let conn = test_db();
        let post = test_post();
        upsert_post(&conn, &post).expect("post should insert");

        let first = record_enrichment_success(
            &conn,
            "t3_test123",
            "provider",
            "model-a",
            "prompt-v1",
            "raw one",
            &test_output(RecommendedAction::ReadingQueue, "first"),
        )
        .expect("first enrichment should insert");
        let second = record_enrichment_success(
            &conn,
            "t3_test123",
            "provider",
            "model-b",
            "prompt-v1",
            "raw two",
            &test_output(RecommendedAction::ShouldBuild, "second"),
        )
        .expect("second enrichment should insert");

        assert_ne!(first, second);

        let latest = latest_enrichment(&conn, "t3_test123")
            .expect("latest should query")
            .expect("latest should exist");
        assert_eq!(latest.id, second);
        assert_eq!(latest.raw_response, Some("raw two".to_string()));
        assert_eq!(latest.model, "model-b");
        assert_eq!(
            latest
                .output
                .expect("normalized output should exist")
                .recommended_action,
            RecommendedAction::ShouldBuild
        );
    }

    #[test]
    fn triage_views_filter_latest_enrichment() {
        let conn = test_db();
        for fullname in ["t3_build", "t3_test", "t3_read", "t3_ref", "t3_discard"] {
            let mut post = test_post();
            post.reddit_fullname = fullname.to_string();
            post.reddit_id = fullname.trim_start_matches("t3_").to_string();
            upsert_post(&conn, &post).expect("post should insert");
        }

        for (fullname, action, work_value) in [
            ("t3_build", RecommendedAction::ShouldBuild, 0.9),
            ("t3_test", RecommendedAction::ShouldTest, 0.8),
            ("t3_read", RecommendedAction::ReadingQueue, 0.4),
            ("t3_ref", RecommendedAction::ReferenceOnly, 0.71),
            ("t3_discard", RecommendedAction::Discard, 0.1),
        ] {
            let mut output = test_output(action, fullname);
            output.work_value = work_value;
            record_enrichment_success(
                &conn, fullname, "provider", "model", "prompt", "raw", &output,
            )
            .expect("enrichment should insert");
        }

        let build = list_triage_items(&conn, TriageView::ShouldBuild, 10, 0)
            .expect("build view should query");
        assert_eq!(build[0].reddit_fullname, "t3_build");

        let high_value = list_triage_items(&conn, TriageView::HighValue, 10, 0)
            .expect("high value view should query");
        assert_eq!(high_value.len(), 3);

        let discard = list_triage_items(&conn, TriageView::Discard, 10, 0)
            .expect("discard view should query");
        assert_eq!(discard[0].reddit_fullname, "t3_discard");
    }

    #[test]
    fn zero_limits_return_no_enrichment_or_triage_rows() {
        let conn = test_db();
        let post = test_post();
        upsert_post(&conn, &post).expect("post should insert");

        let candidates = list_enrichment_candidates(&conn, 0).expect("candidates should query");
        assert!(candidates.is_empty());

        let items = list_triage_items(&conn, TriageView::All, 0, 0).expect("triage should query");
        assert!(items.is_empty());
    }

    #[test]
    fn record_enrichment_success_rejects_invalid_output() {
        let conn = test_db();
        let post = test_post();
        upsert_post(&conn, &post).expect("post should insert");
        let mut output = test_output(RecommendedAction::ReferenceOnly, "invalid");
        output.confidence = 2.0;

        let err = record_enrichment_success(
            &conn,
            "t3_test123",
            "provider",
            "model",
            "prompt",
            "raw",
            &output,
        )
        .expect_err("invalid output should not persist");

        assert!(err.to_string().contains("invalid enrichment output"));
        assert!(
            latest_enrichment(&conn, "t3_test123")
                .expect("latest should query")
                .is_none()
        );
    }
}
